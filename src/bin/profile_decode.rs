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
use zstandard::{Decoder, DecoderDictionary};

const INPUT_BYTES: usize = 512 * 1024;

struct Args {
    case: String,
    level: i32,
    iters: usize,
}

fn parse_args() -> Args {
    let mut case = String::from("log-lines");
    let mut level = 5i32;
    let mut iters = 500usize;
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
            _ => panic!("unknown argument: {arg}"),
        }
    }
    Args { case, level, iters }
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
    let cases = benchmark_report_cases(INPUT_BYTES);
    let case = cases
        .iter()
        .find(|case| case.name == args.case)
        .unwrap_or_else(|| panic!("unknown case {}", args.case));

    let helper = upstream_zstd::require_helper("profile_decode");
    let raw_dictionary = upstream_zstd::emit_raw_dictionary(helper);
    let trained_dictionary = upstream_zstd::emit_trained_dictionary(helper);
    let raw_prepared = DecoderDictionary::new(&raw_dictionary).expect("raw dictionary must parse");
    let trained_prepared =
        DecoderDictionary::new(&trained_dictionary).expect("trained dictionary must parse");

    let encoded = upstream_zstd::compress_once(
        helper,
        compress_mode(case.dict_kind),
        args.level,
        false,
        &case.input,
    );

    let mut decoder = Decoder::new();
    let mut total_len = 0usize;
    // Timed here rather than around the process: building the upstream helper
    // and compressing the corpus run first and take longer than the decode loop
    // on the faster cases, so an external `time` measures mostly setup.
    let started = std::time::Instant::now();
    for _ in 0..args.iters {
        let decoded = match case.dict_kind {
            DictKind::None => decoder.decode_all(black_box(&encoded)).unwrap(),
            DictKind::Raw => decoder
                .decode_all_with_prepared_dict(black_box(&encoded), black_box(&raw_prepared))
                .unwrap(),
            DictKind::Trained => decoder
                .decode_all_with_prepared_dict(black_box(&encoded), black_box(&trained_prepared))
                .unwrap(),
        };
        total_len = total_len.wrapping_add(black_box(decoded.len()));
    }

    let elapsed = started.elapsed();

    let decoded_bytes = (case.input.len() as f64) * (args.iters as f64);
    println!(
        "case={} level={} iters={} encoded={} total_len={} seconds={:.6} mb_per_s={:.1}",
        args.case,
        args.level,
        args.iters,
        encoded.len(),
        total_len,
        elapsed.as_secs_f64(),
        decoded_bytes / elapsed.as_secs_f64() / (1024.0 * 1024.0),
    );
}
