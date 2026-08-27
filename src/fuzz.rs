use crate::{
    BLOCK_SIZE_MAX,
    block::parse_block_header,
    decode::{
        DecoderOptions, decode_all_with_options, decode_all_with_prepared_dict_and_options,
        decode_into_slice_with_options,
    },
    dictionary::{DecoderDictionary, EncoderDictionary},
    encode::{
        CompressionLevel, Encoder, EncoderOptions, LiteralCompressionMode, ParameterBounds,
        ParameterOverrides, RowMatchFinderMode, Strategy, compression_parameters_for_options,
        encode_all_with_options,
    },
    error::Error,
    frame::{
        Format, FrameHeader, parse_frame_header, parse_frame_header_with_format,
        representable_window_size,
    },
    literals::{LiteralsState, decode_literals_section},
    sequence::{
        SequenceTablesState, TableTarget, decode_sequence_commands, parse_sequence_section,
    },
    streaming::{StreamingDecoder, StreamingEncoder},
    window::LdmMode,
};

const MAX_FUZZ_OUTPUT_SIZE: usize = 1 << 20;
const MAX_FUZZ_WINDOW_SIZE: u64 = 1 << 20;
const MAX_FUZZ_SEQUENCE_COUNT: usize = 4096;

pub fn frame_parse(data: &[u8]) {
    if let Ok(header) = parse_frame_header(data) {
        match header {
            FrameHeader::Zstandard(header) => {
                if let Some(blocks) = data.get(header.header_size..) {
                    let _ = parse_block_header(blocks);
                }
            }
            FrameHeader::Skippable(frame) => {
                let _ = data.get(..frame.header_size.saturating_add(frame.size as usize));
            }
        }
    }
}

pub fn literals_parse(data: &[u8]) {
    let split = split_point(data);
    let (first, second) = data.split_at(split);
    let mut state = LiteralsState::default();

    let _ = decode_literals_section(first, &mut state, BLOCK_SIZE_MAX);
    let _ = decode_literals_section(second, &mut state, BLOCK_SIZE_MAX);
}

pub fn sequence_parse(data: &[u8]) {
    let split = split_point(data);
    let (first, second) = data.split_at(split);
    let mut tables = SequenceTablesState::default();

    parse_sequence_chunk(first, &mut tables);
    parse_sequence_chunk(second, &mut tables);
}

pub fn full_decode(data: &[u8]) {
    let permissive = DecoderOptions {
        max_window_size: Some(MAX_FUZZ_WINDOW_SIZE),
        max_output_size: Some(MAX_FUZZ_OUTPUT_SIZE),
        verify_checksum: false,
        ..Default::default()
    };
    let relaxed = decode_all_with_options(data, permissive);

    // The strict mode is a restriction of the permissive one, and that gives a
    // real oracle rather than a crash check: anything it accepts, the default
    // must accept and decode identically. Only the frame count and what follows
    // it differ between the two, never the bytes of the first frame.
    let strict = decode_all_with_options(
        data,
        DecoderOptions {
            single_frame: true,
            ..permissive
        },
    );
    if let Ok(strict) = strict {
        assert_eq!(
            relaxed.as_deref(),
            Ok(strict.as_slice()),
            "single_frame accepted input the default decoder did not decode identically"
        );
    }
}

/// The same frames as [`full_decode`], but into a destination sized to the byte.
///
/// A fixed destination is where the decoder loses the trailing slack its match
/// wildcopy overshoots into, so this is the only fuzzed path that reaches
/// `execute_sequence_exact`, and the only one where a length the frame invented
/// turns into a write past the end of a buffer rather than into spare `Vec`
/// capacity.
///
/// The growable decode is the oracle: whatever it produces, this must produce
/// exactly, a destination one byte shorter must be refused rather than
/// truncated, and neither call may touch the guard bytes laid down past the
/// end — which is the part no assertion on the output itself can see.
pub fn slice_decode(data: &[u8]) {
    const GUARD: usize = 64;
    const GUARD_BYTE: u8 = 0xA5;

    let permissive = DecoderOptions {
        max_window_size: Some(MAX_FUZZ_WINDOW_SIZE),
        max_output_size: Some(MAX_FUZZ_OUTPUT_SIZE),
        verify_checksum: false,
        ..Default::default()
    };
    let Ok(expected) = decode_all_with_options(data, permissive) else {
        return;
    };

    let mut backing = vec![GUARD_BYTE; expected.len() + GUARD];
    let (dst, guard) = backing.split_at_mut(expected.len());
    let written = decode_into_slice_with_options(data, dst, permissive)
        .expect("a destination sized by the growable decode has to be enough");
    assert_eq!(
        written,
        expected.len(),
        "slice decode reported a short write"
    );
    assert_eq!(
        dst,
        expected.as_slice(),
        "slice decode diverged from decode_all"
    );
    assert!(
        guard.iter().all(|&byte| byte == GUARD_BYTE),
        "slice decode wrote past the end of an exactly-sized destination"
    );

    let Some(short) = expected.len().checked_sub(1) else {
        return;
    };
    let mut backing = vec![GUARD_BYTE; short + GUARD];
    let (dst, guard) = backing.split_at_mut(short);
    let result = decode_into_slice_with_options(data, dst, permissive);
    assert!(
        matches!(result, Err(Error::DstSizeTooSmall)),
        "a destination one byte short reported {result:?} instead of refusing"
    );
    assert!(
        guard.iter().all(|&byte| byte == GUARD_BYTE),
        "a refused slice decode wrote past the end of the destination"
    );
}

/// The same frames as [`full_decode`], but through the streaming decoder and fed
/// in fragments, so the state machine has to resume mid-header, mid-block, and
/// mid-checksum.
///
/// This path was unfuzzed until the sliding window was rewritten as a ring, and
/// a ring is exactly the shape where hostile offsets and lengths turn into
/// out-of-bounds indices. The first byte picks the chunk size so the fuzzer can
/// steer where the splits land rather than always seeing one shape.
pub fn streaming_decode(data: &[u8]) {
    let Some((&selector, frame)) = data.split_first() else {
        return;
    };

    // 1, 2, 4 ... 128 bytes, plus "all at once" so the unsplit path stays covered.
    let chunk = match selector % 9 {
        8 => frame.len().max(1),
        shift => 1usize << shift,
    };

    let mut decoder = StreamingDecoder::new(DecoderOptions {
        max_window_size: Some(MAX_FUZZ_WINDOW_SIZE),
        max_output_size: Some(MAX_FUZZ_OUTPUT_SIZE),
        verify_checksum: false,
        ..Default::default()
    });

    for piece in frame.chunks(chunk) {
        if decoder.push(piece).is_err() {
            return;
        }
        // Draining as we go is what makes any history droppable at all: the
        // decoder keeps its match history in the buffer the caller reads from,
        // and releases the front of it only once the caller has taken those
        // bytes. A target that never drained would exercise the growing case
        // and never the compacting one.
        let _ = decoder.take_output();
    }
    let _ = decoder.finish();
    let _ = decoder.take_output();
}

