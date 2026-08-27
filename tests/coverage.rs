//! **Which encoder paths does the test suite actually run?**
//!
//! Every other layer asserts that some configuration produces the right bytes.
//! This one asserts that the configurations exist at all.
//!
//! It was written after a defect worth 2x lived in the tree with 555 tests
//! green. `ZSTD_c_useRowMatchFinder` resolves from the window: `auto` turns the
//! row finder on above `1 << 14` and off at or below it. No level asks for a
//! window that narrow and every CDict resolves to one, so *without* a
//! dictionary `auto` always chose the row parsers and *with* one it never did.
//! Six of the eleven parser strategies were therefore unreachable in one shape
//! or another, and no test anywhere said so, because a parser that never runs
//! fails nothing.
//!
//! An auto-resolved switch is what makes this invisible. A parameter a caller
//! sets is a parameter somebody thought about; a parameter the library resolves
//! can pin a whole family of code paths off without anyone choosing it.
//!
//! The audit compares two sets of `(parser, dictionary mode, table source)`
//! triples, both measured by running the encoder rather than reasoned about:
//!
//! - **reachable** — what a caller can get to, by forcing every `Strategy`
//!   against every `RowMatchFinderMode` and a spread of windows, with and
//!   without a dictionary, at large and small sources.
//! - **covered** — what the axes in `tests/baseline.rs` reach. That list is
//!   duplicated here rather than shared, which is the one weak joint in this
//!   file: adding a mode there and not here understates coverage, which is the
//!   safe direction, and adding one here and not there is caught by the
//!   assertion below.
//!
//! `reachable - covered` must be empty.

use std::collections::BTreeSet;

#[allow(dead_code)]
#[path = "../src/support/corpora.rs"]
mod benchmark_corpora;

use zstandard::{
    BlockTrace, CompressionLevel, EncoderDictionary, EncoderOptions, LdmMode, ParameterOverrides,
    RowMatchFinderMode, Strategy, trace_first_block_with_options,
    trace_first_block_with_prepared_dict_and_options, train_dictionary,
};

/// The levels `tests/baseline.rs` records, so "covered" means what that grid
/// covers rather than what some other spread would.
const LEVELS: [i32; 26] = [
    -7, -5, -3, -1, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22,
];

/// The band `tests/baseline.rs` records its `*-dict-row` modes at, one level per
/// prefixed row parser. Mirrored here so this file does not credit coverage
/// those rows do not provide; if the band stops reaching all three parsers, the
/// assertion below is what says so.
const ROW_DICT_LEVELS: [i32; 3] = [4, 5, 6];

/// Source length for the rows that stand in for the baseline's attached-dictionary
/// modes. A dictionary only attaches below 32 KiB, and attaching is what puts
/// the dictionary in its own match state instead of folded into the source's.
const SMALL_SOURCE: usize = 900;

fn describe(trace: &BlockTrace) -> String {
    let p = trace.compression_parameters;
    format!(
        "{:?} / {:?} / {:?}",
        p.parser_strategy, p.dictionary_mode, p.dict_table_source
    )
}

struct Corpus {
    input: Vec<u8>,
    raw: EncoderDictionary<'static>,
    trained: EncoderDictionary<'static>,
}

impl Corpus {
    fn observe(
        &self,
        into: &mut BTreeSet<String>,
        level: i32,
        parameters: ParameterOverrides,
        with_dictionary: bool,
        small_source: bool,
    ) {
        let options = EncoderOptions {
            compression_level: CompressionLevel::try_new(level).expect("a level in range"),
            parameters,
            ..Default::default()
        };
        let src: &[u8] = if small_source {
            &self.input[..SMALL_SOURCE]
        } else {
            &self.input
        };
        if with_dictionary {
            for dictionary in [&self.raw, &self.trained] {
                // An error here is a configuration this crate refuses, not a
                // path to record: long-distance matching with a dictionary is
                // the one that does it, and it is refused on purpose.
                if let Ok(trace) =
                    trace_first_block_with_prepared_dict_and_options(src, dictionary, options)
                {
                    into.insert(describe(&trace));
                }
            }
        } else if let Ok(trace) = trace_first_block_with_options(src, options) {
            into.insert(describe(&trace));
        }
    }
}

fn corpus() -> Corpus {
    let case = benchmark_corpora::benchmark_report_cases(256 * 1024)
        .into_iter()
        .find(|case| case.name == "json-records")
        .expect("the corpus set always carries json-records");
    let body = &case.input[..case.input.len() - 8192];
    let samples: Vec<&[u8]> = body.chunks(4096).collect();
    let trained = train_dictionary(&samples, 4096).expect("a trainable corpus");
    Corpus {
        input: case.input.clone(),
        // Raw content rather than a formatted dictionary, so both dictionary
        // parse paths are represented.
        raw: EncoderDictionary::from_shared(vec![0x5a; 4096]).expect("raw content"),
        trained: EncoderDictionary::from_shared(trained).expect("a trained dictionary"),
    }
}

