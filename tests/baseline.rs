//! **What replaces "nothing changed by accident."**
//!
//! Byte parity against upstream `zstd` was doing two jobs. It said *this is
//! correct*, which the decodability, structural and size-bound layers now cover
//! between them. It also said *nothing changed by accident*: every unintended
//! change to encoder output failed a parity sweep for free, without anyone
//! having to decide in advance which property to assert. Once this crate
//! deliberately stops producing upstream's bytes, that second job has nobody
//! doing it, and a refactor that quietly costs 2% compression is invisible
//! until somebody regenerates a report and reads it carefully.
//!
//! So this file records *our own* output and asserts it has not moved. See
//! `docs/ORACLE_PLAN.md` item 1.
//!
//! Two properties make this the durable layer rather than another parity sweep:
//!
//! - It **references nothing but our own history**, so it does not decay as the
//!   crate improves past upstream. Every check that compares against upstream's
//!   choices covers fewer rows with each deliberate divergence.
//! - It **needs no upstream helper**, so it can be far broader than any parity
//!   sweep and still runs where there is no C checkout at all. Every row here
//!   is an encode and a hash.
//!
//! `BENCHMARKS.md` is not this and should not be mistaken for it: it is
//! regenerated rather than asserted, it needs the helper to produce, and it has
//! already sat 20 commits stale without anything noticing.
//!
//! **When this test fails, that is the system working.** Read the diff, decide
//! whether the change was intended, and if it was, re-record with
//! `ZSTANDARD_UPDATE_BASELINE=1 cargo test --test baseline`. The update lands in
//! review as a diff of which rows moved and by how much, which is the point.

use std::{collections::BTreeMap, env, fmt::Write as _, fs, path::PathBuf};

#[allow(dead_code)]
#[path = "../src/support/corpora.rs"]
mod benchmark_corpora;

use zstandard::{
    CompressionLevel, DecoderDictionary, EncoderDictionary, EncoderOptions, LdmMode,
    ParameterOverrides, RowMatchFinderMode, StreamingEncoder, decode_all,
    decode_all_with_prepared_dict, encode_all_with_options,
    encode_all_with_prepared_dict_and_options, train_dictionary,
};

/// Corpus size for every row.
///
/// 256 KiB rather than the 1 MiB the parity sweeps use, because what this grid
/// buys is breadth of *configuration*, not of input length, and it is already
/// far wider than they are. It still spans two full blocks plus a partial one,
/// so block-boundary state -- repeat offsets carried across blocks, the block
/// splitter -- is exercised rather than skipped.
///
/// Buffer compaction is not reached by *this* length at the windows the levels
/// declare, which is what `NARROW_WINDOW_LOG` is for; the note there has the
/// measurement.
///
/// Sized against the clock, not chosen freely: at 512 KiB across the full grid
/// this test took 72 seconds, which is the range where a test stops being run
/// and starts being `#[ignore]`d, and an ignored regression baseline guards
/// nothing at all. The grid runs in a little under 40 of those seconds.
///
/// One thing this axis does *not* need is a band of overridden parameters, and
/// that was checked rather than assumed. Six clamps were perturbed to see what
/// the levels alone would miss, and the levels caught every one that could be
/// reached at all: the row finder's `search_log` floor moves 104 rows, the
/// seven-byte match hash 13. Two of the six are not reachable in practice --
/// the row finder's ceiling on `hash_log` needs a table of a gigabyte before it
/// binds, and the cap on search depth needs a search still paying its way past
/// 512 probes, which none does; every corpus here saturates by 10 even at a
/// megabyte. A grid of forced strategies crossed with parameter extremes was
/// built and then dropped, because no injection was found that it caught and
/// this axis did not.
const SIZE: usize = 256 * 1024;

