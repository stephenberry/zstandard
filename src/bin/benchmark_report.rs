// Shared support module: several targets `#[path]`-include this same file and
// each uses a different subset, so the rest reads as dead in every one of
// them. Deleting is not an option — what one target drops, another needs.
use std::{
    collections::{BTreeSet, HashMap},
    fmt::Write,
    fs,
    hint::black_box,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

#[path = "../support/corpora.rs"]
#[allow(dead_code)]
mod corpora;
#[path = "../support/upstream_zstd.rs"]
#[allow(dead_code)]
mod upstream_zstd;

use corpora::{CorpusCase, DictKind, benchmark_report_cases};
use upstream_zstd::{
    BenchCompressOutput, BenchTiming, benchmark_compress_mode_with_output, benchmark_mode,
    compress_streaming_once, decompress_once, emit_raw_dictionary, emit_trained_dictionary,
    require_helper, try_compress_once,
};
use zstandard::{
    BlockTrace, BlockTraceDecision, CompressionLevel, DecodeStageProfile, Decoder,
    DecoderDictionary, DecoderOptions, EncodeStageProfile, Encoder, EncoderDictionary,
    EncoderOptions, PlannerPhases, StreamingEncoder, decode_all, decode_all_with_prepared_dict,
    encode_all_with_options, encode_all_with_prepared_dict_and_options,
};

const QUICK_STAGE_TARGET_BYTES: usize = 8 * 1024 * 1024;
const DEFAULT_STAGE_TARGET_BYTES: usize = 32 * 1024 * 1024;
/// Wall-clock budget for one timing trial, which sizes that trial's iteration
/// count.
///
/// Iterations used to be sized by a fixed byte target — 64 MiB of encode per
/// row regardless of level — so every row ran the same 15 encodes of 4 MiB and
/// the time per row scaled with the inverse of throughput. That gave the
/// slowest rows the most repetition, which is backwards. Measured on the
/// report this replaced: `wikipedia` L1 timed a 1.6 ms window that wanted more
/// passes, while `tabular-csv` L22 spent 55 s per trial re-measuring something
/// already thousands of times above timer noise. Levels 16-22 were 83% of a
/// 30-minute sweep and levels 1-10 were 2.2%, with encode 99.3% of the whole.
///
/// Sizing by time spends the effort where it buys confidence. Note that level
/// is the wrong key for this: at level 22 the corpus spans 2049x in encode
/// throughput, from `tabular-csv` at 1.1 MiB/s to `repeated-chunk` at 2233
/// MiB/s, so a per-level iteration table would starve one end while still
/// overpaying at the other. Only a measured probe handles both.
///
/// This is also the floor on the whole sweep, and the reason it is 60 ms
/// rather than 100. Every row times four quantities — encode and decode on
/// both sides — at [`TIMING_TRIALS`] trials each, so a row cannot cost less
/// than twelve budgets however fast it runs, and 242 rows at 100 ms put an
/// unavoidable 290 s under a report that has to stay inside ten minutes. At
/// 60 ms that floor is 174 s. The risk this trades against is measuring over
/// too short a window, and 60 ms is not near it: the reports this replaced
/// timed some fast rows over 1.6 ms, which is where that failure actually
/// lives. Treat fast-level throughput from reports generated before this as
/// measured over a wider window, not a better one.
const DEFAULT_TRIAL_BUDGET: Duration = Duration::from_millis(60);
const QUICK_TRIAL_BUDGET: Duration = Duration::from_millis(25);
/// Iteration bounds per trial.
///
/// The floor is 1: an iteration that already exceeds the budget on its own is
/// its own measurement, and repeating it inside the trial would only re-time
/// what [`TIMING_TRIALS`] is about to time again. The ceiling bounds the
/// fastest rows, where a 100 ms budget would otherwise ask for a loop tens of
/// thousands deep — `repeated-chunk` decodes at several GiB/s.
const MIN_ITERATIONS: usize = 1;
const MAX_ITERATIONS: usize = 128;
/// Bytes of input per corpus case.
///
/// This has to stay above the point where the encoder changes behavior across
/// blocks, or the report cannot see the defects that only appear there. At the
/// previous 512 KiB — four blocks — the pre-parse block splitter looked
/// harmless; it costs 17-21% of ratio at levels 5 through 15 once input passes
/// roughly 1 MiB, and no row in the report moved. Keep this comfortably above
/// that threshold.
const DEFAULT_INPUT_BYTES: usize = 4 * 1024 * 1024;
const BLOCK_SIZE: usize = 128 * 1024;
const MIB: f64 = 1024.0 * 1024.0;
/// Timing trials per throughput measurement, of which the fastest is reported.
///
/// One timed run of N iterations is a single sample, and background load only
/// ever adds to it, so the fastest trial is the closest estimate of what the
/// code costs. A transient stall spoils one trial instead of the whole row.
///
/// Keep this at 3 even when trimming sweep time. It is the tempting lever,
/// being an immediate 3x, but the incident below is what it was added to catch
/// and the cost is bounded: each trial is one [`DEFAULT_TRIAL_BUDGET`], so the
/// whole row is three of them however slow the level is. Cut the iterations
/// inside a trial instead.
///
/// Measured before this existed: two sweeps four commits apart moved 17 encode
/// rows and 34 decode rows across the 50% threshold this report gates on,
/// including rows on the one-shot decode path that none of those commits
/// touched. Three back-to-back sweeps of genuinely identical code then put one
/// row at 0.48x, 0.45x and 0.51x, straddling the same threshold. The ratio
/// columns were unaffected throughout, being byte-exact.
///
/// More trials is not what this measurement most needs, though. **The two sides
/// of each row's comparison are timed seconds apart**, and a whole sweep runs
/// ten minutes of sustained load, which is long enough for the machine to drift
/// underneath it. Per row the order is: upstream's encode trials, upstream's
/// decode trials, our decode trials, then a subprocess round trip and our own
/// to verify the bytes, and only then our encode trials. So the encode pair
/// that becomes one ratio is measured at opposite ends of the row, and the
/// later a case sits in the sweep the more drift it has accumulated --
/// `raw-dictionary`, tenth of eleven, moved its decode count from 0 and 2 of 22
/// to 16 with nothing in the library changed.
///
/// The fix is to interleave the trials, alternating the two sides within one
/// loop so each pair is measured under the same conditions, taking the fastest
/// of each. Until that is done, treat a case's position in the sweep as part of
/// its error bar, and see [`SpeedSummary::is_broadly_behind`] for what that
/// does to the summary flag.
const TIMING_TRIALS: usize = 3;

fn main() {
    let config = parse_config();
    let helper = require_helper("benchmark_report");
    let raw_dictionary = emit_raw_dictionary(helper);
    let trained_dictionary = emit_trained_dictionary(helper);
    let raw_prepared = EncoderDictionary::new(&raw_dictionary).expect("raw dictionary must parse");
    let trained_prepared =
        EncoderDictionary::new(&trained_dictionary).expect("trained dictionary must parse");
    // The decoding halves. Since the split, an encoding dictionary carries no
    // decode tables, so the decode columns need their own parse of the same
    // bytes; the content is identical, and only the tables differ.
    let raw_decoding =
        DecoderDictionary::new(&raw_dictionary).expect("raw dictionary must parse for decoding");
    let trained_decoding = DecoderDictionary::new(&trained_dictionary)
        .expect("trained dictionary must parse for decoding");
    let cases = filter_cases(
        benchmark_report_cases(config.input_bytes),
        &config.case_filters,
    );
    let stage_levels = config
        .levels
        .iter()
        .copied()
        .filter(|level| matches!(level, 3..=7))
        .collect::<Vec<_>>();

    let mut report = String::new();
    let mut rust_encode_ok_rows = 0usize;
    let mut rust_decode_ok_rows = 0usize;
    let mut encode_fail_rows = Vec::new();
    let mut decode_fail_rows = Vec::new();
    let mut ratio_regressions: Vec<RatioRegression> = Vec::new();
    let mut speed_rows: Vec<SpeedRow> = Vec::new();
    let mut streaming_deltas: Vec<StreamingDelta> = Vec::new();
    let mut one_shot_by_case: HashMap<&'static str, HashMap<u8, usize>> = HashMap::new();
    let total_rows = cases.len() * config.levels.len();
    let total_rust_encode_rows = cases.len()
        * config
            .levels
            .iter()
            .filter(|&&level| i32::from(level) <= CompressionLevel::MAX.as_i32())
            .count();

    write_header(&mut report, &config, cases.len());

    for case in &cases {
        eprintln!("benchmarking {}", case.name);
        let case_start = Instant::now();
        let prepared_dictionary =
            prepared_dictionary(case.dict_kind, &raw_prepared, &trained_prepared);
        let prepared_decoding_dictionary =
            prepared_decoding_dictionary(case.dict_kind, &raw_decoding, &trained_decoding);
        let modes = case_modes(case.dict_kind);
        let stage_iterations = choose_iterations(config.stage_target_bytes, case.input.len());
        // Retained so the streaming section can report what streaming costs
        // against one-shot without encoding the case a second time.
        let mut one_shot_bytes: HashMap<u8, usize> = HashMap::new();

        writeln!(&mut report, "## {}", case.name).unwrap();
        writeln!(&mut report).unwrap();
        writeln!(&mut report, "{}", case.description).unwrap();
        writeln!(&mut report).unwrap();
        writeln!(&mut report, "- Input bytes: {}", case.input.len()).unwrap();
        writeln!(
            &mut report,
            "- Dictionary mode: {}",
            dict_kind_name(case.dict_kind)
        )
        .unwrap();
        if let Some(dictionary) =
            dictionary_bytes(case.dict_kind, &raw_dictionary, &trained_dictionary)
        {
            // Stated because the throughput on these two cases is far more a
            // statement about the prefix machinery than about dictionaries.
            // Both fixtures are the interop suite's, sized for a parity test
            // rather than for this, and against a 4 MiB frame a dictionary
            // this small moves the ratio by well under a percent while the
            // frame still pays for a prefix on every block. Read the encode
            // column here as an overhead measurement; the regime dictionaries
            // are actually used in -- a large dictionary against a small
            // input -- is not covered by any case in this report.
            writeln!(
                &mut report,
                "- Dictionary bytes: {} (1 per {} bytes of input)",
                dictionary.len(),
                case.input.len() / dictionary.len().max(1)
            )
            .unwrap();
        }
        writeln!(
            &mut report,
            "- Timing trial budget: {} ms",
            config.trial_budget.as_millis()
        )
        .unwrap();
        writeln!(&mut report).unwrap();
        writeln!(
            &mut report,
            "| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |"
        )
        .unwrap();
        writeln!(
            &mut report,
            "| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |"
        )
        .unwrap();

        for &level in &config.levels {
            // A single-iteration probe, which both sizes the timing trials and
            // produces the reference bytes the ratio column and every decode
            // row below are measured against.
            let upstream_encode_probe = benchmark_compress_mode_with_output(
                helper,
                modes.bench_compress_with_output_mode,
                1,
                i32::from(level),
                false,
                &case.input,
            );
            let encode_iterations =
                iterations_for_budget(upstream_encode_probe.timing.elapsed_ns, config.trial_budget);
            let encode_probe_timing = upstream_encode_probe.timing;
            let upstream_encoded = upstream_encode_probe.encoded;
            let upstream_ratio = upstream_encoded.len() as f64 / case.input.len() as f64;
            let upstream_encode_metrics = upstream_encode_best_of(
                &upstream_encoded,
                (encode_iterations == 1).then_some(encode_probe_timing),
                || {
                    benchmark_compress_mode_with_output(
                        helper,
                        modes.bench_compress_with_output_mode,
                        encode_iterations,
                        i32::from(level),
                        false,
                        &case.input,
                    )
                },
            );
            let upstream_encode_mib_s = mib_per_s(
                case.input.len() as u64 * encode_iterations as u64,
                upstream_encode_metrics.elapsed_ns,
            );
            assert_eq!(
                upstream_encode_metrics.last_output_size,
                upstream_encoded.len(),
                "upstream encoded size drifted during benchmark for {} level {}",
                case.name,
                level
            );
            let upstream_decode_probe =
                benchmark_mode(helper, modes.bench_decode_mode, 1, &upstream_encoded);
            let decode_iterations =
                iterations_for_budget(upstream_decode_probe.elapsed_ns, config.trial_budget);
            let upstream_decode_metrics = upstream_best_of(
                (decode_iterations == 1).then_some(upstream_decode_probe),
                || {
                    benchmark_mode(
                        helper,
                        modes.bench_decode_mode,
                        decode_iterations,
                        &upstream_encoded,
                    )
                },
            );
            let upstream_decode_mib_s = mib_per_s(
                upstream_decode_metrics.total_output_size,
                upstream_decode_metrics.elapsed_ns,
            );
            assert_eq!(
                upstream_decode_metrics.last_output_size,
                case.input.len(),
                "upstream decoded size drifted during benchmark for {} level {}",
                case.name,
                level
            );

            let rust_decode_start = Instant::now();
            let rust_decode = rust_decode_case(&upstream_encoded, prepared_decoding_dictionary);
            let rust_decode_probe_ns = rust_decode_start.elapsed().as_nanos();
            let (rust_decode_status, rust_decode_mib_s) = match rust_decode {
                Ok(decoded) if decoded == case.input => {
                    rust_decode_ok_rows += 1;
                    let rust_decode_mib_s = rust_decode_mib_per_s(
                        &upstream_encoded,
                        prepared_decoding_dictionary,
                        iterations_for_budget(rust_decode_probe_ns, config.trial_budget),
                    );
                    if speed_ratio(rust_decode_mib_s, upstream_decode_mib_s) < 0.5 {
                        decode_fail_rows.push(format!(
                            "{} L{} decode {:.2}x ({:.2} / {:.2} MiB/s)",
                            case.name,
                            level,
                            speed_ratio(rust_decode_mib_s, upstream_decode_mib_s),
                            rust_decode_mib_s,
                            upstream_decode_mib_s
                        ));
                    }
                    ("ok".to_string(), Some(rust_decode_mib_s))
                }
                Ok(_) => ("mismatch".to_string(), None),
                Err(error) => (format!("{error:?}"), None),
            };

            let (rust_encode_status, rust_ratio, rust_encode_mib_s) = if i32::from(level)
                <= CompressionLevel::MAX.as_i32()
            {
                let rust_encode_start = Instant::now();
                let rust_encoded = rust_encode_case(case, prepared_dictionary, level);
                let rust_encode_probe_ns = rust_encode_start.elapsed().as_nanos();
                let upstream_decoded =
                    decompress_once(helper, modes.decompress_mode, &rust_encoded);
                assert_eq!(
                    upstream_decoded, case.input,
                    "upstream failed to decode Rust output for {} level {}",
                    case.name, level
                );
                let rust_roundtrip = rust_decode_case(&rust_encoded, prepared_decoding_dictionary)
                    .expect("Rust should decode its own output");
                assert_eq!(
                    rust_roundtrip, case.input,
                    "Rust failed to roundtrip its own output for {} level {}",
                    case.name, level
                );
                rust_encode_ok_rows += 1;
                one_shot_bytes.insert(level, rust_encoded.len());
                let rust_ratio_value = rust_encoded.len() as f64 / case.input.len() as f64;
                let rust_encode_mib_s = rust_encode_mib_per_s(
                    case,
                    prepared_dictionary,
                    level,
                    iterations_for_budget(rust_encode_probe_ns, config.trial_budget),
                );
                if speed_ratio(rust_encode_mib_s, upstream_encode_mib_s) < 0.5 {
                    encode_fail_rows.push(format!(
                        "{} L{} encode {:.2}x ({:.2} / {:.2} MiB/s)",
                        case.name,
                        level,
                        speed_ratio(rust_encode_mib_s, upstream_encode_mib_s),
                        rust_encode_mib_s,
                        upstream_encode_mib_s
                    ));
                }
                if rust_encoded.len() > upstream_encoded.len() {
                    ratio_regressions.push(RatioRegression {
                        case: case.name,
                        level,
                        rust_bytes: rust_encoded.len(),
                        upstream_bytes: upstream_encoded.len(),
                    });
                }
                (
                    "ok".to_string(),
                    Some(rust_ratio_value),
                    Some(rust_encode_mib_s),
                )
            } else {
                ("unsupported".to_string(), None, None)
            };

            speed_rows.push(SpeedRow {
                case: case.name,
                level,
                encode: rust_encode_mib_s.map(|rust| speed_ratio(rust, upstream_encode_mib_s)),
                decode: rust_decode_mib_s.map(|rust| speed_ratio(rust, upstream_decode_mib_s)),
            });

            writeln!(
                &mut report,
                "| {} | {} | {} | {} | {:.4} | {} | {:.2} | {} | {:.2} |",
                level,
                rust_encode_status,
                rust_decode_status,
                format_optional_ratio(rust_ratio),
                upstream_ratio,
                format_optional_speed(rust_encode_mib_s),
                upstream_encode_mib_s,
                format_optional_speed(rust_decode_mib_s),
                upstream_decode_mib_s,
            )
            .unwrap();
        }

        eprintln!("  timed section {:.1}s", case_start.elapsed().as_secs_f64());
        // Filled by the streaming pass once every timed row above is recorded.
        if case.dict_kind == DictKind::None {
            writeln!(&mut report, "__STREAMING_{}__", case.name).unwrap();
        }
        one_shot_by_case.insert(case.name, one_shot_bytes);

        if should_profile_case(case) && !stage_levels.is_empty() {
            // Two caches, because the planner's phase timers cannot be on
            // while the stage totals are being read. See `PlannerPhases`.
            let mut stage_profiles = HashMap::new();
            let mut phase_profiles = HashMap::new();
            writeln!(&mut report, "### Rust First-Block Stage Timing").unwrap();
            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "- Samples the first raw `block_size` chunk only, so the timing breakdown stays aligned with the real block-local hot path."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- Uses prepared dictionaries for dictionary-backed cases so the sample reflects encoder hot paths instead of repeated dictionary parsing."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- The stage table above is sampled with the planner's phase timers off, so its milliseconds and its shares are both the real encoder's. The two sub-breakdown tables below need those timers and are sampled separately, because a timer taken per lazy parser step costs far more than the step: with them on, this case's first block reads up to 18x its real time and 99% of the frame lands in `Plan`. Read the sub-breakdowns as shares of their own row and never against the table above."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- The planning sub-breakdown covers row and chain/extdict lazy paths; other planner families may still report zeros. The lazy parser phase sub-breakdown is instrumented for no-dict row and trained-dictionary chain/extdict cases, and likewise reports zeros elsewhere."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- Sampled on levels {} over {} iterations.",
                format_levels(&stage_levels),
                stage_iterations
            )
            .unwrap();
            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "| Level | Sampled ms | Blocks | Compressed | Split % | Plan % | Lit % | Seq % | Other % |"
            )
            .unwrap();
            writeln!(
                &mut report,
                "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            )
            .unwrap();

            for &level in &stage_levels {
                let profile = *stage_profiles.entry(level).or_insert_with(|| {
                    rust_stage_profile(
                        case,
                        &raw_prepared,
                        &trained_prepared,
                        level,
                        stage_iterations,
                        PlannerPhases::Off,
                    )
                });
                writeln!(
                    &mut report,
                    "| {} | {:.2} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                    level,
                    duration_ms(profile.total),
                    profile.blocks / stage_iterations.max(1),
                    profile.compressed_blocks / stage_iterations.max(1),
                    share_percent(profile.block_split, profile.total),
                    share_percent(profile.planning, profile.total),
                    share_percent(profile.literals, profile.total),
                    share_percent(profile.sequences, profile.total),
                    share_percent(stage_other(profile), profile.total),
                )
                .unwrap();
            }

            writeln!(&mut report).unwrap();
            writeln!(&mut report, "### Rust First-Block Decode Timing").unwrap();
            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- Sampled on levels {} over {} iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.",
                format_levels(&stage_levels),
                stage_iterations
            )
            .unwrap();
            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |"
            )
            .unwrap();
            writeln!(
                &mut report,
                "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            )
            .unwrap();
            for &level in &stage_levels {
                let profile = rust_decode_stage_profile(
                    helper,
                    case,
                    &raw_decoding,
                    &trained_decoding,
                    level,
                    stage_iterations,
                );
                writeln!(
                    &mut report,
                    "| {} | {:.2} | {} | {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                    level,
                    duration_ms(profile.total),
                    profile.blocks / stage_iterations.max(1),
                    profile.compressed_blocks / stage_iterations.max(1),
                    share_percent(profile.literals, profile.total),
                    share_percent(profile.sequence_tables, profile.total),
                    share_percent(profile.sequence_commands, profile.total),
                    share_percent(profile.sequence_execute, profile.total),
                    share_percent(decode_stage_other(profile), profile.total),
                )
                .unwrap();
            }

            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |"
            )
            .unwrap();
            writeln!(&mut report, "| ---: | ---: | ---: | ---: | ---: |").unwrap();
            for &level in &stage_levels {
                let profile = rust_decode_stage_profile(
                    helper,
                    case,
                    &raw_decoding,
                    &trained_decoding,
                    level,
                    stage_iterations,
                );
                writeln!(
                    &mut report,
                    "| {} | {:.1} | {:.1} | {:.1} | {:.1} |",
                    level,
                    share_percent(
                        profile.sequence_execute_literal_copy,
                        profile.sequence_execute
                    ),
                    share_percent(
                        profile.sequence_execute_prefix_match_copy,
                        profile.sequence_execute
                    ),
                    share_percent(
                        profile.sequence_execute_dictionary_match_copy,
                        profile.sequence_execute
                    ),
                    share_percent(decode_execute_other(profile), profile.sequence_execute),
                )
                .unwrap();
            }

            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |"
            )
            .unwrap();
            writeln!(
                &mut report,
                "| ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            )
            .unwrap();
            for &level in &stage_levels {
                let profile = *phase_profiles.entry(level).or_insert_with(|| {
                    rust_stage_profile(
                        case,
                        &raw_prepared,
                        &trained_prepared,
                        level,
                        stage_iterations,
                        PlannerPhases::On,
                    )
                });
                writeln!(
                    &mut report,
                    "| {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                    level,
                    share_percent(profile.planning_row_search, profile.planning),
                    share_percent(profile.planning_chain_search, profile.planning),
                    share_percent(profile.planning_match_count, profile.planning),
                    share_percent(profile.planning_rep_check, profile.planning),
                    share_percent(profile.planning_insert_update, profile.planning),
                    share_percent(profile.planning_parser, profile.planning),
                )
                .unwrap();
            }

            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |"
            )
            .unwrap();
            writeln!(
                &mut report,
                "| ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            )
            .unwrap();
            for &level in &stage_levels {
                let profile = *phase_profiles.entry(level).or_insert_with(|| {
                    rust_stage_profile(
                        case,
                        &raw_prepared,
                        &trained_prepared,
                        level,
                        stage_iterations,
                        PlannerPhases::On,
                    )
                });
                writeln!(
                    &mut report,
                    "| {} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} | {:.1} |",
                    level,
                    share_percent(
                        profile.planning_row_parser_baseline_rep,
                        profile.planning_parser
                    ),
                    share_percent(
                        profile.planning_row_parser_baseline_regular,
                        profile.planning_parser
                    ),
                    share_percent(
                        profile.planning_row_parser_continue,
                        profile.planning_parser
                    ),
                    share_percent(profile.planning_row_parser_store, profile.planning_parser),
                    share_percent(profile.planning_row_parser_rep2, profile.planning_parser),
                    share_percent(stage_row_parser_other(profile), profile.planning_parser),
                )
                .unwrap();
            }

            writeln!(&mut report).unwrap();
            writeln!(&mut report, "### Rust First-Block Parser Stats").unwrap();
            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "- One-shot trace of the first block at each level. Sequence counts, byte breakdowns, and repcode usage come from the parser trace, not from timing."
            )
            .unwrap();
            writeln!(
                &mut report,
                "- Sampled on levels {}.",
                format_levels(&stage_levels)
            )
            .unwrap();
            writeln!(&mut report).unwrap();
            writeln!(
                &mut report,
                "| Level | Sequences | C Seqs | Lit bytes | Match bytes | Rep1 | Rep2 | Rep3 | Rep1-1 | Explicit | Avg ML | Avg offset |"
            )
            .unwrap();
            writeln!(
                &mut report,
                "| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
            )
            .unwrap();
            for &level in &stage_levels {
                let trace = rust_first_block_trace(case, &raw_prepared, &trained_prepared, level);
                let c_seqs = c_first_block_sequence_count(helper, case, level);
                let c_seqs_str = c_seqs
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "-".to_string());
                if trace.decision != BlockTraceDecision::Compressed {
                    writeln!(
                        &mut report,
                        "| {} | — | {} | — | — | — | — | — | — | — | — | — |",
                        level, c_seqs_str,
                    )
                    .unwrap();
                    continue;
                }
                let stats = &trace.parser_stats;
                let repcodes = &stats.repcodes;
                let avg_ml = if trace.sequence_count > 0 {
                    stats.matched_bytes as f64 / trace.sequence_count as f64
                } else {
                    0.0
                };
                let avg_offset = if stats.explicit_offset_count > 0 {
                    stats.explicit_offset_sum as f64 / stats.explicit_offset_count as f64
                } else {
                    0.0
                };
                writeln!(
                    &mut report,
                    "| {} | {} | {} | {} | {} | {} | {} | {} | {} | {} | {:.1} | {:.0} |",
                    level,
                    trace.sequence_count,
                    c_seqs_str,
                    stats.literal_bytes,
                    stats.matched_bytes,
                    repcodes.rep1,
                    repcodes.rep2,
                    repcodes.rep3,
                    repcodes.rep1_minus1,
                    repcodes.explicit_offsets,
                    avg_ml,
                    avg_offset,
                )
                .unwrap();
            }

            writeln!(&mut report).unwrap();
        }

        writeln!(&mut report).unwrap();
    }

    // Second pass. Every timed row above is already recorded, so this is the
    // only point at which the report can afford to use the whole machine.
    let streaming_start = Instant::now();
    let measurements = measure_streaming(helper, &cases, &config.levels);
    eprintln!(
        "streaming pass {:.1}s ({} comparisons)",
        streaming_start.elapsed().as_secs_f64(),
        measurements.len()
    );
    for (case_index, case) in cases.iter().enumerate() {
        if case.dict_kind != DictKind::None {
            continue;
        }
        let for_case = measurements
            .iter()
            .filter(|measurement| measurement.case_index == case_index)
            .collect::<Vec<_>>();
        let empty = HashMap::new();
        let section = render_streaming_section(
            &config.levels,
            one_shot_by_case.get(case.name).unwrap_or(&empty),
            &for_case,
            &mut streaming_deltas,
            case.name,
        );
        report = report.replacen(&format!("__STREAMING_{}__\n", case.name), &section, 1);
    }

    let summary = format!(
        "| Metric | Value |\n| --- | --- |\n| Corpus cases | {} |\n| Total case/level rows | {} |\n| Rust encode rows supported | {}/{} |\n| Rust encode rows completed | {} |\n| Rust decode rows completed | {} |\n| Rust encoder level range | {}..={} |\n| Benchmarked levels | {} |\n",
        cases.len(),
        total_rows,
        total_rust_encode_rows,
        total_rows,
        rust_encode_ok_rows,
        rust_decode_ok_rows,
        CompressionLevel::MIN.as_i32(),
        CompressionLevel::MAX.as_i32(),
        format_levels(&config.levels),
    );
    let fail_summary = format_fail_summary(
        &encode_fail_rows,
        &decode_fail_rows,
        &mut ratio_regressions,
        &mut streaming_deltas,
        &speed_rows,
        &cases,
    );
    report = report.replacen("__SUMMARY_TABLE__", &summary, 1);
    report = report.replacen("__FAIL_SUMMARY__", &fail_summary, 1);

    fs::write(&config.output, report).expect("failed to write benchmark report");
    println!("wrote {}", config.output.display());
}