/// Largest body an encode target will compress, by parser family.
///
/// libFuzzer reports an input that exceeds `-timeout` as a hang rather than
/// fuzzing it, so each family gets a cap sized to its measured work per byte.
/// The caps also have to clear 256 KiB, the largest source size at which
/// `upstream_cparams_tier` still picks a reduced parameter tier: a family
/// capped below it only ever runs its smallest-tier parameters, so its window,
/// chain, and search logs never take the values a real encode of that level
/// uses. Every cap here is past that boundary.
///
/// Only the fast families get more than that, and only because the level-1
/// window is 512 KiB: how far back a match may reach is decided per block
/// against that window, and below it the decision never binds. Every other
/// level's window starts at 2 MiB, so no body these targets can afford would
/// reach its bound either way, and a cap past the tier boundary is all the
/// coverage there is to buy. Bodies cost time roughly linearly, so the levels
/// that stop at the boundary are the reason the targets run at all.
fn body_cap_for(level: CompressionLevel) -> usize {
    match level.as_i32() {
        // Negative levels, fast and double-fast. Three windows' worth, which is
        // cheap for these and the only way the window bound is reachable from a
        // fuzz input. The negative levels all run the fast parser with an
        // acceleration factor, so they are strictly cheaper than level 1 and
        // belong on this side of the split rather than in the catch-all.
        ..=4 => 3 << 19,
        // Everything else: far enough past 256 KiB to leave the reduced tiers,
        // and no further. btlazy2 is what sets the value. Its DUBT match finder
        // costs time superlinear in the source on near-periodic input, 61 ms at
        // this cap against 2 s at 1.5 MiB on a body that tiles a three-byte
        // seed. Upstream's `ZSTD_DUBT_findBestMatch` does the same thing to
        // within 15% on the same input, so this is a budget for a shared cost
        // and not a cap hiding a defect of ours.
        _ => 320 << 10,
    }
}

/// Body sizes an encode target chooses between, up to the level's cap.
///
/// These bracket the source sizes upstream switches compression-parameter tiers
/// on, which `upstream_cparams_tier` puts at 16 KiB, 128 KiB and 256 KiB, each
/// bound inclusive. A cap alone cannot reach them, because the tiled shape fills
/// exactly to the cap: one cap per family means one tier per family, whatever
/// the fuzzer does to the input. Picking the size separately is what puts the
/// boundary itself under test, and the values one byte either side of each are
/// there because an inclusive bound is the kind that gets written as exclusive.
const FUZZ_BODY_SIZES: [usize; 12] = [
    1 << 8,
    (16 << 10) - 1,
    16 << 10,
    (16 << 10) + 1,
    (128 << 10) - 1,
    128 << 10,
    (128 << 10) + 1,
    (256 << 10) - 1,
    256 << 10,
    (256 << 10) + 1,
    320 << 10,
    3 << 19,
];

/// Block sizes an encode target chooses between.
///
/// The values that do not divide evenly matter as much as the round ones: they
/// leave a short final block, and the last block is where the terminal flag,
/// the content checksum, and the final flush all meet. `BLOCK_SIZE_MAX` and one
/// below it are both here because the block-size bound is inclusive.
const FUZZ_BLOCK_SIZES: [usize; 8] = [
    1 << 10,
    (1 << 10) + 7,
    1 << 12,
    5000,
    1 << 14,
    1 << 16,
    BLOCK_SIZE_MAX - 1,
    BLOCK_SIZE_MAX,
];

/// Every level the crate supports, so one control byte reaches every parser
/// family and the negative "fast mode" configuration.
///
/// The 22 ordinary levels take the low 22 residues and the negative levels the
/// next 10, which keeps every parser family reachable while still spending
/// about a third of the byte on fast mode. Fast mode earns that share by being
/// a different encoder configuration rather than a faster one: it is the only
/// path that disables Huffman coding of the literals section, so it exercises
/// the raw-literals branch that no positive level reaches.
///
/// The negative side is sampled rather than swept. Acceleration only widens
/// the parser's stride, so `-1` and `-9` differ in how far they skip and not
/// in which code runs, and the deep floor is worth pinning explicitly because
/// it is where the clamp lives.
fn level_from(byte: u8) -> CompressionLevel {
    const NEGATIVE: [i32; 10] = [-1, -2, -3, -5, -9, -17, -33, -1000, -100_000, -131_072];
    let slot = byte % 32;
    let level = if slot < 22 {
        1 + i32::from(slot)
    } else {
        NEGATIVE[usize::from(slot - 22)]
    };
    CompressionLevel::try_new(level).expect("level_from stays inside the supported range")
}

/// Peel `N` configuration bytes off the front of a fuzz input, leaving the rest
/// as the body. Keeping the knobs at a fixed offset lets a mutation move one of
/// them without disturbing the bytes being compressed.
fn split_control<const N: usize>(data: &[u8]) -> Option<([u8; N], &[u8])> {
    let (control, body) = data.split_at_checked(N)?;
    Some((control.try_into().ok()?, body))
}

/// Grow `seed` to `target` bytes by tiling it, changing one byte per tile so the
/// result repeats *nearly* rather than exactly.
///
/// The drift is the whole point. A body that repeats verbatim is close to
/// useless for exercising a match finder: the first repeat offset that lands
/// becomes rep1, every later position hits it, and the parser never returns to
/// its hash table — so the candidate selection these targets exist to reach is
/// barely consulted. Rewriting one byte per tile breaks the match a few times
/// per period and sends the parser back to the table. The rewritten position
/// moves with the tile index so the disturbance does not become periodic itself.
fn tile_with_drift(seed: &[u8], target: usize) -> Vec<u8> {
    debug_assert!(!seed.is_empty());
    let mut out = Vec::with_capacity(target);
    let mut tile = 0u32;
    while out.len() < target {
        let start = out.len();
        let take = seed.len().min(target - start);
        out.extend_from_slice(&seed[..take]);
        // `| 1` keeps the XOR from being a no-op on every 256th tile.
        out[start + (tile as usize * 7) % take] ^= (tile as u8) | 1;
        tile = tile.wrapping_add(1);
    }
    out
}