/// Window for the `narrow-window` rows, which are the only ones here that
/// compact their buffer.
///
/// Without them nothing in this grid does. The narrowest window any level
/// declares unaided is 512 KiB, at levels 1 and below, and the streaming
/// encoder keeps history until the frame would exceed roughly twice the window,
/// so a 256 KiB body never comes close. That hole used to be recorded here as
/// needing bodies four times this size to close, which is wrong in a way worth
/// naming: it holds the *window* fixed and scales the body, when the ratio is
/// what matters and the window is the side that is free to move. Overriding it
/// down to 64 KiB compacts three times over the same 256 KiB, at no cost in
/// input length at all -- and the clock above is why that distinction decides
/// whether the coverage exists.
///
/// The hole was real and was measured rather than assumed, by re-running this
/// grid against two injected compaction defects. Dropping the three-byte
/// table's rebase -- the actual defect that was fixed -- moved **0 of 884**
/// rows. Making the contiguous state decline to rebase, so every binary-tree
/// row falls back to a rebuild, also moved **0**. With these rows the same two
/// injections move **19** and **60** of 1170, the second costing 0.988%.
///
/// Every corpus carries them, unlike the modes below, and the reason is the
/// same measurement: confining them to `WIDE_MODE_CORPORA` detected the
/// three-byte defect on 5 rows rather than 19, all on one corpus. A rebase
/// defect only shows where a stale entry actually wins a match, which is a
/// property of the bytes, so this is the one axis here whose value does
/// multiply across corpora. It is also close to free -- a 64 KiB window is far
/// cheaper to search than the megabytes the upper levels declare, so these are
/// among the fastest rows in the grid and widening them cost about a second.
///
/// Streaming only, because one-shot holds the whole input in one buffer and has
/// nothing to compact. What its twin plain-`streaming` row gives in exchange is
/// a control: these two differ by one parameter, so a change that moves both is
/// not about compaction and a change that moves only these is.
const NARROW_WINDOW_LOG: u32 = 16;

/// A dictionary in front of a *stream*, at the narrow window so it compacts.
///
/// Every other dictionary row here is one-shot, which left the rebuild branch of
/// `compact_frame` with nothing on it. A dictionary sits immediately before
/// frame position 0, so it leaves the window the moment history is dropped, and
/// carrying the prefixed state on would keep offering matches the decoder can no
/// longer reach. Converting it is the one route through that function which
/// cannot rebase and has to rebuild the table instead, and no `narrow-window`
/// row could reach it because none of them had a dictionary in front.
///
/// Measured, at 23 of 1274 rows and all of them this mode: rebuilding over only
/// the second half of the retained history moves those and nothing else.
///
/// The perturbation has to actually remove history. Inserting one position
/// fewer moves **nothing**, because the position dropped is the one the parser
/// is about to file itself -- which is worth knowing before reading a quiet
/// injection as an uncovered path, since that is exactly what it looks like.
///
/// One dictionary rather than both, because what is uncovered is the prefixed
/// *state*, which is the same machinery whether the content was trained or raw.
const STREAMED_DICT_MODE: &str = "streamed-dict";

/// Source lengths for [`ATTACHED_DICT_CAPS`], crossed with it to make the
/// attached-dictionary grid. Both have to clear the *smallest* entry of C's
/// `attachDictSizeCutoffs` (`zstd_compress.c:2296`) — 8 KiB for `fast`,
/// `btultra` and `btultra2`, 16 KiB for `dfast`, 32 KiB for the six strategies
/// between — because the test is `pledgedSrcSize <= cutoff` and a row that does
/// not attach is a row in the blind spot.
///
/// **The second entry is a power of two and that is the whole point of it.**
/// `ZSTD_adjustCParams_internal` fits the window to `highbit32(srcSize - 1) + 1`,
/// so a source of exactly `2^n` gets a window of exactly itself and sits on the
/// dictionary-retirement boundary from its very first block. Every other
/// dictionary row in this file avoids that, and 3 KiB was chosen *because* it
/// avoids it: an earlier version of this comment recorded that 4 KiB "retires
/// the whole dictionary on the first block, leaving zero dictionary matches at
/// every level" and treated that as a fact about the format to be designed
/// around. It was a defect — the retirement test was `>=` where C's is `>` —
/// and steering the grid around it is what kept it uncovered. Worth 1.05x to
/// 3.73x against upstream depending on corpus and dictionary size.
///
/// So both belong here: 3 KiB is the interior of the live branch, 4 KiB is its
/// edge, and a boundary needs a row on each side of it to be a boundary at all.
///
/// **The 3 KiB rows do move when the boundary is broken, and that is not the
/// same as covering it.** Reverting the fix moves them ~45%, but only through
/// the trainer: `DictionaryBuilder::measure` scores every candidate by
/// compressing the samples *with that candidate attached*, and the samples here
/// are `chunks(4096)` — the power-of-two case again, one level down. Hold the
/// dictionary bytes fixed and re-measure and the 3 KiB rows do not move by a
/// single byte on any corpus or capacity, while every 4 KiB row does. That
/// indirect path would vanish the day the trainer chunked to 4000 instead, so
/// the `-pow2` rows are the only coverage of the boundary that is about the
/// boundary.
const ATTACHED_DICT_SRCS: [(usize, &str); 2] = [(3 * 1024, ""), (4 * 1024, "-pow2")];