struct Config {
    output: PathBuf,
    input_bytes: usize,
    trial_budget: Duration,
    stage_target_bytes: usize,
    case_filters: Vec<String>,
    levels: Vec<u8>,
}

struct CaseModes {
    compress_mode: &'static str,
    bench_compress_with_output_mode: &'static str,
    bench_decode_mode: &'static str,
    decompress_mode: &'static str,
}

/// One row's throughput against upstream's, kept so the summary can aggregate
/// per case rather than only counting rows past a fixed floor.
///
/// The per-row floor above is 50% and is deliberately loose, because a single
/// row's throughput is noisy enough to straddle a threshold on identical code
/// -- see [`TIMING_TRIALS`], which records three back-to-back sweeps putting
/// one row at 0.48x, 0.45x and 0.51x. Tightening that floor would trade one
/// blind spot for a flaky report.
///
/// Aggregating a whole case is the other way out, and it sees the shape the
/// floor cannot: the dictionary cases sat at 0.62-0.82x across the whole of
/// levels 1-10 while this report said `Encode rows below 50% | 0` -- true, and
/// read for as long as it stood as though there were nothing to find.
///
/// Which aggregate matters, and the obvious one is wrong. A median looks
/// robust and is not, because these cases are *bimodal*: `raw-dictionary` runs
/// at 0.65-0.82x for levels 1 through 10 and 0.95-1.07x above, so its median
/// sits on the boundary between the two clusters and a hair of noise moves it
/// across. Measured, on two consecutive sweeps of identical code: 0.84x then
/// 0.95x. The second reads as a case with nothing wrong with it.
///
/// Counting the levels materially behind does not have that failure *for the
/// cases it exists to catch*, because a band well below the floor is nowhere
/// near a boundary in that measure -- ten rows have to cross it to move the
/// count by ten, and on two consecutive sweeps none did. It also separates the
/// cases far more cleanly: 20, 12, 11 and 10 of 22 for `mixed-entropy`, the two
/// dictionary cases and `pseudorandom`, then 6 and below for every other case.
///
/// It is not noise-free in general, and the claim that it was did not survive
/// its second sweep. A case whose levels cluster just *above* the floor has
/// every one of them a coin flip: `tabular-csv` read 4 then 6, and
/// `small-alphabet` 4 then 2, across identical code. So the count is steady
/// where it matters and unsteady where the numbers are near parity, and the
/// flag has to sit clear of the unsteady part -- see
/// [`SpeedSummary::is_broadly_behind`]. Both are reported, the median for how
/// far behind and the count for how much of the case is, and the count is what
/// the summary flags on. See
/// [`SLOW_LEVEL_FLOOR`] for where the line sits and why.
struct SpeedRow {
    case: &'static str,
    level: u8,
    encode: Option<f64>,
    decode: Option<f64>,
}

