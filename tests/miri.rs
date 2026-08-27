//! Undefined-behavior coverage for the unsafe paths, sized for an interpreter.
//!
//! The rest of the suite is sized for native speed: bodies of 128 KiB and up,
//! which Miri needs many minutes apiece to walk because it tracks the
//! provenance of every access. This file exists so the `unsafe` in
//! `src/entropy/`, `src/window/`, and `src/sequence.rs` gets checked for UB at
//! all, and it buys that by keeping every body small rather than by checking
//! less.
//!
//! Two tricks keep the inputs small without narrowing what they reach.
//! `block_size` is set well below the 128 KiB default, so a few kilobytes still
//! spans many blocks and exercises repcode carry-over, history retention, and
//! the block-boundary decisions that only appear on the second block onwards.
//! And `StreamingEncoder::flush` ends a block wherever the caller asks rather
//! than where the split heuristic would choose, which is the one way to reach
//! block sizes the encoder would never pick for itself — two of the five
//! defects found when the encoder was first fuzzed were reachable only that
//! way.
//!
//! Run with:
//!
//! ```sh
//! MIRIFLAGS="-Zmiri-disable-isolation" cargo +nightly miri test --test miri
//! ```
//!
//! It is an ordinary test target, so it also runs natively on every `cargo
//! test` and stays honest that way.

use zstandard::{
    CompressionLevel, DecoderOptions, EncoderOptions, FrameHeader, ParameterOverrides, Strategy,
    StreamingDecoder, StreamingEncoder, decode_all, decode_all_with_dict,
    encode_all_with_dict_and_options, encode_all_with_options, parse_frame_header,
};

/// Blocks small enough that a couple of kilobytes still crosses several
/// boundaries.
const SMALL_BLOCK: usize = 384;

/// Body length for the level sweeps: several `SMALL_BLOCK` blocks, enough
/// repetition to give every parser real matches to find, and no more.
///
/// Sizing this is the whole game. Miri's cost is superlinear in body length at
/// the btopt and btultra levels, where the parser runs a dynamic program over
/// every position: at 6 KiB the sweep below took 23 minutes, and at 2 KiB it
/// takes a small fraction of that while visiting exactly the same code. Bytes
/// buy compression quality, which this file does not check; what it checks is
/// reached on the first pass through each path.
const BODY: usize = 2 * 1024;

/// Body length for the streaming cases, which push one chunk at a time and so
/// pay per byte rather than per body.
const STREAMING_BODY: usize = 1024;

/// A body with matches worth finding and a period the finder cannot simply
/// ride.
///
/// A verbatim-repeating body is the tempting shape here and a vacuous one: a
/// match finder that locks onto a single repeat offset and never looks again
/// reproduces it perfectly, so the test passes while covering almost none of
/// the search. Phrases are chosen by a xorshift, and a stray byte is inserted
/// often enough to keep breaking the groove, so matches stay plentiful but
/// their offsets keep changing.
fn structured_body(len: usize) -> Vec<u8> {
    const PHRASES: [&[u8]; 5] = [
        b"the quick brown fox jumps ",
        b"over the lazy dog while ",
        b"packing my box with five ",
        b"dozen liquor jugs 0123456789 ",
        b"|-|-|-|-|-|-|-|-|-|-|-|-|-|-|-",
    ];
    let mut out = Vec::with_capacity(len + 32);
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    while out.len() < len {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        out.extend_from_slice(PHRASES[(state % PHRASES.len() as u64) as usize]);
        if state & 0x3 == 0 {
            out.push(b'a' + (state >> 40) as u8 % 26);
        }
    }
    out.truncate(len);
    out
}

/// Every level this crate accepts. Each parser family — fast, double-fast, the
/// lazy family, btlazy2, btopt, btultra — carries its own unchecked indexing,
/// so covering families rather than levels would leave whole match finders
/// unvisited.
fn every_level() -> impl Iterator<Item = CompressionLevel> {
    (1..=22).map(|level| CompressionLevel::try_new(level).expect("1..=22 is in range"))
}

fn small_block_options(level: CompressionLevel) -> EncoderOptions {
    EncoderOptions {
        block_size: SMALL_BLOCK,
        checksum: true,
        ..EncoderOptions::default()
    }
    .with_compression_level(level)
}

#[test]
fn every_level_round_trips_across_many_blocks() {
    let body = structured_body(BODY);
    for level in every_level() {
        let encoded = encode_all_with_options(&body, small_block_options(level))
            .unwrap_or_else(|err| panic!("encode failed at level {}: {err}", level.as_i32()));
        let decoded = decode_all(&encoded)
            .unwrap_or_else(|err| panic!("decode failed at level {}: {err}", level.as_i32()));
        assert_eq!(
            decoded,
            body,
            "round trip differed at level {}",
            level.as_i32()
        );
    }
}