/// How an encode target turns a fuzz seed into a body to compress.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyShape {
    /// The seed itself, truncated to the level's cap.
    Seed,
    /// The seed tiled up to `target`, which is already capped for the level.
    Tiled { target: usize },
}

/// Read the body shape out of the flags byte. The size index takes the bits
/// above the three flags, so a mutation that flips a flag does not also move the
/// size and leave which of the two mattered unclear.
fn body_shape_from(flags: u8, level: CompressionLevel) -> BodyShape {
    if flags & 4 == 0 {
        return BodyShape::Seed;
    }
    let index = usize::from(flags >> 3) % FUZZ_BODY_SIZES.len();
    BodyShape::Tiled {
        target: FUZZ_BODY_SIZES[index].min(body_cap_for(level)),
    }
}

/// Turn a fuzz input into a body to compress: either truncated to the level's
/// cap, or tiled up to the chosen size.
fn shape_body(seed: &[u8], level: CompressionLevel, shape: BodyShape) -> Vec<u8> {
    let cap = body_cap_for(level);
    let BodyShape::Tiled { target } = shape else {
        return seed[..seed.len().min(cap)].to_vec();
    };
    if seed.is_empty() || seed.len() >= target {
        return seed[..seed.len().min(target)].to_vec();
    }
    tile_with_drift(seed, target)
}

/// Build encoder options from seven control bytes. Returns the body shape
/// alongside, since it shares the flags byte.
///
/// `control[2]` carries the two header flags *and* the body shape, which is
/// why the frame-format switches live in `control[6]` rather than sharing it:
/// `body_shape_from` reads everything from bit 2 up, so a flag placed there
/// would silently change which body the fuzzer compressed.
///
/// `control[6]`'s remaining bits carry the long-distance switches, which need a
/// byte of their own because `control[3]`'s override mask is fully spent.
/// [`overrides_from`] sees them shifted down, so the two frame-format flags are
/// not part of the field it reads.
///
/// The two three-state mode switches come from `control[1]`'s upper bits.
/// `FUZZ_BLOCK_SIZES` has a power-of-two length, so the index below is exactly
/// the low three bits and the other five are free rather than merely
/// unlikely-to-correlate. They could not come from `control[6]`: only six of
/// its bits survive the shift, and the long-distance switches spend five.
fn encoder_options_from(control: [u8; 7]) -> (EncoderOptions, BodyShape) {
    let level = level_from(control[0]);
    let options = EncoderOptions {
        block_size: FUZZ_BLOCK_SIZES[usize::from(control[1]) % FUZZ_BLOCK_SIZES.len()],
        checksum: control[2] & 1 != 0,
        write_dict_id: control[2] & 2 != 0,
        compression_level: level,
        parameters: overrides_from(
            control[3],
            [control[4], control[5]],
            control[6] >> 2,
            control[1] >> 3,
        ),
        // Not fuzzed: the encoders check a pledge against what actually
        // arrived, so any value but the real length is a configuration error
        // rather than a body to compress. `a_pledged_size_is_declared_and_checked`
        // covers both sides of that.
        pledged_src_size: None,
        write_content_size: control[6] & 1 == 0,
        format: if control[6] & 2 == 0 {
            Format::Zstd1
        } else {
            Format::Zstd1Magicless
        },
    };
    (options, body_shape_from(control[2], level))
}