/// How far behind a case is, how much of it is, and where to start looking.
struct SpeedSummary {
    median: f64,
    below_floor: usize,
    levels: usize,
    worst: f64,
    worst_level: u8,
}

impl SpeedSummary {
    /// Whether enough of the case is behind to be worth a name, which is the
    /// flag; see [`SpeedRow`] for why this and not the median.
    ///
    /// A third of the levels, not most of them, because the band does not have
    /// to be the majority to be real: `raw-dictionary` is behind on levels 1
    /// through 10 of 22 and level with upstream above, and that is the case
    /// this whole section was added to see.
    ///
    /// A third rather than the quarter this first shipped as, because a
    /// quarter is inside the noise and a third is not. The tally is only as
    /// steady as the rows it counts, and how steady that is depends on where
    /// the case's levels sit: a band well below the floor never crosses it, so
    /// `mixed-entropy` read 20 and `pseudorandom` 11 and `raw-dictionary` 10 on
    /// two consecutive sweeps of identical code, while a case whose levels
    /// cluster just *above* the floor has every one of them a coin flip, and
    /// `tabular-csv` read 4 and then 6 across the same two sweeps and
    /// `small-alphabet` 4 and then 2. Six of 22 clears a quarter, so the
    /// summary count of flagged cases moved with it, from four to five, with
    /// nothing in the library changed.
    ///
    /// Those two sweeps left a gap to put the line in -- 2, 2, 2, 3, 3, 4, 4, 6
    /// and then 10, 11, 12, 20, so a noise floor topping out at six and real
    /// bands starting at ten, with a third of 22 being 8 and sitting between
    /// them. **A third sweep closed that gap**: `log-lines` read 8 after two
    /// sweeps at 4, and `raw-dictionary`'s *decode* count read 16 after 0 and
    /// 2. There is no threshold on this metric that noise cannot reach, and
    /// picking a third over a quarter is choosing between two arbitrary lines,
    /// not fixing anything.
    ///
    /// The line stays here because it is no worse than the alternative, but
    /// **the flag is not a gate and a change of one or two cases between
    /// reports means nothing**. What would fix it is not a threshold: it is the
    /// measurement, which times the two sides of each A/B pair seconds apart --
    /// see [`TIMING_TRIALS`].
    fn is_broadly_behind(&self) -> bool {
        self.below_floor * 3 >= self.levels
    }
}

fn summarize_speed(mut ratios: Vec<(u8, f64)>) -> Option<SpeedSummary> {
    if ratios.is_empty() {
        return None;
    }
    ratios.sort_by(|left, right| left.1.total_cmp(&right.1));
    let middle = ratios.len() / 2;
    let median = if ratios.len().is_multiple_of(2) {
        (ratios[middle - 1].1 + ratios[middle].1) / 2.0
    } else {
        ratios[middle].1
    };
    Some(SpeedSummary {
        median,
        below_floor: ratios
            .iter()
            .filter(|(_, ratio)| *ratio < SLOW_LEVEL_FLOOR)
            .count(),
        levels: ratios.len(),
        worst: ratios[0].1,
        worst_level: ratios[0].0,
    })
}

/// A row where this crate's encoder emitted more bytes than upstream.
///
/// Kept as byte counts rather than ratios. The report prints ratios to four
/// decimal places, so a row one byte larger on a 4 MiB case rendered exactly
/// like one 26% larger: the list carried 62 entries of which 56 were within
/// 1%, and nothing in it distinguished the six that mattered.
struct RatioRegression {
    case: &'static str,
    level: u8,
    rust_bytes: usize,
    upstream_bytes: usize,
}

impl RatioRegression {
    /// How much larger this crate's output is, as a fraction of upstream's.
    fn excess(&self) -> f64 {
        // Only constructed when Rust emitted more, so upstream can only be
        // empty if it compressed a case to nothing, which the corpus never is.
        if self.upstream_bytes == 0 {
            f64::INFINITY
        } else {
            (self.rust_bytes - self.upstream_bytes) as f64 / self.upstream_bytes as f64
        }
    }
}

/// Excess above which a ratio regression is worth acting on rather than noting.
const MATERIAL_RATIO_EXCESS: f64 = 0.01;

/// Piece size fed to both streaming encoders in the per-case streaming table.
///
/// Streaming block layout is a function of how much input arrives per call, so
/// any single value is one sample of a curve rather than a result;
/// [`STREAMING_SENSITIVITY_PIECES`] sweeps around it.
///
/// 32 KiB is deliberately *not* a divisor of [`BLOCK_SIZE`]. It sits in the
/// range a real reader delivers and it exercises the partial-buffer path;
/// aligning the piece to the block max instead would let our layout and
/// upstream's agree for a reason that says nothing about either parser. It is
/// also the value `streaming_block_layout_matches_upstream_streaming` uses, so
/// a divergence here and a failure there are the same measurement.
const STREAMING_PRIMARY_PIECE: usize = 32 * 1024;

/// Piece sizes for the sensitivity table, picked to hit distinct regimes
/// rather than to fill a grid: below the block max, exactly at it, and well
/// above it. The middle one matters most — upstream's buffered streaming path
/// hands its frame-chunk loop one `blockSizeMax` per call, so that is where
/// "a chunk yields at most two blocks" changes character.
const STREAMING_SENSITIVITY_PIECES: [usize; 3] = [16 * 1024, BLOCK_SIZE, 1024 * 1024];

/// Levels sampled by the sensitivity table: Fast, DoubleFast, Lazy2,
/// BinaryTreeLazy2 and BinaryTreeUltra2, rather than an even spread.
///
/// Sweeping all 22 at every piece size costs more than the rest of the
/// streaming pass put together while re-measuring each strategy several times
/// over. Five is enough because this table asks about *piece size*, and how
/// block layout responds to the arrival pattern is very largely a property of
/// the streaming buffer rather than of which parser fills the block. The
/// per-level parser detail lives in the table above it, which sweeps all 22.
const STREAMING_SENSITIVITY_LEVELS: [u8; 5] = [1, 3, 9, 15, 19];

/// One case/level/piece comparison against upstream's streaming encoder.
struct StreamingDelta {
    case: &'static str,
    level: u8,
    piece: usize,
    rust_bytes: usize,
    upstream_bytes: usize,
}

impl StreamingDelta {
    /// Signed, unlike [`RatioRegression::excess`]: positive means this crate
    /// emitted more than upstream, negative means less.
    ///
    /// The one-shot list only collects rows where we are over, which is right
    /// for a regression gate and wrong here. The largest streaming difference
    /// known at the time of writing runs the other way — `next_block_size`
    /// never splits at the optimal strategies while upstream does, worth
    /// -18.56% in our favour on `tabular-csv` L19 — and a gate that could not
    /// see it would report the healthiest-looking table in the document.
    fn delta(&self) -> f64 {
        if self.upstream_bytes == 0 {
            f64::INFINITY
        } else {
            (self.rust_bytes as f64 - self.upstream_bytes as f64) / self.upstream_bytes as f64
        }
    }
}

