// Shared support module: several targets `#[path]`-include this same file and
// each uses a different subset, so the rest reads as dead in every one of
// them. Deleting is not an option — what one target drops, another needs.
#[path = "../support/corpora.rs"]
#[allow(dead_code)]
mod corpora;
#[path = "../support/upstream_zstd.rs"]
#[allow(dead_code)]
mod upstream_zstd;

use corpora::{DictKind, benchmark_report_cases};
use std::time::Duration;
use zstandard::{DecodeStageProfile, Decoder, DecoderDictionary};

/// Matches `DEFAULT_INPUT_BYTES` in `benchmark_report`, so a profile here
/// describes the same frame the report's decode column timed. The two used to
/// disagree — this profiled 512 KiB while the report measured 4 MiB — and a
/// frame that fits in one block does not exercise the paths that only open once
/// a match can point at an earlier one.
const DEFAULT_INPUT_BYTES: usize = 4 * 1024 * 1024;

struct Args {
    case: String,
    level: i32,
    iters: usize,
    input_bytes: usize,
    first_block_only: bool,
    throughput: bool,
}

fn parse_args() -> Args {
    let mut case = String::from("log-lines");
    let mut level = 5i32;
    let mut iters = 100usize;
    let mut input_bytes = DEFAULT_INPUT_BYTES;
    let mut first_block_only = false;
    let mut throughput = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--case" => case = args.next().expect("--case requires a value"),
            "--level" => {
                level = args
                    .next()
                    .expect("--level requires a value")
                    .parse()
                    .expect("level must be an integer");
            }
            "--iters" => {
                iters = args
                    .next()
                    .expect("--iters requires a value")
                    .parse()
                    .expect("iters must be an integer");
            }
            "--input-bytes" => {
                input_bytes = args
                    .next()
                    .expect("--input-bytes requires a value")
                    .parse()
                    .expect("input-bytes must be an integer");
            }
            "--first-block-only" => first_block_only = true,
            "--throughput" => throughput = true,
            _ => panic!("unknown argument: {arg}"),
        }
    }
    Args {
        case,
        level,
        iters,
        input_bytes,
        first_block_only,
        throughput,
    }
}

fn compress_mode(dict_kind: DictKind) -> &'static str {
    match dict_kind {
        DictKind::None => "compress-regular-configured",
        DictKind::Raw => "compress-raw-dict-configured",
        DictKind::Trained => "compress-trained-dict-configured",
    }
}