/// A [`ParameterOverrides`] from three fuzzer bytes and the long-distance field.
///
/// `mask` picks which parameters are overridden at all — most inputs should
/// leave most of them alone, because a level's own parameters are the ones the
/// encoder is tuned for and the interesting mutations are one or two steps off
/// them. The two value bytes are shared across the fields, mapped into each
/// parameter's published bounds so nothing here is rejected: a target that
/// spent its iterations on `Err(InvalidParameter)` would compress nothing.
///
/// `ldm` is a second mask, in bits of its own because `mask` has none left. Bit
/// 0 turns long-distance matching on and bits 1 through 4 pick which of its four
/// parameters are overridden — all four are ignored when it is off, so a
/// mutation to them is never silently inert in a configuration that could have
/// used them.
///
/// `modes` carries the two three-state switches, two bits each. They are not in
/// `mask`, whose bits mean "override this at all" against an `Option`; these
/// have no unset state to name, only a default one.
fn overrides_from(mask: u8, values: [u8; 2], ldm: u8, modes: u8) -> ParameterOverrides {
    // A table is `1 << log` 32-bit entries and adjustment only shrinks it when
    // the source size is known, which the streaming target's never is. Left at
    // the published ceiling of 30 the fuzzer spends its iterations on four
    // gibibytes of `calloc` rather than on the parse, and libFuzzer reports the
    // input as a slow unit. 22 is 16 MiB, still far wider than any level's row.
    // The published bounds are what `parameter_overrides_reject_values_outside_
    // their_bounds` and the round-trip property walk; this cap is about
    // throughput here, not about what the encoder accepts.
    const MAX_FUZZ_TABLE_LOG: u32 = 22;

    // The same cap for the long-distance table, which needs its own because
    // nothing shrinks it: the parser's tables are fitted to the source whenever
    // its size is known, where `LdmParameters::resolve` reads only the window
    // and the strategy. Its entries are eight bytes rather than four, so this is
    // two megabytes.
    //
    // It doubles as the window cap below, and that is what bounds the *derived*
    // table as well as an explicitly supplied one: an unset `ldm_hash_log`
    // resolves to `window_log - hash_rate_log`, which is never above the window.
    // Without it the 27-bit window that enabling long-distance matching forces
    // derives an eight-megabyte table to search a body that cannot reach past a
    // single block.
    const MAX_FUZZ_LDM_LOG: u32 = 18;

    // And a floor, which is about time rather than memory. A block is capped at
    // the window, so a narrow one turns a body into hundreds of blocks whose
    // per-block cost barely falls with their size: `btultra2` at a window of 10
    // spends 20 s of libFuzzer's 25 s budget on a 128 KiB body before the
    // matcher is switched on at all, and 26 s with it. The floor is measured
    // against that — a window of 14 costs 2.5 s on the same input.
    //
    // Nothing is lost by confining the band. Long-distance matching is for
    // distances the parser cannot reach, and every offset it emits is bounded by
    // the same window the parser's are, so a kilobyte of window has no long
    // distances in it to find. The clamp is deliberate rather than a remapping:
    // it keeps a value byte meaning the same window on both sides of the switch,
    // which is what lets a test hold the window equal and vary only the matcher.
    const MIN_FUZZ_LDM_LOG: u32 = 14;

    fn within(bounds: ParameterBounds, byte: u8) -> u32 {
        let span = bounds.max - bounds.min + 1;
        bounds.min + (u32::from(byte) % span)
    }
    fn within_table(bounds: ParameterBounds, byte: u8) -> u32 {
        within(bounds, byte).min(MAX_FUZZ_TABLE_LOG)
    }
    let pick = |bit: u8, byte: u8, bounds: ParameterBounds| {
        (mask & (1 << bit) != 0).then(|| within(bounds, byte))
    };
    let pick_table = |bit: u8, byte: u8, bounds: ParameterBounds| {
        (mask & (1 << bit) != 0).then(|| within_table(bounds, byte))
    };
    let long_distance_matching = if ldm & 1 != 0 {
        LdmMode::Enabled
    } else {
        LdmMode::Auto
    };
    let pick_ldm = |bit: u8, byte: u8, bounds: ParameterBounds| {
        (ldm & 1 != 0 && ldm & (1 << bit) != 0).then(|| within(bounds, byte))
    };
    ParameterOverrides {
        // Enabling long-distance matching *forces* a 27-bit window, and an
        // explicit `window_log` is the only thing that beats it, so this target
        // always supplies one rather than leaving it to the mask. The value is
        // still the fuzzer's; only the band is ours.
        window_log: match long_distance_matching {
            LdmMode::Enabled => Some(
                within(ParameterOverrides::WINDOW_LOG, values[0])
                    .clamp(MIN_FUZZ_LDM_LOG, MAX_FUZZ_LDM_LOG),
            ),
            _ => pick(0, values[0], ParameterOverrides::WINDOW_LOG),
        },
        hash_log: pick_table(1, values[1], ParameterOverrides::HASH_LOG),
        chain_log: pick_table(2, values[0].rotate_left(3), ParameterOverrides::CHAIN_LOG),
        search_log: pick(3, values[1].rotate_left(3), ParameterOverrides::SEARCH_LOG),
        min_match: pick(4, values[0].rotate_left(5), ParameterOverrides::MIN_MATCH),
        target_length: pick(
            5,
            values[1].rotate_left(5),
            ParameterOverrides::TARGET_LENGTH,
        ),
        strategy: (mask & 0xc0 != 0).then(|| {
            const STRATEGIES: [Strategy; 9] = [
                Strategy::Fast,
                Strategy::DoubleFast,
                Strategy::Greedy,
                Strategy::Lazy,
                Strategy::Lazy2,
                Strategy::BinaryTreeLazy2,
                Strategy::BinaryTreeOpt,
                Strategy::BinaryTreeUltra,
                Strategy::BinaryTreeUltra2,
            ];
            STRATEGIES[usize::from(values[0] ^ values[1]) % STRATEGIES.len()]
        }),
        // Auto twice over rather than a fourth state, since it is the default
        // every other target runs under and the two forced modes are the ones
        // worth reaching.
        //
        // These used to read `ldm`'s top two bits, which the caller's shift had
        // already cleared: `Disabled` was unreachable and the mode was a single
        // bit wide. Nothing failed, because every configuration here is a legal
        // one — an inert switch in a fuzz target is silent by construction,
        // which is why `every_mode_switch_is_reachable` now pins it.
        use_row_match_finder: match modes & 0b11 {
            1 => RowMatchFinderMode::Enabled,
            2 => RowMatchFinderMode::Disabled,
            _ => RowMatchFinderMode::Auto,
        },
        literal_compression: match (modes >> 2) & 0b11 {
            1 => LiteralCompressionMode::Enabled,
            2 => LiteralCompressionMode::Disabled,
            _ => LiteralCompressionMode::Auto,
        },
        long_distance_matching,
        ldm_hash_log: pick_ldm(
            1,
            values[1].rotate_left(1),
            ParameterOverrides::LDM_HASH_LOG,
        )
        .map(|log| log.min(MAX_FUZZ_LDM_LOG)),
        // `LDM_MIN_MATCH` runs to 4096 and one byte reaches 4..=259 of it. That
        // is the range worth walking anyway: a minimum above the body finds
        // nothing at all, and the default is 64.
        ldm_min_match: pick_ldm(
            2,
            values[0].rotate_left(1),
            ParameterOverrides::LDM_MIN_MATCH,
        ),
        ldm_bucket_size_log: pick_ldm(
            3,
            values[1].rotate_left(7),
            ParameterOverrides::LDM_BUCKET_SIZE_LOG,
        ),
        ldm_hash_rate_log: pick_ldm(
            4,
            values[0].rotate_left(7),
            ParameterOverrides::LDM_HASH_RATE_LOG,
        ),
    }
}

/// Decode a frame this crate just produced. Anything other than success is a
/// defect in the encoder: these bytes were not mutated by the fuzzer.
fn decode_own_frame(encoded: &[u8], format: Format) -> Vec<u8> {
    decode_all_with_options(
        encoded,
        DecoderOptions {
            format,
            // Deliberately unbounded. The bound that matters is the one the
            // frame declares for itself, which `execute_dictionary_match`
            // enforces against every offset; capping it here would only mask an
            // over-wide declaration behind a decoder error.
            max_window_size: None,
            max_output_size: Some(MAX_FUZZ_ENCODE_OUTPUT),
            verify_checksum: true,
            ..Default::default()
        },
    )
    .expect("a frame this crate produced must decode")
}

/// The largest body any encode target produces, and so the most any of their
/// frames can decode to.
const MAX_FUZZ_ENCODE_OUTPUT: usize = 3 << 19;

/// How many `push` calls the streaming target will split a body into. Small
/// chunks are the interesting case, but the interest is in the first few
/// hundred boundaries, not the millionth.
const MAX_FUZZ_PUSHES: usize = 4096;