/// The longest of [`ATTACHED_DICT_SRCS`], which is the slice held back from
/// training. Excluding the largest source keeps *every* source out of the
/// training body, so no row compresses bytes its own dictionary was trained on,
/// and it does so with one trained dictionary per capacity rather than one per
/// cell — the trainer is by far the slowest thing in this file.
const ATTACHED_DICT_TRAIN_HOLDOUT: usize = 4 * 1024;

/// Trained dictionaries in front of a source small enough that they *attach*.
/// The two capacities and their mode names are in [`ATTACHED_DICT_CAPS`].
///
/// Every other dictionary row in this grid resolves down C's copying path, and
/// the two paths do not share a parameter set. `ZSTD_shouldAttachDict` attaches
/// only when the pledged size is at most the strategy's cutoff -- 32 KiB for
/// the lazy family, 8 KiB either side of it -- or is unknown; at [`SIZE`] with
/// a pledged size, all 24 levels copy. Copying leaves the applied parameters
/// equal to the CDict's, so the dictionary's own table geometry cannot differ
/// from the source's and nothing here could tell whether the right one was
/// being read. Attaching re-fits the applied parameters to the source and
/// leaves the dictionary's alone, and the two then disagree by several bits:
/// 16 KiB of dictionary against this source resolves `chain_log` 16 for the
/// dictionary and 9 for applied.
///
/// The dictionary that had been sized from the wrong one of those held 255
/// positions of a 16 KiB dictionary within reach of the tree and none of the
/// rest, which was worth 3.5x at levels 11 and 12 -- and no row of this grid,
/// no parity sweep and no benchmark case moved by a single byte, because the
/// regime a dictionary exists for had no coverage anywhere.
///
/// Trained rather than raw, and that is one of two load-bearing choices: a
/// raw-content dictionary is routed to the copying path regardless of size, so
/// a raw row here would sit in exactly the blind spot it is meant to leave.
///
/// The other is that the dictionary is trained on *this corpus* rather than on
/// the shared one the modes above use. That is not a detail. A first version of
/// this mode reused the shared dictionary, and every one of its 104 rows came
/// out byte-identical on a library with the defect still in it — a dictionary
/// trained on unrelated bytes yields so few dictionary matches that how far
/// into it the search can reach does not change the parse. The rows existed,
/// they simply detected nothing. Anything that makes the dictionary irrelevant
/// to the source makes this whole mode vacuous, so check that a deliberate
/// break still moves rows before trusting a quiet run.
///
/// **Two capacities, and neither can stand in for the other.**
///
/// 16 KiB is where the dictionary's own table geometry diverges most from the
/// applied one, and it is what caught the tables being sized from the wrong
/// set. 512 bytes is a different regime and covers a different field: it is
/// where `applied` and the separately-resolved `requested` disagree, on 60 of
/// 280 attached optimal-parser rows — sometimes as far apart as `hash_log` 12
/// against 22, and occasionally on the strategy itself. The 16 KiB rows do not
/// move when that resolution changes and the 512-byte rows do, so one size
/// cannot stand in for the other.
///
/// Crossed with [`ATTACHED_DICT_SRCS`], whose suffix is appended to these names.
const ATTACHED_DICT_CAPS: [(usize, &str); 2] =
    [(16 * 1024, "attached-dict"), (512, "attached-dict-small")];