fn parse_config() -> Config {
    let mut quick = false;
    let mut output = PathBuf::from("BENCHMARKS.md");
    let mut case_filters = Vec::new();
    let mut levels = (1u8..=22).collect::<Vec<_>>();
    let mut args = std::env::args().skip(1);

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => quick = true,
            "--case" => {
                let value = args.next().expect("--case requires a case name");
                case_filters.extend(
                    value
                        .split(',')
                        .map(str::trim)
                        .filter(|name| !name.is_empty())
                        .map(ToOwned::to_owned),
                );
            }
            "--levels" => {
                let value = args.next().expect("--levels requires a level list");
                levels = parse_levels(&value);
            }
            "--output" => {
                let path = args.next().expect("--output requires a path argument");
                output = PathBuf::from(path);
            }
            _ => panic!("unknown argument: {arg}"),
        }
    }

    if quick {
        Config {
            output,
            input_bytes: DEFAULT_INPUT_BYTES,
            trial_budget: QUICK_TRIAL_BUDGET,
            stage_target_bytes: QUICK_STAGE_TARGET_BYTES,
            case_filters,
            levels,
        }
    } else {
        Config {
            output,
            input_bytes: DEFAULT_INPUT_BYTES,
            trial_budget: DEFAULT_TRIAL_BUDGET,
            stage_target_bytes: DEFAULT_STAGE_TARGET_BYTES,
            case_filters,
            levels,
        }
    }
}

fn parse_levels(value: &str) -> Vec<u8> {
    let mut levels = BTreeSet::new();
    for part in value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        if let Some((start, end)) = part.split_once('-') {
            let start = parse_level(start);
            let end = parse_level(end);
            let (start, end) = if start <= end {
                (start, end)
            } else {
                (end, start)
            };
            for level in start..=end {
                levels.insert(level);
            }
            continue;
        }
        levels.insert(parse_level(part));
    }
    assert!(
        !levels.is_empty(),
        "--levels must select at least one compression level"
    );
    levels.into_iter().collect()
}

fn parse_level(value: &str) -> u8 {
    let level = value
        .parse::<u8>()
        .unwrap_or_else(|_| panic!("invalid compression level: {value}"));
    assert!(
        (1..=22).contains(&level),
        "compression level out of range 1..=22: {level}"
    );
    level
}

fn filter_cases(mut cases: Vec<CorpusCase>, filters: &[String]) -> Vec<CorpusCase> {
    if filters.is_empty() {
        return cases;
    }

    let selected = filters.iter().map(String::as_str).collect::<BTreeSet<_>>();
    cases.retain(|case| selected.contains(case.name));
    assert!(
        !cases.is_empty(),
        "no benchmark cases matched --case filters: {}",
        filters.join(",")
    );
    cases
}

fn format_levels(levels: &[u8]) -> String {
    let mut parts = Vec::new();
    let mut start = None;
    let mut prev = 0u8;

    for &level in levels {
        match start {
            None => {
                start = Some(level);
                prev = level;
            }
            Some(run_start) if level == prev.saturating_add(1) => {
                prev = level;
                start = Some(run_start);
            }
            Some(run_start) => {
                parts.push(format_level_range(run_start, prev));
                start = Some(level);
                prev = level;
            }
        }
    }

    if let Some(run_start) = start {
        parts.push(format_level_range(run_start, prev));
    }

    parts.join(",")
}

fn format_level_range(start: u8, end: u8) -> String {
    if start == end {
        start.to_string()
    } else {
        format!("{start}-{end}")
    }
}

/// `git describe --tags --always --dirty` for a checkout, or `None` when the
/// directory is absent or git declines to describe it. A report is still worth
/// emitting without a revision label, so every failure here is non-fatal.
fn git_describe(dir: &Path) -> Option<String> {
    git_describe_with(dir, &["describe", "--tags", "--always", "--dirty"])
}

/// As [`git_describe`], but naming the commit only. The caller decides what
/// counts as a dirty tree; see [`crate_revision`].
fn git_describe_commit(dir: &Path) -> Option<String> {
    git_describe_with(dir, &["describe", "--tags", "--always"])
}

fn git_describe_with(dir: &Path, args: &[&str]) -> Option<String> {
    if !dir.exists() {
        return None;
    }
    let describe = Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(args)
        .output()
        .ok()?;
    if !describe.status.success() {
        return None;
    }
    let label = String::from_utf8(describe.stdout).ok()?.trim().to_string();
    if label.is_empty() { None } else { Some(label) }
}

fn upstream_zstd_reference() -> Option<String> {
    git_describe(&upstream_zstd::upstream_dir())
}

/// Every tracked path with uncommitted changes, relative to `dir`.
///
/// `None` when git cannot answer, which the caller treats as "no evidence of
/// dirt" rather than guessing.
fn dirty_paths(dir: &Path) -> Option<Vec<PathBuf>> {
    let status = Command::new("git")
        .args(["-C"])
        .arg(dir)
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()?;
    if !status.status.success() {
        return None;
    }
    Some(
        String::from_utf8(status.stdout)
            .ok()?
            .lines()
            .filter_map(|line| line.get(3..))
            // A rename reports `old -> new`; the new name is the working copy.
            .map(|path| path.rsplit(" -> ").next().unwrap_or(path))
            .map(|path| dir.join(path.trim_matches('"')))
            .collect(),
    )
}

/// The revision of this crate that produced the report. Pinning upstream alone
/// says what the numbers were measured *against*, not what they were measured
/// *from*: a report regenerated three commits ago looks identical to a current
/// one. `-dirty` also marks numbers produced from an uncommitted tree, which
/// no commit can identify afterwards.
///
/// The report itself is excluded from that check, because writing it is what
/// dirties the tree. A regeneration over an uncommitted previous run would
/// otherwise stamp `-dirty` on a report whose only uncommitted change was the
/// report, and the flag means nothing once it is always set. Getting a truthful
/// stamp used to require committing the previous run first, which is backwards:
/// it asks you to commit numbers in order to replace them.
fn crate_revision(output: &Path) -> Option<String> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let label = git_describe_commit(dir)?;
    let output = output.canonicalize().ok();
    let dirty = dirty_paths(dir).is_some_and(|paths| {
        paths.iter().any(|path| {
            let path = path.canonicalize().ok();
            // An unreadable path cannot be the output, which exists.
            path.is_none() || path != output
        })
    });
    Some(if dirty {
        format!("{label}-dirty")
    } else {
        label
    })
}

fn write_header(report: &mut String, config: &Config, case_count: usize) {
    writeln!(report, "# Benchmark Report").unwrap();
    writeln!(report).unwrap();
    writeln!(
        report,
        "This report compares the local `zstandard` checkout against the official `zstd` implementation at the revision pinned in `upstream-zstd.ref`. Numbers measured against any other revision are not comparable: upstream changes its level mapping, parser heuristics, and block splitter between releases."
    )
    .unwrap();
    writeln!(report).unwrap();
    writeln!(report, "Notes:").unwrap();
    writeln!(
        report,
        "- Rust encode is benchmarked for the currently supported public compression levels `1..=22`."
    )
    .unwrap();
    writeln!(
        report,
        "- Upstream encode is benchmarked for levels `1..=22`."
    )
    .unwrap();
    writeln!(
        report,
        "- Rust decode and upstream decode are benchmarked on the same upstream-produced frame for each row."
    )
    .unwrap();
    writeln!(
        report,
        "- Dictionary cases use the deterministic raw and trained dictionary fixtures emitted by the upstream helper."
    )
    .unwrap();
    writeln!(
        report,
        "- Throughput is machine-local and depends on the current host; use this file for relative comparison, not cross-machine claims."
    )
    .unwrap();
    writeln!(
        report,
        "- Each throughput number is the fastest of {TIMING_TRIALS} timing trials, since background load can only make a trial slower. Ratios are exact byte counts and reproduce exactly; throughput does not. The two sides of each comparison are timed seconds apart inside a ten-minute sweep, so a case's throughput carries drift from wherever it sits in the run: across three sweeps of identical code one case's slow-level count read 4, 4 and 8, and another's decode count 0, 2 and 16. Read the throughput columns as indicative, not as a gate, and do not act on a change of one or two."
    )
    .unwrap();
    writeln!(
        report,
        "- Each trial runs as many iterations as fit its time budget, sized per row from a single probe iteration, so a fast row is measured over a longer loop rather than a slower one being measured more times. Reports generated before this replaced a fixed byte target read low levels over windows of a few milliseconds; their fast-level throughput is measured over too short an interval to compare against numbers here."
    )
    .unwrap();
    writeln!(report).unwrap();
    writeln!(report, "| Setting | Value |").unwrap();
    writeln!(report, "| --- | --- |").unwrap();
    writeln!(report, "| Output file | `{}` |", config.output.display()).unwrap();
    writeln!(report, "| Corpus cases | {} |", case_count).unwrap();
    writeln!(report, "| Input bytes per case | {} |", config.input_bytes).unwrap();
    writeln!(
        report,
        "| Benchmarked levels | `{}` |",
        format_levels(&config.levels)
    )
    .unwrap();
    writeln!(
        report,
        "| Case filters | {} |",
        if config.case_filters.is_empty() {
            "all".to_string()
        } else {
            config.case_filters.join(", ")
        }
    )
    .unwrap();
    writeln!(report, "| Block size | {} |", BLOCK_SIZE).unwrap();
    writeln!(
        report,
        "| Streaming piece size | {} |",
        format_piece(STREAMING_PRIMARY_PIECE)
    )
    .unwrap();
    writeln!(
        report,
        "| Streaming sensitivity pieces | {} |",
        STREAMING_SENSITIVITY_PIECES
            .iter()
            .map(|&piece| format_piece(piece))
            .collect::<Vec<_>>()
            .join(", ")
    )
    .unwrap();
    if let Some(revision) = crate_revision(&config.output) {
        writeln!(report, "| zstandard revision | `{}` |", revision).unwrap();
    }
    if let Some(reference) = upstream_zstd_reference() {
        writeln!(report, "| Upstream zstd reference | `{}` |", reference).unwrap();
    }
    writeln!(
        report,
        "| Timing trial budget | {} ms |",
        config.trial_budget.as_millis()
    )
    .unwrap();
    writeln!(
        report,
        "| Iterations per trial | {}-{}, sized per row |",
        MIN_ITERATIONS, MAX_ITERATIONS
    )
    .unwrap();
    writeln!(
        report,
        "| Stage profiling target bytes | {} |",
        config.stage_target_bytes
    )
    .unwrap();
    writeln!(
        report,
        "| Timing trials per row (fastest reported) | {} |",
        TIMING_TRIALS
    )
    .unwrap();
    writeln!(report).unwrap();
    writeln!(report, "## Coverage Summary").unwrap();
    writeln!(report).unwrap();
    writeln!(report, "__SUMMARY_TABLE__").unwrap();
    writeln!(report).unwrap();
    writeln!(report, "## Target Gaps").unwrap();
    writeln!(report).unwrap();
    writeln!(report, "__FAIL_SUMMARY__").unwrap();
    writeln!(report).unwrap();
}

fn speed_ratio(rust: f64, upstream: f64) -> f64 {
    if upstream == 0.0 {
        0.0
    } else {
        rust / upstream
    }
}

/// A level slower than this fraction of upstream counts against its case.
///
/// Not a gate on any single row: one row here means nothing, and the per-row
/// floor above is where a row is judged. This decides only whether a *level*
/// counts toward the tally that flags a case.
///
/// Chosen from the measured spread rather than picked round. At 0.90 the cases
/// separate into ten to twenty slow levels and two to five, with nothing in
/// between -- `mixed-entropy` 20, the two dictionary cases and `pseudorandom`
/// 10, then `log-lines` and `small-alphabet` at 5 and everything else below.
/// At 0.95 that gap closes to 12 against 8 and the tally starts counting rows
/// that are merely near parity: `raw-dictionary` gains six levels sitting
/// exactly on 0.95, which are the rows most likely to cross it on noise. The
/// threshold has to sit clear of parity for the count to mean "behind".
const SLOW_LEVEL_FLOOR: f64 = 0.90;