#[test]
fn every_parser_family_round_trips_with_a_raw_dictionary() {
    // The dictionary paths run the match finders against an external buffer
    // rather than the frame's own history, which is separate unchecked indexing
    // from the in-frame case.
    //
    // One level per family rather than all 22, unlike the sweep above. What
    // varies across levels within a family is the parameters — hash bits,
    // search depth — while the external-dictionary code they run is the same,
    // and the sweep above already covers every level's parameters against the
    // in-frame path. Keeping all 22 here cost more wall clock under Miri than
    // the rest of this file put together.
    let dictionary = structured_body(1024);
    let body = structured_body(BODY);
    for level in [1i32, 4, 5, 9, 12, 13, 16, 19, 22]
        .into_iter()
        .map(|level| CompressionLevel::try_new(level).expect("1..=22 is in range"))
    {
        let encoded = encode_all_with_dict_and_options(&body, &dictionary, {
            let mut options = small_block_options(level);
            options.write_dict_id = false;
            options
        })
        .unwrap_or_else(|err| panic!("dict encode failed at level {}: {err}", level.as_i32()));
        let decoded = decode_all_with_dict(&encoded, &dictionary)
            .unwrap_or_else(|err| panic!("dict decode failed at level {}: {err}", level.as_i32()));
        assert_eq!(
            decoded,
            body,
            "dictionary round trip differed at level {}",
            level.as_i32()
        );
    }
}

/// The dictionary paths again, but with a window narrow enough that the match
/// floors actually bind.
///
/// Every other test here leaves `window_log` alone, and no compression level
/// produces a window narrower than the content it is given — fitting the
/// parameters to the source sees to that. So the bounds deciding how far back a
/// match may reach were never *reached* under Miri: they were computed,
/// compared against, and always satisfied. Three defects lived in exactly that
/// gap, each an index bound expressed in the wrong coordinate space, and each
/// reachable only by combining a dictionary with a `window_log` override.
///
/// This is **not** a regression test for those three. They were wrong answers
/// rather than unsound memory accesses, and each is pinned in `tests/codec.rs`
/// by a test that fails on its own parent. Checked against the source before
/// the binary-tree fix, this one passes: without the block cap that shipped
/// alongside, a body this size is a single block and the frame declares a
/// window wide enough to cover any offset inside it.
///
/// What it adds is interpretation of that arithmetic while it binds. Folding a
/// prefix floor and a source floor into one index space and then indexing
/// tables with the result is unchecked in every parser that does it, and until
/// now no UB check had run with a floor anywhere but the bottom.
///
/// Strategies rather than levels, because which parser runs is the whole point
/// and a level reaches only one of them.
#[test]
fn every_parser_family_round_trips_with_a_dictionary_and_a_narrow_window() {
    const NARROW_WINDOW_LOG: u32 = 10;
    const STRATEGIES: [Strategy; 7] = [
        Strategy::Fast,
        Strategy::DoubleFast,
        Strategy::Greedy,
        Strategy::Lazy2,
        Strategy::BinaryTreeLazy2,
        Strategy::BinaryTreeOpt,
        Strategy::BinaryTreeUltra,
    ];

    let dictionary = structured_body(512);
    let body = structured_body(BODY);
    for strategy in STRATEGIES {
        let options = EncoderOptions {
            checksum: true,
            write_dict_id: false,
            parameters: ParameterOverrides {
                window_log: Some(NARROW_WINDOW_LOG),
                strategy: Some(strategy),
                ..ParameterOverrides::default()
            },
            // Left at the default so the encoder's own cap is what sizes the
            // blocks, which is the arrangement that ships.
            ..EncoderOptions::default()
        };

        let encoded =
            encode_all_with_dict_and_options(&body, &dictionary, options).unwrap_or_else(|err| {
                panic!("narrow-window dict encode failed for {strategy:?}: {err}")
            });

        // The premise, asserted rather than assumed. Everything this test is
        // for depends on the window coming out narrower than the body, so that
        // the floors sit above zero and the block cap splits the body. If a
        // future parameter change widened it back, every assertion below would
        // still pass while covering nothing this file does not already cover.
        let FrameHeader::Zstandard(header) =
            parse_frame_header(&encoded).expect("our own frame parses")
        else {
            panic!("expected a Zstandard frame");
        };
        assert_eq!(
            header.window_size,
            1u64 << NARROW_WINDOW_LOG,
            "{strategy:?}: the window stopped being the narrow one"
        );
        assert!(
            (header.window_size as usize) < body.len(),
            "{strategy:?}: the window has to be narrower than the body to bind"
        );

        let decoded = decode_all_with_dict(&encoded, &dictionary).unwrap_or_else(|err| {
            panic!("narrow-window dict decode failed for {strategy:?}: {err}")
        });
        assert_eq!(
            decoded, body,
            "narrow-window dictionary round trip differed for {strategy:?}"
        );

        // Streaming as well: its history is a buffer that compacts, so the
        // floors are resolved against a moving base rather than a fixed one,
        // and the double-fast defect of the three was reachable only here.
        let streaming_body = structured_body(STREAMING_BODY);
        let mut encoder =
            StreamingEncoder::with_dict(&dictionary, options).expect("encoder options are valid");
        for piece in streaming_body.chunks(61) {
            encoder.push(piece).expect("push accepts any chunk size");
        }
        encoder.finish().expect("finish completes the frame");
        let encoded = encoder.take_output();
        let decoded = decode_all_with_dict(&encoded, &dictionary).unwrap_or_else(|err| {
            panic!("narrow-window streaming dict decode failed for {strategy:?}: {err}")
        });
        assert_eq!(
            decoded, streaming_body,
            "narrow-window streaming dictionary round trip differed for {strategy:?}"
        );
    }
}