/// Corpora that additionally carry the dictionary and long-distance rows.
///
/// Those three modes cost three fifths of the grid, and unlike the level axis
/// their value does not multiply across corpora: a dictionary planner defect
/// shows on any corpus with recurring structure, and the long-distance matcher
/// is inert on the synthetic patterns whose matches all sit inside the window.
/// One from each family -- structured records, log text, natural language,
/// tabular -- keeps what those modes actually detect.
/// Levels the `*-dict-row` modes are recorded at: one per prefixed row parser.
///
/// With a dictionary in front, level 4 resolves to `GreedyRow`, 5 to `LazyRow`
/// and 6 to `Lazy2Row`; 3 and below are the fast pair and 9 and up are binary
/// trees, none of which have a row variant for the override to select. So these
/// three levels are not a sample of the band -- they *are* the band, and every
/// other level would record a second copy of its plain `*-dict` row at the cost
/// of a full parse.
///
/// The mapping shifts with a dictionary present, which is why this is not the
/// no-dictionary band: a CDict resolves its own parameters and the strategy a
/// level lands on moves with them.
///
/// Naming levels rather than deriving them is only safe because something
/// checks they still land where this says: `tests/coverage.rs` fails if these
/// stop reaching all three prefixed row parsers, so a change to the level table
/// cannot silently empty this mode out.
const ROW_DICT_LEVELS: [i32; 3] = [4, 5, 6];

const WIDE_MODE_CORPORA: [&str; 4] = ["json-records", "log-lines", "wikipedia", "tabular-csv"];

/// Streaming piece size, chosen to be neither a block size nor a divisor of one
/// so that pushes land mid-block.
const PIECE: usize = 48 * 1024;

/// Every level the encoder resolves differently, negatives included.
///
/// The negative levels are here because they select a different `clevels.h` row
/// entirely and nothing else in the tree records their output. `-131072` is
/// reachable through the API but resolves the same as the rest of the deep
/// negatives, so the sample stops where the behaviour stops changing.
const LEVELS: [i32; 26] = [
    -7, -5, -3, -1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
];

/// FNV-1a, 64-bit.
///
/// Written out rather than pulled from `crate::xxhash` so that this file
/// depends on no crate internals: a baseline that moved because the hash
/// changed would be indistinguishable from one that moved because the encoder
/// did. Any stable hash works; this one is five lines and needs no explanation
/// to a future reader.
fn digest(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

fn baseline_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/baselines/encoder.tsv")
}

/// One row's key: everything that determines the frame except the encoder.
type Key = (String, i32, String);

fn record(
    rows: &mut BTreeMap<Key, (usize, u64)>,
    corpus: &str,
    level: i32,
    mode: &str,
    frame: &[u8],
) {
    let previous = rows.insert(
        (corpus.to_string(), level, mode.to_string()),
        (frame.len(), digest(frame)),
    );
    assert!(
        previous.is_none(),
        "{corpus} L{level} {mode} was recorded twice; the grid has a duplicate key"
    );
}

/// Push `input` through a streaming encoder in `PIECE`-sized pieces.
fn stream(input: &[u8], options: EncoderOptions) -> Vec<u8> {
    stream_with(input, options, None)
}

/// As [`stream`], optionally in front of a dictionary.
fn stream_with(
    input: &[u8],
    options: EncoderOptions,
    dictionary: Option<&EncoderDictionary>,
) -> Vec<u8> {
    let mut encoder = match dictionary {
        Some(dictionary) => StreamingEncoder::with_prepared_dict(dictionary, options).unwrap(),
        None => StreamingEncoder::new(options).unwrap(),
    };
    let mut streamed = Vec::new();
    for chunk in input.chunks(PIECE) {
        encoder.push(chunk).unwrap();
        streamed.extend_from_slice(&encoder.take_output());
    }
    encoder.finish().unwrap();
    streamed.extend_from_slice(&encoder.take_output());
    streamed
}