fn format_fail_summary(
    encode_fail_rows: &[String],
    decode_fail_rows: &[String],
    ratio_regressions: &mut [RatioRegression],
    streaming_deltas: &mut [StreamingDelta],
    speed_rows: &[SpeedRow],
    cases: &[CorpusCase],
) -> String {
    // Largest relative excess first, so the rows worth acting on lead the list
    // instead of being buried among ties. Case and level break ties to keep
    // successive reports diffable.
    ratio_regressions.sort_by(|left, right| {
        right
            .excess()
            .total_cmp(&left.excess())
            .then_with(|| left.case.cmp(right.case))
            .then_with(|| left.level.cmp(&right.level))
    });
    let material = ratio_regressions
        .iter()
        .filter(|regression| regression.excess() > MATERIAL_RATIO_EXCESS)
        .count();

    let mut out = String::new();
    writeln!(&mut out, "| Metric | Value |").unwrap();
    writeln!(&mut out, "| --- | --- |").unwrap();
    writeln!(
        &mut out,
        "| Encode rows below 50% | {} |",
        encode_fail_rows.len()
    )
    .unwrap();
    writeln!(
        &mut out,
        "| Decode rows below 50% | {} |",
        decode_fail_rows.len()
    )
    .unwrap();
    writeln!(
        &mut out,
        "| Ratio regressions | {} |",
        ratio_regressions.len()
    )
    .unwrap();
    writeln!(
        &mut out,
        "| Ratio regressions above {:.0}% | {} |",
        MATERIAL_RATIO_EXCESS * 100.0,
        material
    )
    .unwrap();

    let case_speeds = cases
        .iter()
        .map(|case| {
            let encode = summarize_speed(
                speed_rows
                    .iter()
                    .filter(|row| row.case == case.name)
                    .filter_map(|row| row.encode.map(|ratio| (row.level, ratio)))
                    .collect(),
            );
            let decode = summarize_speed(
                speed_rows
                    .iter()
                    .filter(|row| row.case == case.name)
                    .filter_map(|row| row.decode.map(|ratio| (row.level, ratio)))
                    .collect(),
            );
            (case.name, encode, decode)
        })
        .collect::<Vec<_>>();
    let broadly_behind = |summary: &Option<SpeedSummary>| {
        summary
            .as_ref()
            .is_some_and(SpeedSummary::is_broadly_behind)
    };
    let slow_encode_cases = case_speeds
        .iter()
        .filter(|(_, encode, _)| broadly_behind(encode))
        .count();
    let slow_decode_cases = case_speeds
        .iter()
        .filter(|(_, _, decode)| broadly_behind(decode))
        .count();
    writeln!(
        &mut out,
        "| Cases behind upstream on a third of encode levels | {} |",
        slow_encode_cases
    )
    .unwrap();
    writeln!(
        &mut out,
        "| Cases behind upstream on a third of decode levels | {} |",
        slow_decode_cases
    )
    .unwrap();

    // Streaming is counted in both directions. A row where we emit fewer bytes
    // than upstream is not automatically good news -- it can mean we declined a
    // split upstream took -- so hiding it the way the one-shot list does would
    // make the largest known streaming difference invisible.
    streaming_deltas.sort_by(|left, right| {
        right
            .delta()
            .total_cmp(&left.delta())
            .then_with(|| left.case.cmp(right.case))
            .then_with(|| left.level.cmp(&right.level))
            .then_with(|| left.piece.cmp(&right.piece))
    });
    let streaming_over = streaming_deltas
        .iter()
        .filter(|delta| delta.rust_bytes > delta.upstream_bytes)
        .count();
    let streaming_material_over = streaming_deltas
        .iter()
        .filter(|delta| delta.delta() > MATERIAL_RATIO_EXCESS)
        .count();
    let streaming_material_under = streaming_deltas
        .iter()
        .filter(|delta| delta.delta() < -MATERIAL_RATIO_EXCESS)
        .count();
    writeln!(
        &mut out,
        "| Streaming rows above upstream | {} |",
        streaming_over
    )
    .unwrap();
    writeln!(
        &mut out,
        "| Streaming rows above upstream by {:.0}% | {} |",
        MATERIAL_RATIO_EXCESS * 100.0,
        streaming_material_over
    )
    .unwrap();
    writeln!(
        &mut out,
        "| Streaming rows below upstream by {:.0}% | {} |",
        MATERIAL_RATIO_EXCESS * 100.0,
        streaming_material_under
    )
    .unwrap();
    writeln!(&mut out).unwrap();

    writeln!(&mut out, "### Throughput by Case").unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "This crate's throughput as a fraction of upstream's, summarized over the benchmarked levels. Above 1.00x is this crate being faster."
    )
    .unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "The `slow` column counts the case's levels below {:.0}% of upstream, and it is the one to read. A single row's throughput moves a tenth between sweeps of identical code, so no one row means anything here; the median is worth less than it looks too, because a case split into a slow band and a fast one has its median on the boundary between them and crosses it on noise. The worst column says which level to open first once `slow` has flagged the case.",
        SLOW_LEVEL_FLOOR * 100.0
    )
    .unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "| Case | Encode slow | Encode median | Encode worst | Level | Decode slow | Decode median | Decode worst | Level |"
    )
    .unwrap();
    writeln!(
        &mut out,
        "| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |"
    )
    .unwrap();
    for (name, encode, decode) in &case_speeds {
        let cell = |summary: &Option<SpeedSummary>| match summary {
            Some(summary) => (
                format!("{}/{}", summary.below_floor, summary.levels),
                format!("{:.2}x", summary.median),
                format!("{:.2}x", summary.worst),
                summary.worst_level.to_string(),
            ),
            None => (
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
                "-".to_string(),
            ),
        };
        let (encode_slow, encode_median, encode_worst, encode_level) = cell(encode);
        let (decode_slow, decode_median, decode_worst, decode_level) = cell(decode);
        writeln!(
            &mut out,
            "| {} | {} | {} | {} | {} | {} | {} | {} | {} |",
            name,
            encode_slow,
            encode_median,
            encode_worst,
            encode_level,
            decode_slow,
            decode_median,
            decode_worst,
            decode_level
        )
        .unwrap();
    }
    writeln!(&mut out).unwrap();

    if !encode_fail_rows.is_empty() {
        writeln!(&mut out, "### Encode Rows Below 50%").unwrap();
        writeln!(&mut out).unwrap();
        for row in encode_fail_rows {
            writeln!(&mut out, "- {}", row).unwrap();
        }
        writeln!(&mut out).unwrap();
    }
    if !decode_fail_rows.is_empty() {
        writeln!(&mut out, "### Decode Rows Below 50%").unwrap();
        writeln!(&mut out).unwrap();
        for row in decode_fail_rows {
            writeln!(&mut out, "- {}", row).unwrap();
        }
        writeln!(&mut out).unwrap();
    }
    if !ratio_regressions.is_empty() {
        writeln!(&mut out, "### Ratio Regressions").unwrap();
        writeln!(&mut out).unwrap();
        writeln!(
            &mut out,
            "Rows where this crate emitted more bytes than upstream, largest relative excess first. The comparison is on exact byte counts: most of these rows differ by a handful of bytes on a multi-megabyte case and are listed for completeness, not as defects."
        )
        .unwrap();
        writeln!(&mut out).unwrap();
        for regression in ratio_regressions {
            writeln!(
                &mut out,
                "- {} L{} +{:.2}% ({} vs {} bytes, +{})",
                regression.case,
                regression.level,
                regression.excess() * 100.0,
                regression.rust_bytes,
                regression.upstream_bytes,
                regression.rust_bytes - regression.upstream_bytes
            )
            .unwrap();
        }
        writeln!(&mut out).unwrap();
    }

    let notable = streaming_deltas
        .iter()
        .filter(|delta| delta.delta().abs() > MATERIAL_RATIO_EXCESS)
        .collect::<Vec<_>>();
    if !notable.is_empty() {
        writeln!(
            &mut out,
            "### Streaming Size Deltas Above {:.0}%",
            MATERIAL_RATIO_EXCESS * 100.0
        )
        .unwrap();
        writeln!(&mut out).unwrap();
        writeln!(
            &mut out,
            "Signed against upstream's streaming encoder at the same piece size, largest excess first. Negative rows are this crate emitting fewer bytes; they are listed because a streaming size difference in either direction is a block-layout difference, and the direction alone does not say which implementation made the better choice."
        )
        .unwrap();
        writeln!(&mut out).unwrap();
        for delta in notable {
            writeln!(
                &mut out,
                "- {} L{} piece {} {:+.2}% ({} vs {} bytes)",
                delta.case,
                delta.level,
                format_piece(delta.piece),
                delta.delta() * 100.0,
                delta.rust_bytes,
                delta.upstream_bytes,
            )
            .unwrap();
        }
        writeln!(&mut out).unwrap();
    }

    out
}

fn prepared_dictionary<'a>(
    dict_kind: DictKind,
    raw_prepared: &'a EncoderDictionary<'_>,
    trained_prepared: &'a EncoderDictionary<'_>,
) -> Option<&'a EncoderDictionary<'a>> {
    match dict_kind {
        DictKind::None => None,
        DictKind::Raw => Some(raw_prepared),
        DictKind::Trained => Some(trained_prepared),
    }
}

fn prepared_decoding_dictionary<'a>(
    dict_kind: DictKind,
    raw_decoding: &'a DecoderDictionary<'_>,
    trained_decoding: &'a DecoderDictionary<'_>,
) -> Option<&'a DecoderDictionary<'a>> {
    match dict_kind {
        DictKind::None => None,
        DictKind::Raw => Some(raw_decoding),
        DictKind::Trained => Some(trained_decoding),
    }
}

/// The dictionary a case encodes against, or `None` when it encodes without one.
fn dictionary_bytes<'a>(
    dict_kind: DictKind,
    raw_dictionary: &'a [u8],
    trained_dictionary: &'a [u8],
) -> Option<&'a [u8]> {
    match dict_kind {
        DictKind::None => None,
        DictKind::Raw => Some(raw_dictionary),
        DictKind::Trained => Some(trained_dictionary),
    }
}

fn case_modes(dict_kind: DictKind) -> CaseModes {
    match dict_kind {
        DictKind::None => CaseModes {
            compress_mode: "compress-regular-configured",
            bench_compress_with_output_mode: "bench-compress-regular-with-output",
            bench_decode_mode: "bench-decompress",
            decompress_mode: "decompress",
        },
        DictKind::Raw => CaseModes {
            compress_mode: "compress-raw-dict-configured",
            bench_compress_with_output_mode: "bench-compress-raw-dict-with-output",
            bench_decode_mode: "bench-decompress-raw-dict",
            decompress_mode: "decompress-raw-dict",
        },
        DictKind::Trained => CaseModes {
            compress_mode: "compress-trained-dict-configured",
            bench_compress_with_output_mode: "bench-compress-trained-dict-with-output",
            bench_decode_mode: "bench-decompress-trained-dict",
            decompress_mode: "decompress-trained-dict",
        },
    }
}

/// Runs an upstream helper timing `TIMING_TRIALS` times, keeping the fastest.
///
/// The helper times its own iteration loop, so the trials have to be separate
/// invocations of it. See [`TIMING_TRIALS`] for why the fastest is the one
/// worth reporting.
/// Fastest of [`TIMING_TRIALS`] samples, optionally counting the probe as one
/// of them.
///
/// The probe that sized these trials is only admissible as a sample when it
/// ran the same number of iterations they will — that is, when the budget
/// already asked for one. Then it is not an approximation of a trial, it is
/// the identical call, and re-running it would re-measure what has already
/// been measured. That case is exactly the slow rows, which is where the sweep
/// spends its time.
///
/// When the budget asks for more than one iteration the probe must be
/// discarded. It timed a single pass where each trial times many, and taking a
/// minimum across samples of different widths would let the narrowest and
/// noisiest one win on merit it does not have.
fn upstream_best_of(
    seed: Option<BenchTiming>,
    mut trial: impl FnMut() -> BenchTiming,
) -> BenchTiming {
    let mut best = match seed {
        Some(timing) => timing,
        None => trial(),
    };
    for _ in 1..TIMING_TRIALS {
        let next = trial();
        if next.elapsed_ns < best.elapsed_ns {
            best = next;
        }
    }
    best
}

/// Same, for the helper mode that also returns what it compressed.
///
/// Every trial must agree with `expected`, the output of the probe that sized
/// them. They are the same binary at the pinned revision compressing the same
/// bytes at the same level, so disagreement would mean the reference is not
/// reproducible and no row in this report could be compared against a later
/// run.
fn upstream_encode_best_of(
    expected: &[u8],
    seed: Option<BenchTiming>,
    mut trial: impl FnMut() -> BenchCompressOutput,
) -> BenchTiming {
    // See [`upstream_best_of`] for when a probe may stand in for a trial.
    let mut best = match seed {
        Some(timing) => timing,
        None => agreeing_timing(trial(), expected),
    };
    for _ in 1..TIMING_TRIALS {
        let next = agreeing_timing(trial(), expected);
        if next.elapsed_ns < best.elapsed_ns {
            best = next;
        }
    }
    best
}

fn agreeing_timing(output: BenchCompressOutput, expected: &[u8]) -> BenchTiming {
    assert_eq!(
        output.encoded, expected,
        "upstream produced different output for the same input across timing trials"
    );
    output.timing
}