fn main() {
    let args = parse_args();
    let cases = benchmark_report_cases(args.input_bytes);
    let case = cases
        .iter()
        .find(|case| case.name == args.case)
        .unwrap_or_else(|| panic!("unknown case {}", args.case));

    let helper = upstream_zstd::require_helper("profile_decode_stage");
    let raw_dictionary = upstream_zstd::emit_raw_dictionary(helper);
    let trained_dictionary = upstream_zstd::emit_trained_dictionary(helper);
    let raw_prepared = DecoderDictionary::new(&raw_dictionary).expect("raw dictionary must parse");
    let trained_prepared =
        DecoderDictionary::new(&trained_dictionary).expect("trained dictionary must parse");
    let encoded = upstream_zstd::try_compress_once(
        helper,
        compress_mode(case.dict_kind),
        args.level,
        false,
        &case.input,
    )
    .unwrap_or_else(|error| panic!("failed to build upstream frame for {}: {error}", case.name));

    let mut decoder = Decoder::new();

    // The stage profile below and this measurement do not decode the same way.
    // Stage attribution needs each phase to start and stop separately, so it
    // decodes sequence commands into a buffer and then executes them; the real
    // decoder fuses those two into one pass. The fused path is several times
    // faster, which means a percentage from the stage table describes the
    // split, not the cost. `--throughput` is what to compare against upstream.
    if args.throughput {
        // Both loops decode the same frame with the same decoder. The only
        // difference is who owns the destination, which is the difference the
        // benchmark report was unknowingly measuring: upstream's helper hoists
        // its `malloc` out of the timing loop, so a `decode_all` that returns a
        // fresh `Vec` was being compared against decompression alone.
        let mut allocating = f64::MAX;
        let mut reusing = f64::MAX;
        let mut reused = Vec::new();
        for _ in 0..TIMING_TRIALS {
            let start = std::time::Instant::now();
            let mut decoded = 0usize;
            for _ in 0..args.iters {
                let out = match case.dict_kind {
                    DictKind::None => decoder.decode_all(&encoded),
                    DictKind::Raw => decoder.decode_all_with_prepared_dict(&encoded, &raw_prepared),
                    DictKind::Trained => {
                        decoder.decode_all_with_prepared_dict(&encoded, &trained_prepared)
                    }
                }
                .unwrap();
                decoded += out.len();
            }
            allocating = allocating.min(seconds_per_mib(start.elapsed(), decoded));

            let start = std::time::Instant::now();
            let mut decoded = 0usize;
            for _ in 0..args.iters {
                match case.dict_kind {
                    DictKind::None => decoder.decode_all_into(&encoded, &mut reused),
                    DictKind::Raw => decoder.decode_all_into_with_prepared_dict(
                        &encoded,
                        &mut reused,
                        &raw_prepared,
                    ),
                    DictKind::Trained => decoder.decode_all_into_with_prepared_dict(
                        &encoded,
                        &mut reused,
                        &trained_prepared,
                    ),
                }
                .unwrap();
                decoded += reused.len();
            }
            reusing = reusing.min(seconds_per_mib(start.elapsed(), decoded));
        }
        println!(
            "case={} level={} iters={} in={} enc={} allocating_MiB/s={:.2} reusing_MiB/s={:.2} speedup={:.2}x",
            case.name,
            args.level,
            args.iters,
            case.input.len(),
            encoded.len(),
            1.0 / allocating,
            1.0 / reusing,
            allocating / reusing,
        );
        return;
    }

    let mut aggregate = DecodeStageProfile::default();
    for _ in 0..args.iters {
        let profile = match (args.first_block_only, case.dict_kind) {
            (true, DictKind::None) => {
                decoder.profile_first_block_decode_with_options(&encoded, Default::default())
            }
            (true, DictKind::Raw) => decoder
                .profile_first_block_decode_with_prepared_dict_and_options(
                    &encoded,
                    &raw_prepared,
                    Default::default(),
                ),
            (true, DictKind::Trained) => decoder
                .profile_first_block_decode_with_prepared_dict_and_options(
                    &encoded,
                    &trained_prepared,
                    Default::default(),
                ),
            (false, DictKind::None) => {
                decoder.profile_frame_decode_with_options(&encoded, Default::default())
            }
            (false, DictKind::Raw) => decoder.profile_frame_decode_with_prepared_dict_and_options(
                &encoded,
                &raw_prepared,
                Default::default(),
            ),
            (false, DictKind::Trained) => decoder
                .profile_frame_decode_with_prepared_dict_and_options(
                    &encoded,
                    &trained_prepared,
                    Default::default(),
                ),
        }
        .unwrap();
        aggregate = accumulate(aggregate, profile);
    }

    println!(
        "case={} level={} iters={} scope={} in={} enc={} total_ms={:.2} literals_ms={:.3} literals={:.1}% seq_tables={:.1}% seq_cmds={:.1}% execute={:.1}% exec_lits={:.1}% exec_prefix={:.1}% exec_dict={:.1}% blocks={} compressed={} raw={} rle={}",
        case.name,
        args.level,
        args.iters,
        if args.first_block_only {
            "first-block"
        } else {
            "frame"
        },
        case.input.len(),
        encoded.len(),
        millis(aggregate.total),
        millis(aggregate.literals),
        pct(aggregate.literals, aggregate.total),
        pct(aggregate.sequence_tables, aggregate.total),
        pct(aggregate.sequence_commands, aggregate.total),
        pct(aggregate.sequence_execute, aggregate.total),
        pct(aggregate.sequence_execute_literal_copy, aggregate.total),
        pct(
            aggregate.sequence_execute_prefix_match_copy,
            aggregate.total
        ),
        pct(
            aggregate.sequence_execute_dictionary_match_copy,
            aggregate.total
        ),
        aggregate.blocks,
        aggregate.compressed_blocks,
        aggregate.raw_blocks,
        aggregate.rle_blocks,
    );
}

fn accumulate(mut left: DecodeStageProfile, right: DecodeStageProfile) -> DecodeStageProfile {
    left.total += right.total;
    left.literals += right.literals;
    left.sequence_tables += right.sequence_tables;
    left.sequence_commands += right.sequence_commands;
    left.sequence_execute += right.sequence_execute;
    left.sequence_execute_literal_copy += right.sequence_execute_literal_copy;
    left.sequence_execute_prefix_match_copy += right.sequence_execute_prefix_match_copy;
    left.sequence_execute_dictionary_match_copy += right.sequence_execute_dictionary_match_copy;
    left.blocks += right.blocks;
    left.compressed_blocks += right.compressed_blocks;
    left.raw_blocks += right.raw_blocks;
    left.rle_blocks += right.rle_blocks;
    left.output_bytes += right.output_bytes;
    left
}

/// Best-of-N, matching `benchmark_report`: background load can only ever make a
/// trial slower, so the minimum is the least contaminated estimate available.
const TIMING_TRIALS: usize = 3;

fn seconds_per_mib(elapsed: Duration, bytes: usize) -> f64 {
    elapsed.as_secs_f64() / (bytes as f64 / (1024.0 * 1024.0))
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

fn pct(part: Duration, total: Duration) -> f64 {
    if total.is_zero() {
        0.0
    } else {
        (part.as_secs_f64() / total.as_secs_f64()) * 100.0
    }
}