/// Check the frame header against the window the level actually uses.
///
/// Every parser bounds the offsets it emits by that window, so a frame has no
/// reason to declare more than the window or the content, whichever is larger.
/// Declaring a spare block on top of the window is what this encoder used to do,
/// and it asked every decoder for memory no frame needed.
///
/// `size_hint` is what the encoder itself knew when it wrote the header. The
/// one-shot path has the whole body and shrinks the window to fit it; the
/// streaming path writes its header before the first `push` and has to declare
/// the level's full window, because more may still arrive. Passing the hint the
/// encoder had is what keeps this a bound on the encoder rather than a bound on
/// which entry point was used.
///
/// `block_size` is in the ceiling because the format caps a block at
/// `min(Window_Size, 128 KiB)`: neither encoder shrinks a block to fit a
/// narrow window, so both declare a window wide enough for the blocks they
/// emit. Only a `window_log` override can make that bind. The regression this
/// exists to catch — declaring `window + block_size` — still exceeds the
/// ceiling, because a sum is larger than either term.
///
/// The ceiling is rounded up to a representable window, because that is what
/// the header can express: a `block_size` of 131071 is written as 131072.
fn check_declared_window(
    body: &[u8],
    encoded: &[u8],
    options: EncoderOptions,
    size_hint: Option<usize>,
) {
    let header = match parse_frame_header_with_format(encoded, options.format)
        .expect("a frame this crate produced must parse")
    {
        FrameHeader::Zstandard(header) => header,
        FrameHeader::Skippable(_) => panic!("the encoder never emits a skippable frame"),
    };
    let params = compression_parameters_for_options(options, size_hint, None);
    let intended = params
        .max_history_bytes
        .max(options.block_size.max(1 << 10))
        .max(body.len()) as u64;
    let ceiling = representable_window_size(intended)
        .expect("a window this encoder can declare must be representable");
    assert!(
        header.window_size <= ceiling,
        "frame declares a {}-byte window for a {}-byte body at level {}, above the {}-byte ceiling",
        header.window_size,
        body.len(),
        options.compression_level.as_i32(),
        ceiling,
    );
}

/// One-shot encode across every level, block size, and header flag, then decode
/// the result and compare.
///
/// The round trip is a real oracle rather than a panic check. Decoding rejects
/// any offset beyond the frame's declared `Window_Size`, so this also verifies
/// that the parsers stayed inside the window they told the decoder about — the
/// one thing that has gone wrong here twice.
pub fn encode_roundtrip(data: &[u8]) {
    let _ = run_encode_roundtrip(data);
}

/// Returns how many bytes were compressed, so a test can tell a target that did
/// the work from one that bailed out at the first `let else` and asserted
/// nothing.
fn run_encode_roundtrip(data: &[u8]) -> Option<usize> {
    let (control, seed) = split_control::<7>(data)?;
    let (options, shape) = encoder_options_from(control);
    let body = shape_body(seed, options.compression_level, shape);

    let encoded = encode_all_with_options(&body, options)
        .expect("encoding arbitrary bytes with valid options cannot fail");

    check_declared_window(&body, &encoded, options, Some(body.len()));
    assert_eq!(
        decode_own_frame(&encoded, options.format),
        body,
        "one-shot round trip"
    );
    Some(body.len())
}

/// The same bodies through the streaming encoder, cut into pushes at
/// fuzzer-chosen boundaries with flushes scattered through them.
///
/// Chunking and flushing are the two ways a caller can move a block boundary,
/// and a block boundary is where the repeat offsets and the match finder's
/// retained history are carried across. The finder now lives for the life of
/// the frame, so those hand-offs are state rather than a rebuild.
pub fn streaming_encode_roundtrip(data: &[u8]) {
    let _ = run_streaming_encode_roundtrip(data);
}

fn run_streaming_encode_roundtrip(data: &[u8]) -> Option<usize> {
    let (control, seed) = split_control::<8>(data)?;
    let (options, shape) = encoder_options_from(control[..7].try_into().expect("seven bytes"));
    let body = shape_body(seed, options.compression_level, shape);

    // 1, 2, 4 ... 256 bytes, plus "everything in one push" so the unsplit path
    // stays covered. Raised until the body needs no more than `MAX_FUZZ_PUSHES`
    // of them, because a byte at a time through a megabyte is a million calls
    // that all take the same path as the first thousand.
    let chunk = match control[7] & 0x0f {
        shift @ 0..=8 => 1usize << shift,
        _ => body.len().max(1),
    }
    .max(body.len() / MAX_FUZZ_PUSHES)
    .max(1);
    // 0 means never flush. Otherwise flush every Nth push.
    let flush_every = usize::from(control[7] >> 4);

    let mut encoder = StreamingEncoder::new(options).expect("valid options");
    let mut encoded = Vec::new();
    for (index, piece) in body.chunks(chunk).enumerate() {
        encoder.push(piece).expect("push cannot fail before finish");
        if flush_every != 0 && index % flush_every == 0 {
            encoder.flush().expect("flush cannot fail before finish");
        }
        // Draining as we go is the usage the buffered-output guarantees are
        // written for, and it keeps peak memory independent of frame length.
        encoded.extend_from_slice(&encoder.take_output());
    }
    encoder.finish().expect("finish cannot fail");
    encoded.extend_from_slice(&encoder.take_output());

    check_declared_window(&body, &encoded, options, None);
    assert_eq!(
        decode_own_frame(&encoded, options.format),
        body,
        "streaming round trip"
    );

    // A reset has to leave nothing of the first frame behind. Now that the match
    // finder persists for the life of the encoder, a window that survived the
    // reset would let the second frame emit an offset into bytes its reader
    // never saw — which the decoder catches, but only if something asks it to.
    encoder.reset().expect("reset after finish");
    let second = &body[..body.len() / 2];
    encoder.push(second).expect("push after reset");
    encoder.finish().expect("finish after reset");
    let second_frame = encoder.take_output();

    check_declared_window(second, &second_frame, options, None);
    assert_eq!(
        decode_own_frame(&second_frame, options.format),
        second,
        "round trip of the frame after a reset"
    );
    Some(body.len())
}

/// Encode with a dictionary and decode with the same one.
///
/// The dictionary is whatever bytes the fuzzer put before the split. Most will
/// be raw content; the ones that happen to carry the formatted magic take the
/// formatted parse path instead, and both are worth reaching. A dictionary is
/// also the only way a frame's history starts non-empty, so the offsets that
/// point into it are unreachable from the other two targets.
pub fn dictionary_encode_roundtrip(data: &[u8]) {
    let _ = run_dictionary_encode_roundtrip(data);
}