/// Iterations per timing trial for the stage-profiling tables.
///
/// Stage profiling divides accumulated counters by a single iteration count
/// shared across its whole table, and runs only on levels 3-7, which are about
/// 1% of a sweep. It is sized by bytes for that reason; the throughput columns,
/// which are the expensive part, use [`iterations_for_budget`] instead.
fn choose_iterations(target_bytes: usize, unit_bytes: usize) -> usize {
    let unit_bytes = unit_bytes.max(1);
    let iterations = target_bytes / unit_bytes / TIMING_TRIALS;
    iterations.clamp(3, 128)
}

/// Iterations per timing trial, sized from a probe of a single iteration so
/// that one trial takes roughly `budget`.
///
/// The probe is never extra work. On all four sides of a row it is an operation
/// the report already had to perform — the upstream encode that produces the
/// bytes every other column is measured against, the upstream decode of them,
/// and the two Rust correctness runs — so sizing costs only the timing of
/// something already being run.
///
/// The two Rust probes are cold, allocating a fresh buffer where the timing
/// loop reuses one, so they overestimate the steady-state cost and this returns
/// slightly fewer iterations than the budget would allow. That only matters at
/// the fast end, where the count saturates at [`MAX_ITERATIONS`] regardless.
fn iterations_for_budget(single_ns: u128, budget: Duration) -> usize {
    let single_ns = single_ns.max(1);
    let iterations = usize::try_from(budget.as_nanos() / single_ns).unwrap_or(MAX_ITERATIONS);
    iterations.clamp(MIN_ITERATIONS, MAX_ITERATIONS)
}

fn rust_encode_case(
    case: &CorpusCase,
    dictionary: Option<&EncoderDictionary<'_>>,
    level: u8,
) -> Vec<u8> {
    let options = EncoderOptions {
        block_size: BLOCK_SIZE,
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(level)).unwrap(),
        ..Default::default()
    };
    match dictionary {
        Some(dict) => {
            encode_all_with_prepared_dict_and_options(&case.input, dict, options).unwrap()
        }
        None => encode_all_with_options(&case.input, options).unwrap(),
    }
}

/// This crate's streaming encoder over `input`, fed `piece` bytes at a time.
///
/// Mirrors `stream_encode` in `tests/upstream_interop.rs`, and deliberately
/// pledges no source size: that is what makes the frame a streaming frame
/// rather than a one-shot frame produced through a streaming API, and it is
/// the condition under which the block layout is worth comparing at all.
fn rust_stream_encode(input: &[u8], level: u8, piece: usize) -> Vec<u8> {
    let options = EncoderOptions {
        block_size: BLOCK_SIZE,
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(level)).unwrap(),
        ..Default::default()
    };
    let mut encoder = StreamingEncoder::new(options).expect("streaming encoder must construct");
    let mut out = Vec::new();
    for chunk in input.chunks(piece) {
        encoder.push(chunk).expect("streaming push must succeed");
        out.extend_from_slice(&encoder.take_output());
    }
    encoder.finish().expect("streaming finish must succeed");
    out.extend_from_slice(&encoder.take_output());
    out
}

fn format_piece(piece: usize) -> String {
    if piece.is_multiple_of(1024 * 1024) {
        format!("{} MiB", piece / (1024 * 1024))
    } else {
        format!("{} KiB", piece / 1024)
    }
}

/// One streaming comparison, before it is rendered into either table.
struct StreamingMeasurement {
    case_index: usize,
    level: u8,
    piece: usize,
    rust_bytes: usize,
    upstream_bytes: usize,
}

/// Every streaming comparison in the report, measured concurrently.
///
/// This runs as its own pass, after every timed row in the report is already
/// recorded, and that ordering is the whole reason it may use threads at all.
/// Nothing here is timed — these are exact byte counts, reproducible under any
/// load — but a benchmark sharing a machine with eight compressing threads is
/// not measuring what it claims to, and the report has already been wrong that
/// way once. Keeping the two passes disjoint is what makes the concurrency
/// safe; interleaving them per case would not be, however much tidier it looks.
fn measure_streaming(
    helper: &Path,
    cases: &[CorpusCase],
    levels: &[u8],
) -> Vec<StreamingMeasurement> {
    let sampled = sensitivity_levels(levels);
    let mut tasks = Vec::new();
    for (case_index, case) in cases.iter().enumerate() {
        if case.dict_kind != DictKind::None {
            continue;
        }
        for &level in levels {
            if i32::from(level) > CompressionLevel::MAX.as_i32() {
                continue;
            }
            tasks.push((case_index, level, STREAMING_PRIMARY_PIECE));
        }
        for &piece in &STREAMING_SENSITIVITY_PIECES {
            for &level in &sampled {
                tasks.push((case_index, level, piece));
            }
        }
    }
    if tasks.is_empty() {
        return Vec::new();
    }

    // Longest first, across every case rather than within one. Encode cost
    // rises steeply with level -- a 4 MiB encode at 22 runs into seconds on the
    // slowest corpora against milliseconds at 1 -- so starting the expensive
    // tasks first keeps the last thread from finishing minutes after the rest.
    // Grouping by case instead would idle nine threads behind whichever case
    // happened to be last.
    tasks.sort_by_key(|&(case_index, level, piece)| (std::cmp::Reverse(level), case_index, piece));

    let threads = std::thread::available_parallelism()
        .map(|value| value.get())
        .unwrap_or(1)
        .min(tasks.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(Vec::with_capacity(tasks.len()));

    std::thread::scope(|scope| {
        for _ in 0..threads {
            scope.spawn(|| {
                let mut local = Vec::new();
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(&(case_index, level, piece)) = tasks.get(index) else {
                        break;
                    };
                    let case = &cases[case_index];
                    let ours = rust_stream_encode(&case.input, level, piece);
                    let theirs = compress_streaming_once(
                        helper,
                        i32::from(level),
                        false,
                        piece,
                        &case.input,
                    );

                    // The shared helper cannot decode either frame: its
                    // `decompress` mode sizes the destination from
                    // `ZSTD_getFrameContentSize`, which is exactly what a
                    // streaming frame omits. Our decoder handles both, so
                    // upstream's frame going through it is a real
                    // cross-implementation check in the one direction available
                    // here; the other direction is covered by
                    // `upstream_streaming_frames_round_trip_both_ways`.
                    assert_eq!(
                        decode_all(&ours).expect("Rust must decode its own streaming frame"),
                        case.input,
                        "Rust streaming frame failed to roundtrip for {} level {} piece {}",
                        case.name,
                        level,
                        piece
                    );
                    assert_eq!(
                        decode_all(&theirs).expect("Rust must decode upstream's streaming frame"),
                        case.input,
                        "Rust failed to decode upstream's streaming frame for {} level {} piece {}",
                        case.name,
                        level,
                        piece
                    );

                    local.push(StreamingMeasurement {
                        case_index,
                        level,
                        piece,
                        rust_bytes: ours.len(),
                        upstream_bytes: theirs.len(),
                    });
                }
                results
                    .lock()
                    .expect("streaming results lock")
                    .extend(local);
            });
        }
    });

    let mut measurements = results.into_inner().expect("streaming results lock");
    // Threads finish out of order; the report must not.
    measurements
        .sort_by_key(|measurement| (measurement.case_index, measurement.piece, measurement.level));
    measurements
}

/// Sensitivity levels that survive a `--levels` filter.
fn sensitivity_levels(levels: &[u8]) -> Vec<u8> {
    STREAMING_SENSITIVITY_LEVELS
        .iter()
        .copied()
        .filter(|level| levels.contains(level))
        .collect()
}

/// This crate's streaming output against upstream's streaming output at the
/// same piece size, and against this crate's own one-shot output.
///
/// Both comparisons are here because they answer different questions and
/// disagree often enough to matter. Upstream's *one-shot* output cannot stand
/// in for the first: a streaming frame declares a window instead of a content
/// size and upstream lays its blocks out differently there, so measuring
/// against it conflates two differences. And parity with upstream says nothing
/// about what streaming costs us — on `log-lines` at the top levels our
/// one-shot runs 28% under upstream while our streaming sits within 0.13% of
/// it, so the same row is either excellent or unremarkable depending on which
/// question is being asked.
///
/// Dictionary cases are absent: the helper's streaming mode takes no
/// dictionary, so there would be nothing to compare them against.
fn render_streaming_section(
    levels: &[u8],
    one_shot_bytes: &HashMap<u8, usize>,
    measurements: &[&StreamingMeasurement],
    deltas: &mut Vec<StreamingDelta>,
    case_name: &'static str,
) -> String {
    let mut out = String::new();
    writeln!(&mut out, "### Streaming vs Upstream Streaming").unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "- Both sides are fed {} at a time with no pledged source size, so both frames declare a window rather than a content size.",
        format_piece(STREAMING_PRIMARY_PIECE)
    )
    .unwrap();
    writeln!(
        &mut out,
        "- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level."
    )
    .unwrap();
    writeln!(
        &mut out,
        "- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions."
    )
    .unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "| Level | Rust stream | zstd stream | delta | vs one-shot |"
    )
    .unwrap();
    writeln!(&mut out, "| ---: | ---: | ---: | ---: | ---: |").unwrap();

    for measurement in measurements
        .iter()
        .filter(|measurement| measurement.piece == STREAMING_PRIMARY_PIECE)
    {
        let delta = StreamingDelta {
            case: case_name,
            level: measurement.level,
            piece: measurement.piece,
            rust_bytes: measurement.rust_bytes,
            upstream_bytes: measurement.upstream_bytes,
        };
        let vs_one_shot = one_shot_bytes.get(&measurement.level).map(|&one_shot| {
            (measurement.rust_bytes as f64 - one_shot as f64) / one_shot as f64 * 100.0
        });
        writeln!(
            &mut out,
            "| {} | {} | {} | {:+.2}% | {} |",
            measurement.level,
            measurement.rust_bytes,
            measurement.upstream_bytes,
            delta.delta() * 100.0,
            vs_one_shot
                .map(|value| format!("{value:+.2}%"))
                .unwrap_or_else(|| "—".to_string()),
        )
        .unwrap();
        deltas.push(delta);
    }
    writeln!(&mut out).unwrap();

    let sampled = sensitivity_levels(levels);
    if sampled.is_empty() {
        return out;
    }

    writeln!(&mut out, "### Streaming Piece-Size Sensitivity").unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels {}, one per parser strategy.",
        format_levels(&sampled)
    )
    .unwrap();
    writeln!(
        &mut out,
        "- {} is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.",
        format_piece(BLOCK_SIZE)
    )
    .unwrap();
    writeln!(&mut out).unwrap();
    writeln!(
        &mut out,
        "| Piece | Most over upstream | Level | Most under upstream | Level |"
    )
    .unwrap();
    writeln!(&mut out, "| ---: | ---: | ---: | ---: | ---: |").unwrap();

    for &piece in &STREAMING_SENSITIVITY_PIECES {
        let mut measured = Vec::new();
        for measurement in measurements
            .iter()
            .filter(|measurement| measurement.piece == piece)
        {
            let delta = StreamingDelta {
                case: case_name,
                level: measurement.level,
                piece,
                rust_bytes: measurement.rust_bytes,
                upstream_bytes: measurement.upstream_bytes,
            };
            measured.push((measurement.level, delta.delta()));
            deltas.push(delta);
        }
        let most_over = measured
            .iter()
            .max_by(|left, right| left.1.total_cmp(&right.1))
            .copied();
        let most_under = measured
            .iter()
            .min_by(|left, right| left.1.total_cmp(&right.1))
            .copied();
        if let (Some((over_level, over)), Some((under_level, under))) = (most_over, most_under) {
            writeln!(
                &mut out,
                "| {} | {:+.2}% | {} | {:+.2}% | {} |",
                format_piece(piece),
                over * 100.0,
                over_level,
                under * 100.0,
                under_level,
            )
            .unwrap();
        }
    }
    writeln!(&mut out).unwrap();
    out
}

fn rust_decode_case(
    encoded: &[u8],
    dictionary: Option<&DecoderDictionary<'_>>,
) -> zstandard::Result<Vec<u8>> {
    match dictionary {
        Some(dict) => decode_all_with_prepared_dict(encoded, dict),
        None => decode_all(encoded),
    }
}