#[test]
fn streaming_encode_round_trips_under_awkward_chunking_and_flush() {
    let body = structured_body(STREAMING_BODY);
    // Chunk sizes that share no factor with the block size, so pushes and block
    // boundaries keep landing at different offsets. One byte at a time is the
    // worst case and the one worth keeping: it puts a block boundary at every
    // possible offset relative to the parser's state.
    for &chunk in &[1usize, 61, 1023] {
        // One level per parser family. Unlike the one-shot sweep above, what
        // differs between streaming levels is the family's retained state
        // across blocks, not the per-level parameters.
        for level in [1i32, 4, 9, 13, 16, 22] {
            let level = CompressionLevel::try_new(level).expect("1..=22 is in range");
            let mut encoder = StreamingEncoder::new(small_block_options(level))
                .expect("encoder options are valid");
            for (index, piece) in body.chunks(chunk).enumerate() {
                encoder.push(piece).expect("push accepts any chunk size");
                // Flushing partway ends blocks at sizes the split heuristic
                // would never choose.
                if index % 5 == 4 {
                    encoder.flush().expect("flush mid-stream is supported");
                }
            }
            encoder.finish().expect("finish completes the frame");
            let encoded = encoder.take_output();
            let decoded = decode_all(&encoded).unwrap_or_else(|err| {
                panic!(
                    "streaming decode failed at level {} chunk {chunk}: {err}",
                    level.as_i32()
                )
            });
            assert_eq!(
                decoded,
                body,
                "streaming round trip differed at level {} chunk {chunk}",
                level.as_i32()
            );
        }
    }
}

#[test]
fn streaming_decode_round_trips_under_awkward_chunking() {
    // Exercises the decoder's sliding window as a ring: with a body well past
    // one window and pushes that do not align to anything, matches are copied
    // across the wrap.
    let body = structured_body(STREAMING_BODY);
    let encoded = encode_all_with_options(
        &body,
        small_block_options(CompressionLevel::try_new(9).expect("1..=22 is in range")),
    )
    .expect("encode succeeds");
    for &chunk in &[1usize, 29, 257] {
        let mut decoder = StreamingDecoder::new(DecoderOptions::default());
        for piece in encoded.chunks(chunk) {
            decoder.push(piece).expect("push accepts any chunk size");
        }
        decoder.finish().expect("finish completes the frame");
        assert_eq!(
            decoder.take_output(),
            body,
            "streaming decode differed at chunk {chunk}"
        );
    }
}

/// Literals large enough, and compressible enough, to reach the double-symbol
/// Huffman decoder.
///
/// Every other body in this file is a couple of kilobytes, which is deliberate
/// and also puts every literals section below the size where the decoder
/// switches table shapes. The double-symbol path is the crate's newest
/// unchecked indexing, and its two-byte-at-a-time writes are bounded by an
/// argument about how far four stream cursors can drift apart, so leaving it
/// outside Miri's reach would leave the code most worth checking unchecked.
///
/// Shaping the body matters more than its length. A skewed distribution over a
/// handful of bytes looks literal-heavy and is not: with an alphabet that
/// small every three-byte run recurs constantly, the match finder consumes the
/// body, and 24 KiB arrives at the decoder as 911 bytes of literals — below
/// the threshold, and stored raw rather than Huffman-coded at that. Words
/// drawn from a wide alphabet keep matches short and leave most of the body in
/// the literals section, which is what puts it over the ~3.6 KiB floor.
#[test]
fn literal_heavy_blocks_decode_through_the_double_symbol_path() {
    let mut body: Vec<u8> = Vec::with_capacity(24 * 1024);
    let mut index = 0u32;
    while body.len() < 24 * 1024 {
        let noise = index.wrapping_mul(2_654_435_761);
        for byte in 0..2 + (noise >> 29) as usize {
            let value = (noise >> (byte % 4 * 8)) as u8;
            // Mostly letters, so the section compresses; the rest spread over
            // the whole byte range, so it does not compress so far that the
            // cost model prefers the narrow table.
            body.push(if value < 180 {
                b'a' + value % 26
            } else {
                value
            });
        }
        body.push(if noise.is_multiple_of(7) { b'\n' } else { b' ' });
        index = index.wrapping_add(1);
    }
    body.truncate(24 * 1024);

    // Default block size, not `SMALL_BLOCK`: the literals have to land in one
    // section to clear the threshold, and 384-byte blocks would guarantee they
    // do not.
    let encoded = encode_all_with_options(
        &body,
        EncoderOptions {
            compression_level: CompressionLevel::try_new(3).expect("1..=22 is in range"),
            ..Default::default()
        },
    )
    .expect("encode succeeds");

    assert_eq!(decode_all(&encoded).expect("decode succeeds"), body);
}