fn run_dictionary_encode_roundtrip(data: &[u8]) -> Option<usize> {
    let (control, rest) = split_control::<8>(data)?;
    let (options, _) = encoder_options_from(control[..7].try_into().expect("seven bytes"));

    // Proportional split so the dictionary scales with the input rather than
    // swallowing short ones whole.
    let split = usize::from(control[7]) * rest.len() / 256;
    let (dict, seed) = rest.split_at(split);
    // Untiled here, so this target keeps the small bodies and the throughput
    // that go with them. What it reaches that the others do not is the
    // dictionary boundary — offsets that point behind the start of the frame,
    // and the tables a prepared dictionary carries — and none of that needs a
    // body larger than the fuzzer's own input. Tiling one anyway made this
    // target twenty times slower per iteration than the other two, which buys
    // shallower exploration of the thing it is actually for. The cost of that
    // choice is that the cparams tiers a large source selects are reached
    // through the other two encode targets and not through this one.
    let body = shape_body(seed, options.compression_level, BodyShape::Seed);

    // Bytes that open with the formatted magic but are not a valid dictionary
    // are rejected here, which is correct rather than a defect. Both
    // directions are parsed from the same bytes, which also holds them to
    // agreeing: an input either direction rejects and the other accepts fails
    // the `expect` below rather than passing quietly.
    let prepared = EncoderDictionary::new(dict).ok()?;
    let prepared_decoding = DecoderDictionary::new(dict).ok()?;

    // Through the reusable context rather than the free function, so the same
    // two encodes that check the round trip also check that a context carries
    // nothing from one call into the next. A prepared dictionary caches
    // parser-built tables across encodes, which is the most likely place for
    // state to leak, and the symptom would be output that differs between two
    // identical calls rather than output that fails.
    let mut context = Encoder::new();

    let encoded = context
        .encode_all_with_prepared_dict_and_options(&body, &prepared, options)
        .expect("encoding with a parsed dictionary cannot fail");
    let decoded = decode_all_with_prepared_dict_and_options(
        &encoded,
        &prepared_decoding,
        DecoderOptions {
            max_window_size: None,
            max_output_size: Some(MAX_FUZZ_ENCODE_OUTPUT),
            verify_checksum: true,
            // A magicless frame cannot be recognised, only asserted, so the
            // decoder has to be told what the encoder wrote.
            format: options.format,
            ..Default::default()
        },
    )
    .expect("a frame this crate produced must decode against its own dictionary");
    assert_eq!(decoded, body, "dictionary round trip");

    let again = context
        .encode_all_with_prepared_dict_and_options(&body, &prepared, options)
        .expect("second encode on the same context");
    assert_eq!(
        encoded, again,
        "a reused encoder produced different bytes for the same input"
    );
    Some(body.len())
}

fn split_point(data: &[u8]) -> usize {
    if data.is_empty() {
        0
    } else {
        (data[0] as usize) % (data.len() + 1)
    }
}

fn parse_sequence_chunk(data: &[u8], tables: &mut SequenceTablesState) {
    if let Ok(parsed) = parse_sequence_section(data, tables, TableTarget::Both) {
        if parsed.number_of_sequences <= MAX_FUZZ_SEQUENCE_COUNT {
            let _ = decode_sequence_commands(&parsed, tables);
        }
    }
}

/// These run under `cargo test --all-features`, which is what keeps the encode
/// targets from rotting between fuzz runs: a target that stopped compressing
/// anything would still exit zero under libFuzzer forever.
#[cfg(test)]
mod tests {
    use super::*;

    /// A body with structure worth matching against and no exact period, so a
    /// parser has to keep consulting its hash table rather than riding one
    /// repeat offset.
    fn structured_body(len: usize) -> Vec<u8> {
        let words = ["alpha", "bravo", "charlie", "delta", "echo", "foxtrot"];
        let mut out = Vec::with_capacity(len);
        let mut index = 0u64;
        while out.len() < len {
            let record = format!(
                "{{\"id\":{},\"tag\":\"{}\",\"n\":{}}}\n",
                index,
                words[(index as usize) % words.len()],
                index.wrapping_mul(2_654_435_761) as u32,
            );
            let take = record.len().min(len - out.len());
            out.extend_from_slice(&record.as_bytes()[..take]);
            index += 1;
        }
        out
    }

    /// Build a target input: control bytes followed by a body.
    fn input(control: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = control.to_vec();
        out.extend_from_slice(body);
        out
    }

    /// Override masks the target tests are swept against: none, one parameter
    /// at a time, every numeric one at once, and everything including the
    /// strategy. `0` first, because a level's own parameters are what these
    /// targets mostly exercise.
    const FUZZ_OVERRIDE_MASKS: [u8; 5] = [0x00, 0x01, 0x20, 0x3f, 0xff];

    /// Frame-format bytes: standard, no content size, magicless, both.
    const FUZZ_FRAME_FLAGS: [u8; 4] = [0, 1, 2, 3];

    #[test]
    fn every_level_and_block_size_round_trips_one_shot() {
        let body = structured_body(9000);
        for level in 0..32u8 {
            for block in 0..FUZZ_BLOCK_SIZES.len() as u8 {
                for flags in [0u8, 1, 3] {
                    for mask in FUZZ_OVERRIDE_MASKS {
                        let compressed = run_encode_roundtrip(&input(
                            &[level, block, flags, mask, 0x37, 0x9b, 0],
                            &body,
                        ))
                        .expect("the target must reach the encoder, not bail on a short input");
                        assert!(compressed > 0, "the target compressed an empty body");
                    }
                }
            }
        }
    }

    #[test]
    fn every_frame_format_round_trips_one_shot() {
        let body = structured_body(9000);
        for level in [0u8, 3, 12, 22, 25] {
            for frame_flags in FUZZ_FRAME_FLAGS {
                let compressed =
                    run_encode_roundtrip(&input(&[level, 2, 1, 0, 0, 0, frame_flags], &body))
                        .expect("the target must reach the encoder, not bail on a short input");
                assert!(compressed > 0, "the target compressed an empty body");
            }
        }
    }

    #[test]
    fn every_level_round_trips_through_the_streaming_encoder() {
        let body = structured_body(9000);
        for level in 0..32u8 {
            // One chunk shape per level rather than the full cross product:
            // the shapes are independent of the level, and the suite has to
            // stay quick enough to run on every commit.
            for chunking in [0x00u8, 0x05, 0x0f, 0x23] {
                for mask in FUZZ_OVERRIDE_MASKS {
                    let compressed = run_streaming_encode_roundtrip(&input(
                        &[level, 2, 1, mask, 0x37, 0x9b, 0, chunking],
                        &body,
                    ))
                    .expect("the target must reach the encoder, not bail on a short input");
                    assert!(compressed > 0, "the target compressed an empty body");
                }
            }
        }
    }

    #[test]
    fn every_level_round_trips_with_a_dictionary() {
        let body = structured_body(9000);
        for level in 0..32u8 {
            for split in [32u8, 128, 200] {
                for mask in FUZZ_OVERRIDE_MASKS {
                    let compressed = run_dictionary_encode_roundtrip(&input(
                        &[level, 3, 1, mask, 0x37, 0x9b, 0, split],
                        &body,
                    ))
                    .expect("raw dictionary content always parses");
                    assert!(compressed > 0, "the target compressed an empty body");
                }
            }
        }
    }