fn rust_encode_mib_per_s(
    case: &CorpusCase,
    dictionary: Option<&EncoderDictionary<'_>>,
    level: u8,
    iterations: usize,
) -> f64 {
    let options = EncoderOptions {
        block_size: BLOCK_SIZE,
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(level)).unwrap(),
        ..Default::default()
    };
    let mut encoder = Encoder::new();
    let mut dst = Vec::new();
    let mut best_ns = u128::MAX;
    for _ in 0..TIMING_TRIALS {
        let start = Instant::now();
        let mut total_output = 0usize;
        for _ in 0..iterations {
            match dictionary {
                Some(dict) => encoder
                    .encode_into_with_prepared_dict_and_options(
                        black_box(&case.input),
                        &mut dst,
                        black_box(dict),
                        options,
                    )
                    .unwrap(),
                None => encoder
                    .encode_into_with_options(black_box(&case.input), &mut dst, options)
                    .unwrap(),
            };
            total_output = total_output.wrapping_add(dst.len());
            black_box(total_output);
        }
        best_ns = best_ns.min(start.elapsed().as_nanos());
    }
    mib_per_s((case.input.len() * iterations) as u64, best_ns)
}

/// Time decode into a destination hoisted out of the timing loop.
///
/// The upstream side of this row does the same: `bench_decompress` in the
/// helper allocates `dst` once and reuses it for every iteration, so timing a
/// `decode_all` that returns a fresh `Vec` compared decompression against
/// allocation-plus-decompression. The difference is usually nil, because the
/// allocator hands the same block straight back, and occasionally enormous,
/// because it does not: measured on one frame, one machine, and one build, the
/// two differed by 4.86x. That is what made this column irreproducible — the
/// same json-records level-22 row read 4182 MiB/s benchmarked alone and 2353
/// MiB/s inside a seven-level sweep. Everything below 50% of upstream was read
/// as a decoder gap for as long as the column existed.
///
/// The encode row above never had this problem only because
/// `encode_into_with_options` existed and this function had no counterpart.
fn rust_decode_mib_per_s(
    encoded: &[u8],
    dictionary: Option<&DecoderDictionary<'_>>,
    iterations: usize,
) -> f64 {
    let mut decoder = Decoder::new();
    let mut dst = Vec::new();
    let mut best_ns = u128::MAX;
    let mut trial_output = 0usize;
    for _ in 0..TIMING_TRIALS {
        let start = Instant::now();
        let mut total_output = 0usize;
        for _ in 0..iterations {
            match dictionary {
                Some(dict) => decoder
                    .decode_all_into_with_prepared_dict(
                        black_box(encoded),
                        &mut dst,
                        black_box(dict),
                    )
                    .unwrap(),
                None => decoder
                    .decode_all_into(black_box(encoded), &mut dst)
                    .unwrap(),
            }
            total_output = total_output.wrapping_add(dst.len());
            black_box(total_output);
        }
        best_ns = best_ns.min(start.elapsed().as_nanos());
        trial_output = total_output;
    }
    mib_per_s(trial_output as u64, best_ns)
}

fn should_profile_case(case: &CorpusCase) -> bool {
    matches!(
        case.name,
        "json-records" | "log-lines" | "trained-dictionary" | "binary-structured"
    )
}

fn rust_stage_profile(
    case: &CorpusCase,
    raw_prepared: &EncoderDictionary<'_>,
    trained_prepared: &EncoderDictionary<'_>,
    level: u8,
    iterations: usize,
    phases: PlannerPhases,
) -> EncodeStageProfile {
    let options = EncoderOptions {
        block_size: BLOCK_SIZE,
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(level)).unwrap(),
        ..Default::default()
    };
    let mut encoder = Encoder::new();
    let mut aggregate = EncodeStageProfile::default();
    for _ in 0..iterations {
        let profile = match case.dict_kind {
            DictKind::None => encoder
                .profile_first_block_with_options(black_box(&case.input), options, phases)
                .unwrap(),
            DictKind::Raw => encoder
                .profile_first_block_with_prepared_dict_and_options(
                    black_box(&case.input),
                    black_box(raw_prepared),
                    options,
                    phases,
                )
                .unwrap(),
            DictKind::Trained => encoder
                .profile_first_block_with_prepared_dict_and_options(
                    black_box(&case.input),
                    black_box(trained_prepared),
                    options,
                    phases,
                )
                .unwrap(),
        };
        accumulate_stage_profile(&mut aggregate, profile);
    }
    aggregate
}

fn rust_decode_stage_profile(
    helper: &std::path::Path,
    case: &CorpusCase,
    raw_prepared: &DecoderDictionary<'_>,
    trained_prepared: &DecoderDictionary<'_>,
    level: u8,
    iterations: usize,
) -> DecodeStageProfile {
    let encoded = upstream_zstd::compress_once(
        helper,
        case_modes(case.dict_kind).compress_mode,
        i32::from(level),
        false,
        &case.input,
    );
    let options = DecoderOptions {
        max_window_size: None,
        max_output_size: None,
        verify_checksum: true,
        ..Default::default()
    };
    let mut decoder = Decoder::new();
    let mut aggregate = DecodeStageProfile::default();
    for _ in 0..iterations {
        let profile = match case.dict_kind {
            DictKind::None => decoder
                .profile_first_block_decode_with_options(black_box(&encoded), options)
                .unwrap(),
            DictKind::Raw => decoder
                .profile_first_block_decode_with_prepared_dict_and_options(
                    black_box(&encoded),
                    black_box(raw_prepared),
                    options,
                )
                .unwrap(),
            DictKind::Trained => decoder
                .profile_first_block_decode_with_prepared_dict_and_options(
                    black_box(&encoded),
                    black_box(trained_prepared),
                    options,
                )
                .unwrap(),
        };
        accumulate_decode_stage_profile(&mut aggregate, profile);
    }
    aggregate
}

fn rust_first_block_trace(
    case: &CorpusCase,
    raw_prepared: &EncoderDictionary<'_>,
    trained_prepared: &EncoderDictionary<'_>,
    level: u8,
) -> BlockTrace {
    let options = EncoderOptions {
        block_size: BLOCK_SIZE,
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(level)).unwrap(),
        ..Default::default()
    };
    let mut encoder = Encoder::new();
    match case.dict_kind {
        DictKind::None => encoder
            .trace_first_block_with_options(&case.input, options)
            .unwrap(),
        DictKind::Raw => encoder
            .trace_first_block_with_prepared_dict_and_options(&case.input, raw_prepared, options)
            .unwrap(),
        DictKind::Trained => encoder
            .trace_first_block_with_prepared_dict_and_options(
                &case.input,
                trained_prepared,
                options,
            )
            .unwrap(),
    }
}

/// Get the sequence count from the C (upstream zstd) compressed first block.
///
/// Returns `None` if compression fails or the first block is not a compressed block
/// (e.g., raw or RLE blocks).
fn c_first_block_sequence_count(
    helper: &std::path::Path,
    case: &CorpusCase,
    level: u8,
) -> Option<usize> {
    let modes = case_modes(case.dict_kind);
    let encoded = try_compress_once(
        helper,
        modes.compress_mode,
        i32::from(level),
        false,
        &case.input,
    )
    .ok()?;
    first_block_sequence_count_from_frame(&encoded)
}

/// Parse the sequence count from the first block of a zstd frame, returning `None` if the
/// first block is not a compressed block.
fn first_block_sequence_count_from_frame(frame: &[u8]) -> Option<usize> {
    if frame.len() < 5 {
        return None;
    }
    let header_size = zstd_frame_header_size(frame)?;
    if frame.len() < header_size + 3 {
        return None;
    }
    let bh = read_block_header_u24(&frame[header_size..]);
    let block_type = (bh >> 1) & 0x3;
    if block_type != 2 {
        // Not a compressed block (raw = 0, RLE = 1, reserved = 3).
        return None;
    }
    let block_size = (bh >> 3) as usize;
    let payload_start = header_size + 3;
    if frame.len() < payload_start + block_size {
        return None;
    }
    let payload = &frame[payload_start..payload_start + block_size];
    // Skip the literals section to find the sequence section.
    let lit_end = literals_section_end(payload)?;
    if lit_end >= payload.len() {
        return None;
    }
    let seq_section = &payload[lit_end..];
    if seq_section.is_empty() {
        return None;
    }
    Some(decode_first_byte_sequence_count(seq_section))
}

fn zstd_frame_header_size(src: &[u8]) -> Option<usize> {
    if src.len() < 5 {
        return None;
    }
    let magic = u32::from_le_bytes([src[0], src[1], src[2], src[3]]);
    if magic != 0xFD2F_B528 {
        return None;
    }
    let descriptor = src[4];
    let single_segment = descriptor & (1 << 5) != 0;
    let dict_id_size = match descriptor & 0x3 {
        0 => 0usize,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => unreachable!(),
    };
    let fcs_flag = descriptor >> 6;
    let fcs_size = match fcs_flag {
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        3 => 8,
        _ => unreachable!(),
    };
    Some(5 + usize::from(!single_segment) + dict_id_size + fcs_size)
}

fn read_block_header_u24(src: &[u8]) -> u32 {
    (src[0] as u32) | ((src[1] as u32) << 8) | ((src[2] as u32) << 16)
}

/// Determine the end offset of the literals section within a compressed block payload.
fn literals_section_end(payload: &[u8]) -> Option<usize> {
    if payload.is_empty() {
        return None;
    }
    let byte0 = payload[0];
    let block_type = byte0 & 0x3;
    match block_type {
        // Raw_Literals_Block
        0 => {
            let size_format = (byte0 >> 2) & 0x3;
            match size_format {
                0 | 2 => {
                    let regen_size = (byte0 >> 3) as usize;
                    Some(1 + regen_size)
                }
                1 => {
                    if payload.len() < 2 {
                        return None;
                    }
                    let regen_size = ((byte0 >> 4) as usize) | ((payload[1] as usize) << 4);
                    Some(2 + regen_size)
                }
                3 => {
                    if payload.len() < 3 {
                        return None;
                    }
                    let regen_size = ((byte0 >> 4) as usize)
                        | ((payload[1] as usize) << 4)
                        | ((payload[2] as usize) << 12);
                    Some(3 + regen_size)
                }
                _ => unreachable!(),
            }
        }
        // RLE_Literals_Block
        1 => {
            let size_format = (byte0 >> 2) & 0x3;
            match size_format {
                0 | 2 => Some(1 + 1), // 1-byte header + 1 byte value
                1 => Some(2 + 1),     // 2-byte header + 1 byte value
                3 => Some(3 + 1),     // 3-byte header + 1 byte value
                _ => unreachable!(),
            }
        }
        // Compressed_Literals_Block or Treeless_Literals_Block
        2 | 3 => {
            let size_format = (byte0 >> 2) & 0x3;
            let (header_size, compressed_size) = match size_format {
                0 => {
                    if payload.len() < 3 {
                        return None;
                    }
                    // Both sizes use 10 bits. Total header: 3 bytes.
                    // regeneratedSize = (byte0>>4) + (byte1<<4) & 0x3FF (bits [4..14])
                    // compressedSize uses next 10 bits
                    let combined =
                        (byte0 as u32) | ((payload[1] as u32) << 8) | ((payload[2] as u32) << 16);
                    let compressed = ((combined >> 14) & 0x3FF) as usize;
                    (3usize, compressed)
                }
                1 => {
                    if payload.len() < 3 {
                        return None;
                    }
                    let combined =
                        (byte0 as u32) | ((payload[1] as u32) << 8) | ((payload[2] as u32) << 16);
                    let compressed = ((combined >> 14) & 0x3FF) as usize;
                    (3, compressed)
                }
                2 => {
                    if payload.len() < 4 {
                        return None;
                    }
                    let combined = (byte0 as u32)
                        | ((payload[1] as u32) << 8)
                        | ((payload[2] as u32) << 16)
                        | ((payload[3] as u32) << 24);
                    let compressed = ((combined >> 18) & 0x3FFF) as usize;
                    (4, compressed)
                }
                3 => {
                    if payload.len() < 5 {
                        return None;
                    }
                    let combined = (byte0 as u64)
                        | ((payload[1] as u64) << 8)
                        | ((payload[2] as u64) << 16)
                        | ((payload[3] as u64) << 24)
                        | ((payload[4] as u64) << 32);
                    let compressed = ((combined >> 22) & 0x3FFF) as usize;
                    (5, compressed)
                }
                _ => unreachable!(),
            };
            Some(header_size + compressed_size)
        }
        _ => None,
    }
}

fn decode_first_byte_sequence_count(seq_section: &[u8]) -> usize {
    let byte0 = seq_section[0];
    if byte0 < 128 {
        byte0 as usize
    } else if byte0 < 255 {
        if seq_section.len() < 2 {
            return 0;
        }
        ((byte0 as usize - 128) << 8) + seq_section[1] as usize
    } else {
        if seq_section.len() < 3 {
            return 0;
        }
        0x7F00 + seq_section[1] as usize + ((seq_section[2] as usize) << 8)
    }
}