#[test]
fn corrupt_frames_are_rejected_without_undefined_behavior() {
    // The decoder's entropy readers are the only part of the crate that runs
    // unchecked indexing over bytes an attacker chose. What matters here is not
    // which error comes back but that reaching it touches nothing it should
    // not: the last defect found in `BitDStream` was a pointer computed past
    // the end of a bitstream shorter than one machine word, which no assertion
    // would have caught.
    let body = structured_body(2 * 1024);
    let encoded = encode_all_with_options(
        &body,
        small_block_options(CompressionLevel::try_new(6).expect("1..=22 is in range")),
    )
    .expect("encode succeeds");

    // Truncation at every length, which walks the readers off the end of each
    // section in turn.
    for length in 0..encoded.len().min(96) {
        let _ = decode_all(&encoded[..length]);
    }
    for length in (96..encoded.len()).step_by(37) {
        let _ = decode_all(&encoded[..length]);
    }

    // Single-byte corruption across the header and the first block, where the
    // entropy tables and their bitstreams live.
    for index in 0..encoded.len().min(160) {
        let mut damaged = encoded.clone();
        damaged[index] ^= 0xFF;
        let _ = decode_all(&damaged);
    }
}

#[test]
fn decoding_into_an_exactly_sized_slice_stays_inside_it() {
    // The one place the decoder writes into memory it did not size itself. Its
    // match copy overshoots by up to 31 bytes into slack a `Vec` always has and
    // a caller's exact-sized slice never does, and `execute_sequence_exact` is
    // what keeps that inside the buffer. `tests/codec.rs` guards the same claim
    // with sentinel bytes past the destination, which catches an overshoot that
    // changes them; this catches one that does not, because Miri knows where
    // the slice ends whatever the bytes there happen to be.
    //
    // Swept across match lengths so the overshoot lands at every distance past
    // the end, and across trailing lengths so the closing match sits at every
    // distance from it. Bodies stay small for the interpreter; what matters is
    // the geometry of the tail, not the size of the frame.
    let head = structured_body(256);
    for closing_match in [3usize, 17, 33, 49, 64] {
        for trailing in [0usize, 1, 7, 16, 31, 33] {
            let mut body = head.clone();
            body.extend_from_slice(&head[..closing_match]);
            body.extend_from_slice(&structured_body(64)[..trailing]);

            let encoded = encode_all_with_options(
                &body,
                small_block_options(CompressionLevel::try_new(6).expect("1..=22 is in range")),
            )
            .expect("encode succeeds");

            let mut dst = vec![0u8; body.len()];
            let written =
                zstandard::decode_into_slice(&encoded, &mut dst).unwrap_or_else(|error| {
                    panic!("closing match {closing_match}, trailing {trailing}: {error:?}")
                });
            assert_eq!(written, body.len());
            assert_eq!(dst, body);

            // One byte short must be refused rather than written past.
            let mut short = vec![0u8; body.len() - 1];
            assert!(zstandard::decode_into_slice(&encoded, &mut short).is_err());
        }
    }
}

#[test]
fn a_corrupt_frame_decoding_into_a_slice_stays_inside_it() {
    // The lengths a corrupt frame invents reach the byte-exact tail path too,
    // and there the destination has no slack left to absorb a bad one.
    let body = structured_body(1024);
    let encoded = encode_all_with_options(
        &body,
        small_block_options(CompressionLevel::try_new(6).expect("1..=22 is in range")),
    )
    .expect("encode succeeds");

    for length in (16..encoded.len()).step_by(29) {
        let mut dst = vec![0u8; body.len()];
        let _ = zstandard::decode_into_slice(&encoded[..length], &mut dst);
    }
    for index in (0..encoded.len()).step_by(13) {
        let mut damaged = encoded.clone();
        damaged[index] ^= 0xFF;
        let mut dst = vec![0u8; body.len()];
        let _ = zstandard::decode_into_slice(&damaged, &mut dst);
    }
}