    /// The control prefix each target consumes is what separates configuration
    /// from body, so a target that read the wrong number of bytes would still
    /// pass every round trip above while compressing a body the fuzzer did not
    /// choose. Pinned here rather than left implicit in the array literals.
    #[test]
    fn the_targets_consume_the_control_prefix_they_document() {
        let body = structured_body(64);
        assert!(run_encode_roundtrip(&input(&[0; 7], &body)).is_some());
        assert!(run_encode_roundtrip(&[0; 6]).is_none());
        assert!(run_streaming_encode_roundtrip(&input(&[0; 8], &body)).is_some());
        assert!(run_streaming_encode_roundtrip(&[0; 7]).is_none());
        assert!(run_dictionary_encode_roundtrip(&input(&[0; 8], &body)).is_some());
        assert!(run_dictionary_encode_roundtrip(&[0; 7]).is_none());
    }

    /// The amplified shape exists so the fast parsers see a body longer than
    /// their window; a fuzz input is otherwise three orders of magnitude too
    /// short for that bound to bind. If this stops holding, the targets still
    /// pass and quietly stop covering the window.
    #[test]
    fn the_amplified_body_outgrows_the_level_one_window() {
        // `MIN_POSITIVE`, not `MIN`: this pins a property of level 1, which is
        // the narrowest window a fuzz target can reach. `MIN` used to name that
        // level and now names the negative floor, which no target generates.
        let level = CompressionLevel::MIN_POSITIVE;
        let params = crate::encode::compression_parameters_for_input(level, None, None);
        let amplified = shape_body(
            &structured_body(700),
            level,
            BodyShape::Tiled {
                target: body_cap_for(level),
            },
        );
        assert!(
            amplified.len() > params.max_history_bytes * 2,
            "amplified body is {} bytes against a {}-byte window",
            amplified.len(),
            params.max_history_bytes,
        );
    }

    /// Sizes a tiled body can take at `level`, over every flags byte.
    fn reachable_body_sizes(level: CompressionLevel) -> Vec<usize> {
        (0..=u8::MAX)
            .filter_map(|flags| match body_shape_from(flags | 4, level) {
                BodyShape::Tiled { target } => Some(target),
                BodyShape::Seed => None,
            })
            .collect()
    }

    /// Upstream picks compression parameters from a table indexed by how large
    /// the source is, so a body that can only be one size only ever reaches one
    /// row of it. Levels 13 and up were capped at 16 KiB, which is inside the
    /// smallest tier, so three quarters of the parameter table for those levels
    /// was unreachable from a fuzz input — including every window and chain log
    /// large enough to put the binary-tree match finder to work.
    ///
    /// This is the guarantee that the caps and the size table exist together to
    /// provide, and nothing else fails if it stops holding: the targets keep
    /// passing and quietly stop covering the tiers.
    #[test]
    fn every_level_reaches_every_cparams_tier() {
        // One size inside each tier `upstream_cparams_tier` selects between.
        let per_tier = [1 << 8, (16 << 10) + 1, (128 << 10) + 1, (256 << 10) + 1];
        for byte in 0..22u8 {
            let level = level_from(byte);
            let sizes = reachable_body_sizes(level);
            let reached: Vec<_> = sizes
                .iter()
                .map(|&size| {
                    crate::encode::compression_parameters_for_input(level, Some(size), None)
                        .upstream_cparams
                })
                .collect();
            for size in per_tier {
                let wanted =
                    crate::encode::compression_parameters_for_input(level, Some(size), None);
                assert!(
                    reached.contains(&wanted.upstream_cparams),
                    "level {} cannot reach the parameters it uses for a {}-byte source; \
                     largest reachable body is {}",
                    level.as_i32(),
                    size,
                    sizes.iter().max().expect("at least one tiled size"),
                );
            }
        }
    }

    /// Each tier boundary is inclusive, so a body has to be able to land on it
    /// and one byte past it for the difference to be under test at all.
    #[test]
    fn tiled_bodies_bracket_every_tier_boundary() {
        for byte in 0..22u8 {
            let level = level_from(byte);
            let sizes = reachable_body_sizes(level);
            for boundary in [16 << 10, 128 << 10, 256 << 10] {
                assert!(
                    sizes.contains(&boundary) || boundary > body_cap_for(level),
                    "level {} cannot produce a body of exactly {boundary} bytes",
                    level.as_i32(),
                );
                assert!(
                    sizes.iter().any(|&size| size > boundary),
                    "level {} cannot produce a body past {boundary} bytes",
                    level.as_i32(),
                );
            }
        }
    }

    /// A tiled body that repeated exactly would hide the parser behaviour these
    /// targets exist to reach, so the drift has to actually land.
    #[test]
    fn tiling_perturbs_every_repetition() {
        let seed = structured_body(64);
        let tiled = tile_with_drift(&seed, seed.len() * 8);
        let differing = tiled
            .chunks(seed.len())
            .filter(|tile| tile.len() == seed.len() && *tile != &seed[..])
            .count();
        assert_eq!(differing, 8, "every tile should differ from the seed");
    }

    /// Every long-distance configuration `control[6]` can name: the mode off,
    /// the mode on, and each of the four parameters overridden alongside it.
    ///
    /// The streaming target is here for the reason the whole item exists. Its
    /// window is pinned narrow enough that a body this size compacts several
    /// times, and the long-distance table is the one piece of frame state that
    /// has no rebuild to fall back on — every entry is an index into a buffer
    /// whose front keeps being dropped. The two ablations that checked that
    /// rebase both failed as an index far past the end of the buffer rather
    /// than as a ratio, which is the class these targets exist to find.
    #[test]
    fn every_long_distance_configuration_round_trips() {
        let body = tile_with_drift(&structured_body(700), 1 << 16);
        for ldm in 0..32u8 {
            // Bits 0 and 1 stay clear, so the frame format is the ordinary one
            // and only the long-distance field varies across the sweep.
            let frame_flags = ldm << 2;
            let case = format!("ldm bits {ldm:05b}");
            for level in [1u8, 5, 13] {
                let compressed =
                    run_encode_roundtrip(&input(&[level, 2, 1, 0, 0x37, 0x9b, frame_flags], &body))
                        .expect("the target must reach the encoder, not bail on a short input");
                assert!(
                    compressed > 0,
                    "{case}: the target compressed an empty body"
                );
            }
            for chunking in [0x05u8, 0x0f] {
                let compressed = run_streaming_encode_roundtrip(&input(
                    &[5, 2, 1, 0, 0x37, 0x9b, frame_flags, chunking],
                    &body,
                ))
                .expect("the target must reach the encoder, not bail on a short input");
                assert!(
                    compressed > 0,
                    "{case}: the target compressed an empty body"
                );
            }
            let compressed = run_dictionary_encode_roundtrip(&input(
                &[5, 3, 1, 0, 0x37, 0x9b, frame_flags, 64],
                &body,
            ))
            .expect("raw dictionary content always parses");
            assert!(
                compressed > 0,
                "{case}: the target compressed an empty body"
            );
        }
    }