/// Encode the whole grid. Every frame is round-tripped through our own decoder
/// before being recorded, so a baseline can never enshrine a frame we cannot
/// read back -- upstream's ability to read it is
/// `upstream_decodes_frames_from_every_strategy_and_framing`'s job.
fn measure() -> BTreeMap<Key, (usize, u64)> {
    let mut rows = BTreeMap::new();

    // A dictionary of our own, so this grid needs no upstream helper. It is not
    // upstream's dictionary and does not need to be: the question here is
    // whether *our* output moved, not whose dictionary it moved against.
    let raw_dictionary = benchmark_corpora::build_raw_dictionary_input(112 * 1024);
    let samples: Vec<&[u8]> = raw_dictionary.chunks(4096).collect();
    let trained_dictionary = train_dictionary(&samples, 16 * 1024).unwrap();
    let raw = EncoderDictionary::new(&raw_dictionary).unwrap();
    let trained = EncoderDictionary::new(&trained_dictionary).unwrap();
    // The decoding halves of the same two dictionaries. Parsed from the same
    // bytes, so a row that round-trips proves the two directions built tables
    // that agree, not merely that one direction is self-consistent.
    let raw_decoding = DecoderDictionary::new(&raw_dictionary).unwrap();
    let trained_decoding = DecoderDictionary::new(&trained_dictionary).unwrap();

    for corpus in benchmark_corpora::benchmark_report_cases(SIZE) {
        // Trained on everything but the tail that `ATTACHED_DICT_MODE` then
        // compresses, so the dictionary is about the same kind of bytes without
        // containing them. Built once per corpus rather than once per level:
        // the trainer is the slowest thing in this file.
        let self_trained_bytes: Vec<Vec<u8>> = {
            let body = &corpus.input[..corpus.input.len() - ATTACHED_DICT_TRAIN_HOLDOUT];
            let samples: Vec<&[u8]> = body.chunks(4096).collect();
            ATTACHED_DICT_CAPS
                .iter()
                .map(|(cap, _)| train_dictionary(&samples, *cap).unwrap())
                .collect()
        };
        let self_trained: Vec<EncoderDictionary> = self_trained_bytes
            .iter()
            .map(|bytes| EncoderDictionary::new(bytes).unwrap())
            .collect();
        let self_trained_decoding: Vec<DecoderDictionary> = self_trained_bytes
            .iter()
            .map(|bytes| DecoderDictionary::new(bytes).unwrap())
            .collect();
        for level in LEVELS {
            let options = EncoderOptions {
                compression_level: CompressionLevel::try_new(level).unwrap(),
                ..Default::default()
            };

            let one_shot = encode_all_with_options(&corpus.input, options).unwrap();
            assert_eq!(decode_all(&one_shot).unwrap(), corpus.input);
            record(&mut rows, corpus.name, level, "one-shot", &one_shot);

            let streamed = stream(&corpus.input, options);
            assert_eq!(decode_all(&streamed).unwrap(), corpus.input);
            record(&mut rows, corpus.name, level, "streaming", &streamed);

            // The only rows here whose buffer compacts. See `NARROW_WINDOW_LOG`.
            let narrow = stream(
                &corpus.input,
                EncoderOptions {
                    parameters: ParameterOverrides {
                        window_log: Some(NARROW_WINDOW_LOG),
                        ..Default::default()
                    },
                    ..options
                },
            );
            assert_eq!(decode_all(&narrow).unwrap(), corpus.input);
            record(&mut rows, corpus.name, level, "narrow-window", &narrow);

            if !WIDE_MODE_CORPORA.contains(&corpus.name) {
                continue;
            }

            // The plain, non-row greedy/lazy/lazy2 parsers, which nothing else
            // in this grid reaches. `auto` sends those three strategies to the
            // row finder for any window above `1 << 14`, and the narrowest this
            // file asks for is 16, so every greedy/lazy row above is a *row*
            // parse and the hash-chain siblings behind them were uncovered.
            //
            // That hole hid a defect worth 2x. The chain walk broke off at a
            // 64-byte match, so the parse ignored `search_log` entirely and
            // settled for short matches where upstream kept walking; forced
            // Lazy on 256 KiB of `raw-dictionary` ran 1.99x upstream's size.
            // Every test in the tree passed throughout, and still passed when
            // the walk was fixed, because none of them ran these parsers.
            //
            // Recorded at every level rather than only the ones that select a
            // hash-chain strategy. The rows where the switch is inert duplicate
            // `one-shot` on purpose: that pins the inertness, so a change that
            // started moving a fast or binary-tree frame through this override
            // would show up here rather than nowhere.
            //
            // Confirmed to detect what it exists for, rather than assumed:
            // putting the 64-byte break back moves 14 of 1794 rows, all of them
            // `plain-chain`, `wikipedia` L5 by +13.8%. Note the sign is not
            // uniform -- `json-records` gets 0.2-0.7% *smaller* with the defect
            // in place, which is why a ratio bound would not have caught this
            // and a recorded baseline does.
            let plain_chain = encode_all_with_options(
                &corpus.input,
                EncoderOptions {
                    parameters: ParameterOverrides {
                        use_row_match_finder: RowMatchFinderMode::Disabled,
                        ..Default::default()
                    },
                    ..options
                },
            )
            .unwrap();
            assert_eq!(decode_all(&plain_chain).unwrap(), corpus.input);
            record(&mut rows, corpus.name, level, "plain-chain", &plain_chain);

            let long_distance = encode_all_with_options(
                &corpus.input,
                EncoderOptions {
                    parameters: ParameterOverrides {
                        long_distance_matching: LdmMode::Enabled,
                        ..Default::default()
                    },
                    ..options
                },
            )
            .unwrap();
            assert_eq!(decode_all(&long_distance).unwrap(), corpus.input);
            record(
                &mut rows,
                corpus.name,
                level,
                "long-distance",
                &long_distance,
            );

            for (dictionary, decoding, mode) in [
                (&raw, &raw_decoding, "raw-dict"),
                (&trained, &trained_decoding, "trained-dict"),
            ] {
                let framed =
                    encode_all_with_prepared_dict_and_options(&corpus.input, dictionary, options)
                        .unwrap();
                assert_eq!(
                    decode_all_with_prepared_dict(&framed, decoding).unwrap(),
                    corpus.input
                );
                record(&mut rows, corpus.name, level, mode, &framed);
            }

            // The row parsers *with a dictionary*, which nothing else reaches.
            //
            // This is the `plain-chain` hole from the other side, and it is the
            // same cause: `auto` needs a window above `1 << 14` to select the
            // row finder, and a CDict resolves to a window at or below that, so
            // with a dictionary in front `auto` leaves the row finder off at
            // every level. `GreedyRow`, `LazyRow` and `Lazy2Row` were therefore
            // unreachable in all three of their prefixed shapes -- nine of the
            // 44 combinations `tests/coverage.rs` enumerates, and the only nine
            // that were uncovered.
            //
            // Recording them found a defect immediately: `GreedyRow` against a
            // dictionary ran 1.06x to 1.40x upstream, where the same parser
            // without one was exact to 1.0001x. The cause was a repeat match at
            // depth 0 being weighed against a search upstream does not run;
            // `greedy_in_front_of_a_dictionary_takes_the_depth_zero_repeat` in
            // the interop suite is the guard, and these rows moved when it was
            // fixed. What is recorded here is now known-good.
            //
            // These sizes are ours alone. Nothing in this file consults
            // upstream, by design, and its dictionaries are far larger than the
            // interop helper's, so a level here resolves different cparams and
            // has no counterpart there. `the_baseline_dictionary_rows_stay_at_or
            // _under_upstream` is what covers that gap, and it exists because a
            // movement in these rows was otherwise unreadable: "ours changed"
            // with no way to ask whether it changed for the better.
            for (dictionary, decoding, mode) in [
                (&raw, &raw_decoding, "raw-dict-row"),
                (&trained, &trained_decoding, "trained-dict-row"),
            ]
            .into_iter()
            .filter(|_| ROW_DICT_LEVELS.contains(&level))
            {
                let framed = encode_all_with_prepared_dict_and_options(
                    &corpus.input,
                    dictionary,
                    EncoderOptions {
                        parameters: ParameterOverrides {
                            use_row_match_finder: RowMatchFinderMode::Enabled,
                            ..Default::default()
                        },
                        ..options
                    },
                )
                .unwrap();
                assert_eq!(
                    decode_all_with_prepared_dict(&framed, decoding).unwrap(),
                    corpus.input
                );
                record(&mut rows, corpus.name, level, mode, &framed);
            }

            // See `ATTACHED_DICT_MODE`. One-shot, because the pledged size is
            // what makes the applied parameters differ from the dictionary's at
            // all: an unknown size has nothing to fit the window to, so the two
            // sets come out equal and the row cannot tell which of them was
            // read. That was measured too — a streamed variant of this mode was
            // as quiet as the shared-dictionary one.
            //
            // The tail is the source and the head trained the dictionary, so
            // the two share structure without the source being a sample.
            for ((dictionary, decoding), (_, mode)) in self_trained
                .iter()
                .zip(self_trained_decoding.iter())
                .zip(ATTACHED_DICT_CAPS)
            {
                for (src_len, suffix) in ATTACHED_DICT_SRCS {
                    let short = &corpus.input[corpus.input.len() - src_len..];
                    let attached_dict =
                        encode_all_with_prepared_dict_and_options(short, dictionary, options)
                            .unwrap();
                    assert_eq!(
                        decode_all_with_prepared_dict(&attached_dict, decoding).unwrap(),
                        short
                    );
                    record(
                        &mut rows,
                        corpus.name,
                        level,
                        &format!("{mode}{suffix}"),
                        &attached_dict,
                    );
                }
            }

            // The row parsers against an *attached* dictionary, which is the
            // third of their three prefixed shapes and the one the rows above
            // cannot reach: a 256 KiB source puts C on the copying path, where
            // the dictionary is folded into the source's match state, and only
            // a source small enough to attach gives it a state of its own.
            //
            // One capacity and one source length rather than the full
            // attached-dictionary cross, because what is uncovered is the
            // parser, not the dictionary geometry -- the cross above already
            // varies that against the same code with `auto`'s choice of finder.
            if ROW_DICT_LEVELS.contains(&level) {
                let (dictionary, (_, mode)) = (&self_trained[0], ATTACHED_DICT_CAPS[0]);
                let decoding = &self_trained_decoding[0];
                let (src_len, _) = ATTACHED_DICT_SRCS[0];
                let short = &corpus.input[corpus.input.len() - src_len..];
                let attached_row = encode_all_with_prepared_dict_and_options(
                    short,
                    dictionary,
                    EncoderOptions {
                        parameters: ParameterOverrides {
                            use_row_match_finder: RowMatchFinderMode::Enabled,
                            ..Default::default()
                        },
                        ..options
                    },
                )
                .unwrap();
                assert_eq!(
                    decode_all_with_prepared_dict(&attached_row, decoding).unwrap(),
                    short
                );
                record(
                    &mut rows,
                    corpus.name,
                    level,
                    &format!("{mode}-row"),
                    &attached_row,
                );
            }

            // See `STREAMED_DICT_MODE`.
            let streamed_dict = stream_with(
                &corpus.input,
                EncoderOptions {
                    parameters: ParameterOverrides {
                        window_log: Some(NARROW_WINDOW_LOG),
                        ..Default::default()
                    },
                    ..options
                },
                Some(&raw),
            );
            assert_eq!(
                decode_all_with_prepared_dict(&streamed_dict, &raw_decoding).unwrap(),
                corpus.input
            );
            record(
                &mut rows,
                corpus.name,
                level,
                STREAMED_DICT_MODE,
                &streamed_dict,
            );
        }
    }
    rows
}

