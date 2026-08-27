//! What do the caller-owned-buffer paths actually allocate?
//!
//! `Encoder::encode_into_slice`, `Decoder::decode_into_slice` and the streaming
//! drain exist so a caller with an arena, an FFI buffer, or a fixed memory
//! budget can use this crate without handing it an allocator for the output.
//! Two drafts of the encode documentation were written from reading the code
//! and both were wrong, which is why this file measures instead. The first
//! claimed a warm encode allocated nothing; it made 19 allocations per frame.
//! The second claimed the count was independent of the input; it is not,
//! because the per-block work allocates and larger inputs have more blocks
//! (4 allocations at 32 KiB, 146 at 4 MiB).
//!
//! What survives measurement is the claim the APIs actually rest on: no
//! allocation is ever made that is sized like the output. The destination is
//! the caller's from beginning to end, and neither direction stages its data
//! through a buffer this crate owns. That is what the tests below pin. Decode
//! turns out to be the stronger of the two — warm, it allocates nothing at all.
//!
//! This is its own integration test rather than a case in `codec.rs` because
//! `#[global_allocator]` applies to a whole test binary, and counting the
//! allocations of 118 unrelated tests would be pointless.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::io::Write;

use zstandard::{CompressionLevel, Encoder, EncoderOptions, compress_bound, decode_all};

thread_local! {
    /// Const-initialized so reading them never allocates, which would
    /// otherwise recurse straight back into the allocator below.
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
    static LARGEST: Cell<usize> = const { Cell::new(0) };
}

/// Counts allocations, and their high-water size, on the calling thread only.
///
/// A global counter would fold in whatever the harness thread does while the
/// measurement is open, which is flakiness with nothing to do with the
/// encoder. Per-thread accounting makes the measurement exact.
struct CountingAllocator;