    /// The long-distance bit has to reach the matcher, the table it asks for
    /// has to stay small enough to fuzz, and the window has to stay inside the
    /// band both of those depend on.
    ///
    /// Both routes into that table are under this: an explicit `ldm_hash_log`,
    /// and the derived one, which resolves to `window_log - hash_rate_log` and
    /// is bounded only because the window is. Nothing shrinks a long-distance
    /// table the way the source size shrinks the parser's, so a configuration
    /// that escaped the cap would spend the iteration on `memset` rather than
    /// on the matcher.
    ///
    /// The floor is the other half and is worth as much: a block is capped at
    /// the window, and `btultra2` at the narrowest window this crate accepts
    /// spends the whole of libFuzzer's default timeout on a body a hundredth of
    /// the size the targets allow. That is a slow unit rather than a crash, so
    /// nothing but this fails if the floor goes.
    #[test]
    fn the_long_distance_bit_reaches_the_matcher_with_a_bounded_table() {
        // Entries are eight bytes, so this is the two megabytes the cap buys.
        const MAX_ENTRIES: usize = 1 << 18;
        const WINDOW_BAND: std::ops::RangeInclusive<u32> = 14..=18;
        for mask in FUZZ_OVERRIDE_MASKS {
            for values in [[0u8, 0], [0x37, 0x9b], [0xff, 0xff], [7, 200], [17, 3]] {
                for ldm in 0..32u8 {
                    let case = format!("mask {mask:#04x} values {values:?} ldm {ldm:05b}");
                    let options = EncoderOptions {
                        compression_level: CompressionLevel::try_new(9).expect("a valid level"),
                        // The mode switches are held at their default: this is
                        // about the long-distance table's size, and neither of
                        // them reaches it.
                        parameters: overrides_from(mask, values, ldm, 0),
                        ..Default::default()
                    };
                    // No size hint, which is the streaming target's case and the
                    // one where nothing fits the window down afterwards.
                    let params = compression_parameters_for_options(options, None, None);
                    if ldm & 1 == 0 {
                        assert!(params.ldm.is_none(), "{case}: the matcher ran unasked");
                        continue;
                    }
                    let ldm_params = params.ldm.expect("the bit is set, so the matcher must run");
                    assert!(
                        1usize << ldm_params.hash_log <= MAX_ENTRIES,
                        "{case}: asks for a table of 2^{} entries",
                        ldm_params.hash_log,
                    );
                    let window_log = options
                        .parameters
                        .window_log
                        .expect("the matcher's window is always supplied, never forced");
                    assert!(
                        WINDOW_BAND.contains(&window_log),
                        "{case}: window log {window_log} is outside {WINDOW_BAND:?}",
                    );
                }
            }
        }
    }

    /// Every state of both mode switches has to be something a control byte can
    /// actually name.
    ///
    /// A switch the fuzzer cannot reach is not a failing test anywhere: the
    /// configurations it would have produced are all legal, so the target keeps
    /// passing on the ones it can still name. The row-finder switch spent a
    /// release in exactly that state, reading two bits of a byte whose caller
    /// had already shifted one of them away, and nothing noticed. This walks the
    /// byte the modes are cut from and pins that each state appears.
    #[test]
    fn every_mode_switch_is_reachable() {
        let mut rows = Vec::new();
        let mut literals = Vec::new();
        for byte in 0..=u8::MAX {
            let (options, _) = encoder_options_from([5, byte, 1, 0, 0, 0, 0]);
            rows.push(options.parameters.use_row_match_finder);
            literals.push(options.parameters.literal_compression);
        }
        for mode in [
            RowMatchFinderMode::Auto,
            RowMatchFinderMode::Enabled,
            RowMatchFinderMode::Disabled,
        ] {
            assert!(rows.contains(&mode), "no control byte names {mode:?}");
        }
        for mode in [
            LiteralCompressionMode::Auto,
            LiteralCompressionMode::Enabled,
            LiteralCompressionMode::Disabled,
        ] {
            assert!(literals.contains(&mode), "no control byte names {mode:?}");
        }
        // And the two are cut from disjoint bits, so a byte can ask for any
        // pairing of them rather than only the diagonal.
        assert!(
            (0..=u8::MAX).any(|byte| {
                let (options, _) = encoder_options_from([5, byte, 1, 0, 0, 0, 0]);
                options.parameters.use_row_match_finder == RowMatchFinderMode::Enabled
                    && options.parameters.literal_compression == LiteralCompressionMode::Disabled
            }),
            "the two mode switches cannot be set independently"
        );
    }

    /// Enabling the matcher has to change the frame. Every round trip above
    /// would pass just as well against a mode that resolved to parameters and
    /// then never ran, which is what these targets did until this landed.
    #[test]
    fn the_matcher_changes_the_frame_it_runs_on() {
        let body = tile_with_drift(&structured_body(700), 1 << 16);
        // `WINDOW_LOG` spans 10..=27, so a value byte of 6 resolves to 16 on
        // both sides and the cap the enabled branch applies does not bind.
        // Holding the window equal is what makes the comparison about the
        // matcher rather than about the window enabling it forces.
        let (off, _) = encoder_options_from([5, 2, 1, 0x01, 6, 0x9b, 0]);
        let (on, _) = encoder_options_from([5, 2, 1, 0x01, 6, 0x9b, 0b100]);
        assert_eq!(
            (off.parameters.window_log, on.parameters.window_log),
            (Some(16), Some(16)),
            "the two sides did not resolve to the same window"
        );
        assert!(
            compression_parameters_for_options(off, None, None)
                .ldm
                .is_none()
        );
        assert!(
            compression_parameters_for_options(on, None, None)
                .ldm
                .is_some()
        );

        let without = encode_all_with_options(&body, off).expect("valid options");
        let with = encode_all_with_options(&body, on).expect("valid options");
        assert_ne!(
            without, with,
            "the long-distance matcher was enabled and changed nothing"
        );
    }

    /// Short inputs are not a defect, but they must not be the only thing the
    /// fuzzer ever sees succeed.
    #[test]
    fn inputs_too_short_to_configure_are_declined() {
        for len in 0..3 {
            assert_eq!(run_encode_roundtrip(&vec![0u8; len]), None);
        }
        for len in 0..4 {
            assert_eq!(run_streaming_encode_roundtrip(&vec![0u8; len]), None);
            assert_eq!(run_dictionary_encode_roundtrip(&vec![0u8; len]), None);
        }
    }
}