fn accumulate_stage_profile(aggregate: &mut EncodeStageProfile, profile: EncodeStageProfile) {
    aggregate.total += profile.total;
    aggregate.block_split += profile.block_split;
    aggregate.planning += profile.planning;
    aggregate.planning_row_search += profile.planning_row_search;
    aggregate.planning_chain_search += profile.planning_chain_search;
    aggregate.planning_rep_check += profile.planning_rep_check;
    aggregate.planning_match_count += profile.planning_match_count;
    aggregate.planning_insert_update += profile.planning_insert_update;
    aggregate.planning_parser += profile.planning_parser;
    aggregate.planning_row_parser_baseline_rep += profile.planning_row_parser_baseline_rep;
    aggregate.planning_row_parser_baseline_regular += profile.planning_row_parser_baseline_regular;
    aggregate.planning_row_parser_continue += profile.planning_row_parser_continue;
    aggregate.planning_row_parser_store += profile.planning_row_parser_store;
    aggregate.planning_row_parser_rep2 += profile.planning_row_parser_rep2;
    aggregate.literals += profile.literals;
    aggregate.sequences += profile.sequences;
    aggregate.sequence_codes += profile.sequence_codes;
    aggregate.sequence_statistics += profile.sequence_statistics;
    aggregate.sequence_bitstream += profile.sequence_bitstream;
    aggregate.sequence_assembly += profile.sequence_assembly;
    aggregate.blocks += profile.blocks;
    aggregate.compressed_blocks += profile.compressed_blocks;
    aggregate.raw_blocks += profile.raw_blocks;
    aggregate.rle_blocks += profile.rle_blocks;
}

fn accumulate_decode_stage_profile(
    aggregate: &mut DecodeStageProfile,
    profile: DecodeStageProfile,
) {
    aggregate.total += profile.total;
    aggregate.literals += profile.literals;
    aggregate.sequence_tables += profile.sequence_tables;
    aggregate.sequence_commands += profile.sequence_commands;
    aggregate.sequence_execute += profile.sequence_execute;
    aggregate.sequence_execute_literal_copy += profile.sequence_execute_literal_copy;
    aggregate.sequence_execute_prefix_match_copy += profile.sequence_execute_prefix_match_copy;
    aggregate.sequence_execute_dictionary_match_copy +=
        profile.sequence_execute_dictionary_match_copy;
    aggregate.blocks += profile.blocks;
    aggregate.compressed_blocks += profile.compressed_blocks;
    aggregate.raw_blocks += profile.raw_blocks;
    aggregate.rle_blocks += profile.rle_blocks;
}

fn stage_other(profile: EncodeStageProfile) -> Duration {
    profile
        .total
        .saturating_sub(profile.block_split)
        .saturating_sub(profile.planning)
        .saturating_sub(profile.literals)
        .saturating_sub(profile.sequences)
}

fn stage_row_parser_other(profile: EncodeStageProfile) -> Duration {
    profile
        .planning_parser
        .saturating_sub(profile.planning_row_parser_baseline_rep)
        .saturating_sub(profile.planning_row_parser_baseline_regular)
        .saturating_sub(profile.planning_row_parser_continue)
        .saturating_sub(profile.planning_row_parser_store)
        .saturating_sub(profile.planning_row_parser_rep2)
}

fn decode_stage_other(profile: DecodeStageProfile) -> Duration {
    profile
        .total
        .saturating_sub(profile.literals)
        .saturating_sub(profile.sequence_tables)
        .saturating_sub(profile.sequence_commands)
        .saturating_sub(profile.sequence_execute)
}

fn decode_execute_other(profile: DecodeStageProfile) -> Duration {
    profile
        .sequence_execute
        .saturating_sub(profile.sequence_execute_literal_copy)
        .saturating_sub(profile.sequence_execute_prefix_match_copy)
        .saturating_sub(profile.sequence_execute_dictionary_match_copy)
}

fn duration_ms(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn share_percent(part: Duration, total: Duration) -> f64 {
    if total.is_zero() {
        return 0.0;
    }
    part.as_secs_f64() * 100.0 / total.as_secs_f64()
}

fn mib_per_s(bytes: u64, elapsed_ns: u128) -> f64 {
    let seconds = elapsed_ns as f64 / 1_000_000_000.0;
    if seconds == 0.0 {
        return 0.0;
    }
    (bytes as f64 / MIB) / seconds
}

fn dict_kind_name(dict_kind: DictKind) -> &'static str {
    match dict_kind {
        DictKind::None => "none",
        DictKind::Raw => "raw-content",
        DictKind::Trained => "trained",
    }
}

fn format_optional_ratio(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.4}"))
        .unwrap_or_else(|| "n/a".to_string())
}

fn format_optional_speed(value: Option<f64>) -> String {
    value
        .map(|value| format!("{value:.2}"))
        .unwrap_or_else(|| "n/a".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_levels_supports_ranges_and_deduplicates() {
        assert_eq!(parse_levels("7,5-6,6,9"), vec![5, 6, 7, 9]);
    }

    #[test]
    fn format_levels_coalesces_adjacent_ranges() {
        assert_eq!(format_levels(&[1, 2, 3, 5, 7, 8]), "1-3,5,7-8");
    }

    /// The list used to print ratios to four decimal places, so a row 40 bytes
    /// larger read exactly like one 7.6% larger and the count treated them the
    /// same. Rows are ranked by excess and carry their byte counts.
    #[test]
    fn the_regression_list_leads_with_the_rows_that_matter() {
        let mut regressions = vec![
            RatioRegression {
                case: "raw-dictionary",
                level: 18,
                rust_bytes: 53408,
                upstream_bytes: 53368,
            },
            RatioRegression {
                case: "json-records",
                level: 16,
                rust_bytes: 240181,
                upstream_bytes: 223302,
            },
            RatioRegression {
                case: "tabular-csv",
                level: 8,
                rust_bytes: 548926,
                upstream_bytes: 543126,
            },
        ];

        let summary = format_fail_summary(&[], &[], &mut regressions, &mut [], &[], &[]);
        assert!(summary.contains("| Ratio regressions | 3 |"), "{summary}");
        assert!(
            summary.contains("| Ratio regressions above 1% | 2 |"),
            "{summary}"
        );

        let rows: Vec<&str> = summary
            .lines()
            .filter(|line| line.starts_with("- "))
            .collect();
        assert_eq!(rows.len(), 3, "{summary}");
        assert!(
            rows[0].starts_with("- json-records L16 +7.56%"),
            "{:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("- tabular-csv L8 +1.07%"),
            "{:?}",
            rows[1]
        );
        assert!(
            rows[2] == "- raw-dictionary L18 +0.07% (53408 vs 53368 bytes, +40)",
            "{:?}",
            rows[2]
        );
    }

    /// The per-case tally flags a slow band, ignores a noisy row, and is not
    /// fooled by the case whose median lands between two clusters.
    ///
    /// Three shapes, and they are the whole reason this aggregate is here
    /// beside the per-row floor:
    ///
    /// - `spiky` is level with upstream except for one row at 0.30x, which is
    ///   what a spoiled timing trial looks like: lower than any row in the
    ///   other two and meaning nothing.
    /// - `banded` is the real shape, and the one that motivated all of this.
    ///   A minority of its levels are far behind and the rest are at parity,
    ///   so its *median* is 1.00x and reads as a case with nothing wrong.
    ///   `raw-dictionary` is this, behind on levels 1 through 10 of 22, and
    ///   measured 0.84x then 0.95x on two consecutive sweeps of identical
    ///   code.
    /// - `steady` is behind everywhere, which any aggregate would catch.
    ///
    /// A summary that named `spiky` and not `banded` would have them exactly
    /// backwards, and a median names neither.
    #[test]
    fn the_per_case_tally_flags_a_slow_band_and_not_a_single_slow_row() {
        let mut rows = Vec::new();
        for level in 1..=9u8 {
            rows.push(SpeedRow {
                case: "spiky",
                level,
                encode: Some(if level == 4 { 0.30 } else { 1.02 }),
                decode: Some(1.00),
            });
            rows.push(SpeedRow {
                case: "banded",
                level,
                encode: Some(if level <= 4 { 0.70 } else { 1.00 }),
                decode: Some(1.00),
            });
            rows.push(SpeedRow {
                case: "steady",
                level,
                encode: Some(0.88),
                decode: Some(1.00),
            });
            // Exactly a third of nine, so this pins the flag's boundary from
            // the flagging side; `spiky` at one level pins the other.
            rows.push(SpeedRow {
                case: "edge",
                level,
                encode: Some(if level <= 3 { 0.70 } else { 1.00 }),
                decode: Some(1.00),
            });
        }
        let case = |name| CorpusCase {
            name,
            description: "",
            input: Vec::new(),
            dict_kind: DictKind::None,
        };
        let cases = [case("spiky"), case("banded"), case("steady"), case("edge")];

        let summary = format_fail_summary(&[], &[], &mut [], &mut [], &rows, &cases);

        assert!(
            summary.contains("| Cases behind upstream on a third of encode levels | 3 |"),
            "{summary}"
        );
        // Exactly a third flags; one level below it does not.
        assert!(
            summary.contains("| edge | 3/9 | 1.00x | 0.70x | 1 |"),
            "{summary}"
        );
        // The spike is reported and counts for one level, which is not most.
        assert!(
            summary.contains("| spiky | 1/9 | 1.02x | 0.30x | 4 |"),
            "{summary}"
        );
        // The median that would have hidden this case, next to the tally that
        // does not.
        assert!(
            summary.contains("| banded | 4/9 | 1.00x | 0.70x | 1 |"),
            "{summary}"
        );
        assert!(
            summary.contains("| steady | 9/9 | 0.88x | 0.88x | 1 |"),
            "{summary}"
        );
    }

    /// The streaming gate counts both directions, and the direction that would
    /// be invisible under one-shot semantics is the one that matters most here.
    ///
    /// `tabular-csv` L19 is the shape of the known deviation: we emit far fewer
    /// bytes than upstream because `next_block_size` declines a split upstream
    /// takes. A gate modelled on the one-shot list, which only collects rows
    /// where this crate emits *more*, would report that as a clean sweep.
    #[test]
    fn the_streaming_gate_counts_both_directions() {
        let mut deltas = vec![
            StreamingDelta {
                case: "tabular-csv",
                level: 19,
                piece: 32 * 1024,
                rust_bytes: 452_000,
                upstream_bytes: 555_000,
            },
            StreamingDelta {
                case: "binary-structured",
                level: 9,
                piece: 32 * 1024,
                rust_bytes: 101_840,
                upstream_bytes: 100_000,
            },
            StreamingDelta {
                case: "json-records",
                level: 5,
                piece: 16 * 1024,
                rust_bytes: 412_467,
                upstream_bytes: 412_350,
            },
        ];

        let summary = format_fail_summary(&[], &[], &mut [], &mut deltas, &[], &[]);

        // Two rows are above upstream, but only one is above it materially;
        // the third is 117 bytes on a 412 KB frame.
        assert!(
            summary.contains("| Streaming rows above upstream | 2 |"),
            "{summary}"
        );
        assert!(
            summary.contains("| Streaming rows above upstream by 1% | 1 |"),
            "{summary}"
        );
        assert!(
            summary.contains("| Streaming rows below upstream by 1% | 1 |"),
            "{summary}"
        );

        let rows: Vec<&str> = summary
            .lines()
            .filter(|line| line.starts_with("- "))
            .collect();
        assert_eq!(rows.len(), 2, "{summary}");
        assert!(
            rows[0] == "- binary-structured L9 piece 32 KiB +1.84% (101840 vs 100000 bytes)",
            "{:?}",
            rows[0]
        );
        assert!(
            rows[1].starts_with("- tabular-csv L19 piece 32 KiB -18.56%"),
            "{:?}",
            rows[1]
        );
    }

    /// Both of these rows round to the same four-decimal ratio on a 4 MiB case,
    /// which is exactly how the old comparison rendered them. Their excess
    /// differs by more than a factor of ten.
    #[test]
    fn rows_that_round_to_the_same_ratio_are_still_ranked_apart() {
        let tie = RatioRegression {
            case: "raw-dictionary",
            level: 18,
            rust_bytes: 53408,
            upstream_bytes: 53368,
        };
        let real = RatioRegression {
            case: "tabular-csv",
            level: 10,
            rust_bytes: 555603,
            upstream_bytes: 549584,
        };

        let input = DEFAULT_INPUT_BYTES as f64;
        assert_eq!(
            format!("{:.4}", tie.rust_bytes as f64 / input),
            format!("{:.4}", tie.upstream_bytes as f64 / input),
            "this row is the kind the four-decimal comparison could not show"
        );
        assert!(real.excess() > MATERIAL_RATIO_EXCESS);
        assert!(tie.excess() < MATERIAL_RATIO_EXCESS / 10.0);
    }
}
