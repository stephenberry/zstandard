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
use std::time::Instant;
use zstandard::{Decoder, DecoderDictionary, DecoderOptions, StreamingDecoder};

const INPUT_BYTES: usize = 512 * 1024;

struct Args {
    case: String,
    level: i32,
    iters: usize,
    chunk: usize,
    /// `both` reports the ratio; `streaming` and `one-shot` run a single path
    /// so a sampling profile attributes every frame to it.
    mode: String,
}

fn parse_args() -> Args {
    let mut case = String::from("log-lines");
    let mut level = 5i32;
    let mut iters = 200usize;
    let mut chunk = 128 * 1024usize;
    let mut mode = String::from("both");
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = || {
            args.next()
                .unwrap_or_else(|| panic!("{arg} requires a value"))
        };
        match arg.as_str() {
            "--case" => case = value(),
            "--level" => level = value().parse().expect("level must be an integer"),
            "--iters" => iters = value().parse().expect("iters must be an integer"),
            "--chunk" => chunk = value().parse().expect("chunk must be an integer"),
            "--mode" => mode = value(),
            _ => panic!("unknown argument: {arg}"),
        }
    }
    assert!(
        matches!(mode.as_str(), "both" | "streaming" | "one-shot"),
        "--mode must be both, streaming, or one-shot"
    );
    Args {
        case,
        level,
        iters,
        chunk,
        mode,
    }
}

fn compress_mode(dict_kind: DictKind) -> &'static str {
    match dict_kind {
        DictKind::None => "compress-regular-configured",
        DictKind::Raw => "compress-raw-dict-configured",
        DictKind::Trained => "compress-trained-dict-configured",
    }
}

/// Best-of-N wall time for one closure, in seconds per iteration.
fn best_of<F: FnMut() -> usize>(rounds: usize, iters: usize, mut body: F) -> (f64, usize) {
    let mut best = f64::INFINITY;
    let mut checksum = 0usize;
    for _ in 0..rounds {
        let start = Instant::now();
        for _ in 0..iters {
            checksum = checksum.wrapping_add(body());
        }
        let seconds = start.elapsed().as_secs_f64() / iters as f64;
        best = best.min(seconds);
    }
    (best, checksum)
}

fn main() {
    let args = parse_args();
    let cases = benchmark_report_cases(INPUT_BYTES);
    let case = cases
        .iter()
        .find(|case| case.name == args.case)
        .unwrap_or_else(|| panic!("unknown case {}", args.case));

    let helper = upstream_zstd::require_helper("profile_streaming_decode");
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

    let run_one_shot = args.mode != "streaming";
    let run_streaming = args.mode != "one-shot";

    let mut decoder = Decoder::new();
    let (one_shot, one_shot_len) = best_of(if run_one_shot { 3 } else { 0 }, args.iters, || {
        let decoded = match case.dict_kind {
            DictKind::None => decoder.decode_all(black_box(&encoded)).unwrap(),
            DictKind::Raw => decoder
                .decode_all_with_prepared_dict(black_box(&encoded), black_box(&raw_prepared))
                .unwrap(),
            DictKind::Trained => decoder
                .decode_all_with_prepared_dict(black_box(&encoded), black_box(&trained_prepared))
                .unwrap(),
        };
        decoded.len()
    });

    // The streaming decoder is reconstructed per iteration rather than reset,
    // because a caller decoding one stream is what the benchmark is about and
    // reset() would hide the per-stream setup that path pays.
    let (streaming, streaming_len) = best_of(if run_streaming { 3 } else { 0 }, args.iters, || {
        let mut stream = match case.dict_kind {
            DictKind::None => StreamingDecoder::new(DecoderOptions::default()),
            DictKind::Raw => {
                StreamingDecoder::with_prepared_dict(&raw_prepared, DecoderOptions::default())
            }
            DictKind::Trained => {
                StreamingDecoder::with_prepared_dict(&trained_prepared, DecoderOptions::default())
            }
        };
        let mut produced = 0usize;
        for piece in black_box(&encoded).chunks(args.chunk) {
            stream.push(piece).unwrap();
            produced += stream.take_output().len();
        }
        stream.finish().unwrap();
        produced + stream.take_output().len()
    });

    if run_one_shot && run_streaming {
        assert_eq!(
            one_shot_len, streaming_len,
            "the two paths must decode the same number of bytes"
        );
    }

    let mib = case.input.len() as f64 / (1024.0 * 1024.0);
    print!(
        "case={} level={} chunk={} encoded={}",
        args.case,
        args.level,
        args.chunk,
        encoded.len()
    );
    if run_one_shot {
        print!("\n  one-shot  {:8.1} MiB/s", mib / one_shot);
    }
    if run_streaming {
        print!("\n  streaming {:8.1} MiB/s", mib / streaming);
    }
    if run_one_shot && run_streaming {
        print!("  ({:.0}% of one-shot)", 100.0 * one_shot / streaming);
    }
    println!();
}