fn record(size: usize) {
    ALLOCATIONS.with(|count| count.set(count.get() + 1));
    LARGEST.with(|largest| largest.set(largest.get().max(size)));
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record(layout.size());
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Growth counts, and counts at its new size: a scratch buffer that
        // reallocated up to output size is exactly what this is looking for.
        record(new_size);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

/// Incompressible bytes.
///
/// The drain measurements need these: a compressible corpus produces a few
/// kilobytes per block, small enough that the per-block regrowth they are
/// looking for would slip under any ceiling worth setting. Noise makes every
/// block fall back to raw, so a buffer handed away is a ~128 KiB buffer regrown.
fn build_noise(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size + 8);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    while out.len() < size {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(size);
    out
}

fn build_pattern(size: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(size);
    let mut counter = 0u64;
    while out.len() < size {
        out.extend_from_slice(
            format!("record {counter} field=value other={}\n", counter % 97).as_bytes(),
        );
        counter += 1;
    }
    out.truncate(size);
    out
}

/// `(allocations, largest allocation, frame length)` for one warm encode.
fn measure(input: &[u8], options: EncoderOptions) -> (usize, usize, usize) {
    let mut encoder = Encoder::new();
    let mut buffer = vec![0u8; compress_bound(input.len(), options)];
    // Warm: the first call grows the encoder's scratch to what this shape
    // needs. Everything after it is steady state.
    encoder
        .encode_into_slice(input, &mut buffer, options)
        .unwrap();

    let before = ALLOCATIONS.with(Cell::get);
    LARGEST.with(|largest| largest.set(0));
    let written = encoder
        .encode_into_slice(input, &mut buffer, options)
        .unwrap();
    let count = ALLOCATIONS.with(Cell::get) - before;
    let largest = LARGEST.with(Cell::get);

    assert_eq!(decode_all(&buffer[..written]).unwrap(), input);
    (count, largest, written)
}

#[test]
fn a_warm_slice_encode_never_allocates_anything_output_sized() {
    // The claim the API rests on. If the encoder ever staged the frame through
    // a buffer of its own, that buffer would be on the order of the output and
    // would show up here immediately — which is the failure this rejects, and
    // it rejects it without depending on an allocation *count* that legitimately
    // varies with block count and data.
    //
    // Measured across 4 KiB to 4 MiB: the largest single allocation during a
    // warm encode is 28 bytes. The ceiling is 1 KiB, three orders of magnitude
    // below the smallest output here and far below any plausible staging
    // buffer, so a regression cannot slip under it.
    const CEILING: usize = 1024;

    let options = EncoderOptions {
        checksum: true,
        compression_level: CompressionLevel::try_new(6).unwrap(),
        ..Default::default()
    };

    for size in [4 * 1024usize, 128 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        let input = build_pattern(size);
        let (count, largest, written) = measure(&input, options);
        assert!(
            largest <= CEILING,
            "a {size}-byte input producing a {written}-byte frame made a \
             {largest}-byte allocation ({count} in total). Nothing on this path \
             should allocate anything resembling the output."
        );
    }
}

#[test]
fn slice_encode_allocations_scale_with_blocks_not_with_bytes() {
    // Documents the shape of what remains rather than pretending it is zero.
    // The per-frame allocations are the `Vec`s the sequence table choices carry
    // and the frame header's content-size and dictionary-id fields, so they
    // track the number of blocks, not the byte count. 32x the input costs
    // roughly an order of magnitude more allocations, not 32x.
    //
    // If this ratio blows out, something started allocating per byte, per
    // sequence, or per literal, and that is worth knowing even though it is not
    // a correctness failure.
    let options = EncoderOptions {
        compression_level: CompressionLevel::try_new(6).unwrap(),
        ..Default::default()
    };

    let (small, _, _) = measure(&build_pattern(128 * 1024), options);
    let (large, _, _) = measure(&build_pattern(4 * 1024 * 1024), options);

    assert!(
        large <= small * 64,
        "a 32x larger input made {large} allocations against {small}, which is \
         steeper than per-block growth explains"
    );
}

#[test]
fn the_allocation_counter_can_see_an_allocation() {
    // Without this, a counter that was silently broken — wrong thread, never
    // incremented, optimized away — would make everything above pass by
    // measuring nothing at all.
    let before = ALLOCATIONS.with(Cell::get);
    let witness: Vec<u8> = Vec::with_capacity(4096);
    let after = ALLOCATIONS.with(Cell::get);
    assert!(
        after > before,
        "the counting allocator recorded nothing for a {}-byte allocation, so \
         the measurements above prove nothing",
        witness.capacity()
    );
}

/// A sink that counts and discards.
///
/// Writing into a `Vec` would grow an output-sized buffer of its own and swamp
/// the measurement with the destination's allocations rather than the codec's.
struct NullSink(usize);

impl Write for NullSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Options that let the streaming encoder reach steady state quickly.
///
/// Its frame buffer grows to the window, so with a level's default window the
/// warm-up would still be doubling megabytes when the measurement opened and
/// the numbers below would be about that rather than about the drain. A
/// 256 KiB window is fully grown a few blocks in.
fn bounded_window_options() -> EncoderOptions {
    EncoderOptions {
        compression_level: CompressionLevel::try_new(3).unwrap(),
        parameters: zstandard::ParameterOverrides {
            window_log: Some(18),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// Both drain measurements come out at zero allocations, so neither can use
/// "the counter moved" as its guard against measuring nothing. They assert on
/// bytes moved instead, and `the_allocation_counter_can_see_an_allocation` is
/// what says the counter itself works.
const DRAINED_ENOUGH: usize = 3 * 1024 * 1024;

#[test]
fn a_warm_io_writer_does_not_allocate_a_buffer_per_block() {
    // `drain` used to `take_output()`, handing the encoder's output buffer to
    // the inner writer and leaving the encoder to grow a fresh one for the next
    // block: one block-sized allocation per block, on the crate's most-used
    // adapter. Borrowing the buffer and consuming from it leaves nothing here.
    //
    // The ceiling is a quarter of a block. Measured, the answer is 0 — a warm
    // writer over incompressible input allocates nothing at all — but a ceiling
    // rather than an equality leaves room for an unrelated small allocation to
    // appear without failing a test that is not about it. Reverting the drain
    // reports 262496 bytes here.
    const CEILING: usize = (128 * 1024) / 4;

    let warmup = build_noise(3 * 1024 * 1024);
    let input = build_noise(4 * 1024 * 1024);

    let mut writer =
        zstandard::io::Writer::with_options(NullSink(0), bounded_window_options()).unwrap();
    writer.write_all(&warmup).unwrap();

    LARGEST.with(|largest| largest.set(0));
    let before = ALLOCATIONS.with(Cell::get);
    writer.write_all(&input).unwrap();
    let total = ALLOCATIONS.with(Cell::get) - before;
    let largest = LARGEST.with(Cell::get);
    let sink = writer.finish().unwrap();

    assert!(
        sink.0 > DRAINED_ENOUGH,
        "only {} bytes reached the sink, so the measurement watched almost \
         nothing happen",
        sink.0
    );
    assert!(
        largest <= CEILING,
        "writing {} bytes through a warm io::Writer made a {largest}-byte \
         allocation ({total} in total). Nothing on the drain path should \
         allocate anything on the order of a block.",
        input.len()
    );
}

#[test]
fn a_warm_streaming_read_drain_does_not_allocate_a_buffer_per_block() {
    // The same claim for the API the adapter is built on: a pump that reads
    // into its own fixed buffer allocates that buffer once and never again.
    const CEILING: usize = 32 * 1024;

    let warmup = build_noise(3 * 1024 * 1024);
    let input = build_noise(4 * 1024 * 1024);

    let mut encoder = zstandard::StreamingEncoder::new(bounded_window_options()).unwrap();
    let mut window = vec![0u8; zstandard::StreamingEncoder::RECOMMENDED_OUTPUT_SIZE];
    let mut produced = 0usize;

    encoder.push(&warmup).unwrap();
    while encoder.read(&mut window) != 0 {}

    LARGEST.with(|largest| largest.set(0));
    let before = ALLOCATIONS.with(Cell::get);
    for chunk in input.chunks(zstandard::StreamingEncoder::RECOMMENDED_INPUT_SIZE) {
        encoder.push(chunk).unwrap();
        loop {
            let n = encoder.read(&mut window);
            if n == 0 {
                break;
            }
            produced += n;
        }
    }
    let total = ALLOCATIONS.with(Cell::get) - before;
    let largest = LARGEST.with(Cell::get);

    assert!(
        produced > DRAINED_ENOUGH,
        "the drain produced only {produced} bytes, so this measured almost \
         nothing"
    );
    assert!(
        largest <= CEILING,
        "draining {} bytes with `read` made a {largest}-byte allocation \
         ({total} in total)",
        input.len()
    );
}

/// `(allocations, largest allocation, bytes written)` for one warm slice decode.
fn measure_decode(compressed: &[u8], decoded_len: usize) -> (usize, usize, usize) {
    let mut decoder = zstandard::Decoder::new();
    let mut dst = vec![0u8; decoded_len];
    // Warm: the first call grows the decoder's literals scratch to what this
    // frame needs. Everything after it is steady state.
    decoder.decode_into_slice(compressed, &mut dst).unwrap();

    let before = ALLOCATIONS.with(Cell::get);
    LARGEST.with(|largest| largest.set(0));
    let written = decoder.decode_into_slice(compressed, &mut dst).unwrap();
    let count = ALLOCATIONS.with(Cell::get) - before;
    let largest = LARGEST.with(Cell::get);

    assert_eq!(written, decoded_len);
    (count, largest, written)
}

#[test]
fn a_warm_slice_decode_never_allocates_anything_output_sized() {
    // The mirror of the encode claim, and the one `decode_into_slice` exists
    // for. If the decoder ever staged the output through a buffer of its own —
    // to give the match wildcopy the trailing slack a caller's exact-sized
    // slice does not have, say — that buffer would be on the order of the
    // output and would show up here at once.
    //
    // Measured across 4 KiB to 4 MiB, warm: 0 allocations. The ceiling is a
    // quarter of a block, far below any plausible staging buffer and far above
    // an incidental small one, so a regression cannot slip under it.
    const CEILING: usize = (128 * 1024) / 4;

    for size in [4 * 1024usize, 128 * 1024, 1024 * 1024, 4 * 1024 * 1024] {
        for (name, input) in [
            ("compressible", build_pattern(size)),
            ("incompressible", build_noise(size)),
        ] {
            let compressed = zstandard::encode_all_with_options(
                &input,
                EncoderOptions {
                    checksum: true,
                    ..Default::default()
                },
            )
            .unwrap();
            let (count, largest, _) = measure_decode(&compressed, input.len());
            assert!(
                largest <= CEILING,
                "{name} at {size} bytes made a {largest}-byte allocation \
                 ({count} in total) decoding into a caller's slice"
            );
        }
    }
}

#[test]
fn a_warm_slice_decode_allocates_nothing_at_all() {
    // Stronger than the ceiling above and worth stating separately, because it
    // is the sentence a caller with a fixed memory budget actually needs: a
    // reused `Decoder` writing into a buffer they own asks the allocator for
    // nothing. The ceiling test is the one that survives an unrelated small
    // allocation appearing; this one is the one that notices it.
    let input = build_pattern(1024 * 1024);
    let compressed = zstandard::encode_all_with_options(
        &input,
        EncoderOptions {
            checksum: true,
            ..Default::default()
        },
    )
    .unwrap();
    let (count, largest, written) = measure_decode(&compressed, input.len());
    assert_eq!(
        count, 0,
        "a warm slice decode of {written} bytes made {count} allocations, the \
         largest {largest} bytes"
    );
}