fn serialise(rows: &BTreeMap<Key, (usize, u64)>) -> String {
    let mut out = String::from(
        "# zstandard encoder baseline. Regenerate deliberately:\n\
                                #   ZSTANDARD_UPDATE_BASELINE=1 cargo test --test baseline\n\
                                # corpus\tlevel\tmode\tbytes\tfnv1a64\n",
    );
    for ((corpus, level, mode), (size, hash)) in rows {
        let _ = writeln!(out, "{corpus}\t{level}\t{mode}\t{size}\t{hash:016x}");
    }
    out
}

fn parse(text: &str) -> BTreeMap<Key, (usize, u64)> {
    let mut rows = BTreeMap::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = line.split('\t').collect();
        assert_eq!(fields.len(), 5, "malformed baseline row: {line}");
        rows.insert(
            (
                fields[0].to_string(),
                fields[1].parse().unwrap(),
                fields[2].to_string(),
            ),
            (
                fields[3].parse().unwrap(),
                u64::from_str_radix(fields[4], 16).unwrap(),
            ),
        );
    }
    rows
}

#[test]
fn encoder_output_matches_the_recorded_baseline() {
    let measured = measure();
    let path = baseline_path();

    if env::var_os("ZSTANDARD_UPDATE_BASELINE").is_some() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, serialise(&measured)).unwrap();
        eprintln!("rewrote {} with {} rows", path.display(), measured.len());
        return;
    }

    let Ok(text) = fs::read_to_string(&path) else {
        panic!(
            "no baseline at {}. If this is the first run, record one with \
             ZSTANDARD_UPDATE_BASELINE=1 cargo test --test baseline",
            path.display()
        );
    };
    let recorded = parse(&text);

    // Reported as a whole diff rather than failing on the first row, because the
    // useful question is nearly always "what shape is this change" -- one row,
    // one level band, or everything -- and a first-row failure cannot answer it.
    let mut moved = Vec::new();
    let mut added = Vec::new();
    for (key, &(size, hash)) in &measured {
        match recorded.get(key) {
            None => added.push(key.clone()),
            Some(&(was_size, was_hash)) if (was_size, was_hash) != (size, hash) => {
                moved.push((key.clone(), was_size, size, was_hash == hash));
            }
            Some(_) => {}
        }
    }
    let dropped: Vec<_> = recorded
        .keys()
        .filter(|key| !measured.contains_key(*key))
        .cloned()
        .collect();

    if moved.is_empty() && added.is_empty() && dropped.is_empty() {
        return;
    }

    let mut report = String::new();
    let _ = writeln!(
        report,
        "encoder output moved against tests/baselines/encoder.tsv \
         ({} rows changed, {} added, {} dropped of {} measured)",
        moved.len(),
        added.len(),
        dropped.len(),
        measured.len(),
    );
    let total_was: usize = moved.iter().map(|(_, was, _, _)| was).sum();
    let total_now: usize = moved.iter().map(|(_, _, now, _)| now).sum();
    if total_was > 0 {
        let _ = writeln!(
            report,
            "changed rows total {total_was} -> {total_now} ({:+.3}%)",
            100.0 * (total_now as f64 - total_was as f64) / total_was as f64,
        );
    }
    for ((corpus, level, mode), was, now, same_size) in moved.iter().take(60) {
        if *same_size {
            // Worth calling out separately: the frame changed but its length did
            // not, which a size-only baseline would have missed entirely.
            let _ = writeln!(
                report,
                "  {corpus} L{level} {mode}: {was} bytes, same size, different bytes"
            );
        } else {
            let _ = writeln!(
                report,
                "  {corpus} L{level} {mode}: {was} -> {now} ({:+.3}%)",
                100.0 * (*now as f64 - *was as f64) / *was as f64,
            );
        }
    }
    if moved.len() > 60 {
        let _ = writeln!(report, "  ... and {} more", moved.len() - 60);
    }
    for key in added.iter().take(20) {
        let _ = writeln!(report, "  added: {} L{} {}", key.0, key.1, key.2);
    }
    for key in dropped.iter().take(20) {
        let _ = writeln!(report, "  dropped: {} L{} {}", key.0, key.1, key.2);
    }
    let _ = writeln!(
        report,
        "If this change was intended, re-record with \
         ZSTANDARD_UPDATE_BASELINE=1 cargo test --test baseline and explain the \
         deltas in the commit message."
    );
    panic!("{report}");
}
