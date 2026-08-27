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
use std::hint::black_box;
use zstandard::{CompressionLevel, Encoder, EncoderDictionary, EncoderOptions, ParameterOverrides};

/// What `benchmark_report` measures, and therefore the only size a profile of
/// it can be read against. Overridable because the interesting rows are not all
/// at the same size, but the default has to be the reported one: profiling a
/// 512 KiB input and attributing the result to a 4 MiB row is how the decode
/// stage profiler ended up describing code that was never on the slow path.
const DEFAULT_INPUT_BYTES: usize = 4 * 1024 * 1024;
const BLOCK_SIZE: usize = 128 * 1024;

struct Args {
    case: String,
    level: u8,
    iters: usize,
    input_bytes: usize,
    /// Table-size overrides, for sweeping one table at a time. Sweeping a
    /// single log while the level holds everything else fixed is what
    /// separates "the parser costs more per unit of work" from "the parser
    /// does more work"; see `tmp/instr/` for the counting half of that.
    overrides: ParameterOverrides,
}

fn parse_args() -> Args {
    let mut case = String::from("json-records");
    let mut level = 6u8;
    let mut iters = 200usize;
    let mut input_bytes = DEFAULT_INPUT_BYTES;
    let mut overrides = ParameterOverrides::default();
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
            "--bytes" => {
                input_bytes = args
                    .next()
                    .expect("--bytes requires a value")
                    .parse()
                    .expect("bytes must be an integer");
            }
            "--hash-log" => overrides.hash_log = Some(parse_log(&mut args, "--hash-log")),
            "--chain-log" => overrides.chain_log = Some(parse_log(&mut args, "--chain-log")),
            "--window-log" => overrides.window_log = Some(parse_log(&mut args, "--window-log")),
            _ => panic!("unknown argument: {arg}"),
        }
    }
    Args {
        case,
        level,
        iters,
        input_bytes,
        overrides,
    }
}

fn parse_log(args: &mut impl Iterator<Item = String>, flag: &str) -> u32 {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
        .parse()
        .unwrap_or_else(|_| panic!("{flag} must be an integer"))
}

fn main() {
    let args = parse_args();
    let cases = benchmark_report_cases(args.input_bytes);
    let case = cases
        .iter()
        .find(|case| case.name == args.case)
        .unwrap_or_else(|| panic!("unknown case {}", args.case));

    let options = EncoderOptions {
        block_size: BLOCK_SIZE,
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(args.level))
            .expect("level must be a supported public compression level"),
        parameters: args.overrides,
        ..Default::default()
    };

    let helper = match case.dict_kind {
        DictKind::None => None,
        _ => Some(
            upstream_zstd::helper_path()
                .expect("dictionary cases require sibling ../zstd checkout"),
        ),
    };
    let raw_dictionary = helper.map(|helper| upstream_zstd::emit_raw_dictionary(helper));
    let trained_dictionary = helper.map(|helper| upstream_zstd::emit_trained_dictionary(helper));
    let raw_prepared = raw_dictionary
        .as_deref()
        .map(|dict| EncoderDictionary::new(dict).expect("raw dictionary must parse"));
    let trained_prepared = trained_dictionary
        .as_deref()
        .map(|dict| EncoderDictionary::new(dict).expect("trained dictionary must parse"));

    let mut encoder = Encoder::new();
    let mut dst = Vec::new();
    let mut total_len = 0usize;
    let started = std::time::Instant::now();
    for _ in 0..args.iters {
        match case.dict_kind {
            DictKind::None => encoder
                .encode_into_with_options(black_box(&case.input), &mut dst, options)
                .unwrap(),
            DictKind::Raw => encoder
                .encode_into_with_prepared_dict_and_options(
                    black_box(&case.input),
                    &mut dst,
                    raw_prepared.as_ref().unwrap(),
                    options,
                )
                .unwrap(),
            DictKind::Trained => encoder
                .encode_into_with_prepared_dict_and_options(
                    black_box(&case.input),
                    &mut dst,
                    trained_prepared.as_ref().unwrap(),
                    options,
                )
                .unwrap(),
        };
        total_len = total_len.wrapping_add(black_box(dst.len()));
    }

    let elapsed = started.elapsed();
    // The encoder is reused across iterations here, as it is in the benchmark
    // report, so this number is comparable to the report's encode column.
    let mib = (case.input.len() as f64 * args.iters as f64) / (1024.0 * 1024.0);
    // `total_len` is printed so a sweep can assert the parse did not move:
    // if the compressed length is constant across table sizes, every stage
    // after the parser did identical work and the timing delta is the
    // parser's alone.
    println!(
        "case={} level={} bytes={} iters={} hash_log={} chain_log={} window_log={} \
         total_len={} elapsed={:.3}s throughput={:.1} MiB/s",
        args.case,
        args.level,
        case.input.len(),
        args.iters,
        describe_log(args.overrides.hash_log),
        describe_log(args.overrides.chain_log),
        describe_log(args.overrides.window_log),
        total_len,
        elapsed.as_secs_f64(),
        mib / elapsed.as_secs_f64(),
    );
}

fn describe_log(log: Option<u32>) -> String {
    log.map_or_else(|| String::from("level"), |log| log.to_string())
}