/// What the axes in `tests/baseline.rs` reach.
fn covered(corpus: &Corpus) -> BTreeSet<String> {
    let narrow = ParameterOverrides {
        window_log: Some(16),
        ..Default::default()
    };
    let row_off = ParameterOverrides {
        use_row_match_finder: RowMatchFinderMode::Disabled,
        ..Default::default()
    };
    let row_on = ParameterOverrides {
        use_row_match_finder: RowMatchFinderMode::Enabled,
        ..Default::default()
    };
    let long_distance = ParameterOverrides {
        long_distance_matching: LdmMode::Enabled,
        ..Default::default()
    };

    let mut seen = BTreeSet::new();
    for level in LEVELS {
        // `one-shot` and `streaming`, which resolve the same parameters.
        corpus.observe(
            &mut seen,
            level,
            ParameterOverrides::default(),
            false,
            false,
        );
        // `narrow-window`.
        corpus.observe(&mut seen, level, narrow, false, false);
        // `plain-chain`.
        corpus.observe(&mut seen, level, row_off, false, false);
        // `long-distance`.
        corpus.observe(&mut seen, level, long_distance, false, false);
        // `raw-dict` and `trained-dict`.
        corpus.observe(&mut seen, level, ParameterOverrides::default(), true, false);
        // `raw-dict-row` and `trained-dict-row`, at their sampled band.
        if ROW_DICT_LEVELS.contains(&level) {
            corpus.observe(&mut seen, level, row_on, true, false);
        }
        // `attached-dict`, which needs a source small enough to attach.
        corpus.observe(&mut seen, level, ParameterOverrides::default(), true, true);
        // `attached-dict-row`, likewise.
        if ROW_DICT_LEVELS.contains(&level) {
            corpus.observe(&mut seen, level, row_on, true, true);
        }
        // `streamed-dict`.
        corpus.observe(&mut seen, level, narrow, true, false);
    }
    seen
}

/// What a caller can reach through the public API.
fn reachable(corpus: &Corpus) -> BTreeSet<String> {
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
    // Both sides of the row finder's window boundary, plus the levels' own.
    const WINDOWS: [Option<u32>; 4] = [Some(14), Some(16), Some(20), None];
    // Enough to move the parameter tier; the strategy is forced, so the level
    // is only here for the table sizes it picks.
    const SAMPLE_LEVELS: [i32; 5] = [-3, 1, 5, 12, 19];

    let mut seen = BTreeSet::new();
    for strategy in STRATEGIES {
        for use_row_match_finder in [
            RowMatchFinderMode::Auto,
            RowMatchFinderMode::Enabled,
            RowMatchFinderMode::Disabled,
        ] {
            for window_log in WINDOWS {
                for level in SAMPLE_LEVELS {
                    let parameters = ParameterOverrides {
                        strategy: Some(strategy),
                        use_row_match_finder,
                        window_log,
                        ..Default::default()
                    };
                    for small_source in [false, true] {
                        for with_dictionary in [false, true] {
                            corpus.observe(
                                &mut seen,
                                level,
                                parameters,
                                with_dictionary,
                                small_source,
                            );
                        }
                    }
                }
            }
        }
    }
    seen
}

#[test]
fn every_reachable_encoder_path_is_covered_by_the_baseline() {
    let corpus = corpus();
    let covered = covered(&corpus);
    let reachable = reachable(&corpus);

    let uncovered: Vec<&String> = reachable.difference(&covered).collect();
    assert!(
        uncovered.is_empty(),
        "{} of {} reachable encoder paths are not covered by any baseline row:\n  {}\n\n\
         Each is a (parser / dictionary mode / dictionary table source) triple that a caller \
         can reach and no recorded row runs. Add an axis to `tests/baseline.rs` that reaches \
         it, and the matching axis to `covered` in this file.",
        uncovered.len(),
        reachable.len(),
        uncovered
            .iter()
            .map(|path| path.as_str())
            .collect::<Vec<_>>()
            .join("\n  "),
    );

    // Non-vacuity: a comparison of two empty sets would pass just as well, and
    // so would one where `reachable` had quietly stopped exercising anything.
    // The count is not pinned, because a new parser or dictionary mode should
    // raise it without failing here -- the assertion above is what catches an
    // uncovered one.
    assert!(
        reachable.len() >= 40,
        "the reachable sweep found only {} paths, which is fewer than the eleven parsers \
         and three dictionary shapes can produce -- it has stopped reaching them",
        reachable.len(),
    );
}
