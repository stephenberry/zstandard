// Shared support module: several targets `#[path]`-include this same file and
// each uses a different subset, so the rest reads as dead in every one of
// them. Deleting is not an option — what one target drops, another needs.
use std::{hint::black_box, time::Instant};

#[path = "../src/support/upstream_zstd.rs"]
#[allow(dead_code)]
mod upstream_zstd;

use upstream_zstd::{benchmark_compress_mode, benchmark_mode, compress_once, helper_path};
use zstandard::{
    CompressionLevel, EncoderOptions, decode_all, decode_all_with_dict,
    encode_all_with_dict_and_options, encode_all_with_options,
};

const QUICK_ENCODE_TARGET_BYTES: usize = 16 * 1024 * 1024;
const QUICK_DECODE_TARGET_BYTES: usize = 32 * 1024 * 1024;
const DEFAULT_ENCODE_TARGET_BYTES: usize = 64 * 1024 * 1024;
const DEFAULT_DECODE_TARGET_BYTES: usize = 128 * 1024 * 1024;
const MIB: f64 = 1024.0 * 1024.0;

#[derive(Clone, Copy)]
enum DictKind {
    None,
    Raw,
    Trained,
}

struct BenchCase {
    name: &'static str,
    input: Vec<u8>,
    level: CompressionLevel,
    checksum: bool,
    dict_kind: DictKind,
}

struct BenchConfig {
    encode_target_bytes: usize,
    decode_target_bytes: usize,
}

struct FrameBlockStats {
    first_block_bytes: usize,
    remaining_block_bytes: usize,
    first_literals_section_bytes: usize,
    first_sequence_section_bytes: usize,
    remaining_literals_section_bytes: usize,
    remaining_sequence_section_bytes: usize,
    first_sequence_count: usize,
    remaining_sequence_count: usize,
    first_match_bytes: usize,
    remaining_match_bytes: usize,
    block_count: usize,
}

#[derive(Clone, Copy)]
struct LiteralsHeader {
    header_size: usize,
    regenerated_size: usize,
    compressed_size: usize,
}

impl LiteralsHeader {
    fn payload_end(self) -> usize {
        self.header_size + self.compressed_size
    }
}

fn main() {
    let config = parse_config();
    let Some(helper) = helper_path() else {
        return;
    };

    let raw_dictionary = upstream_zstd::emit_raw_dictionary(helper);
    let trained_dictionary = upstream_zstd::emit_trained_dictionary(helper);

    let cases = vec![
        BenchCase {
            name: "plain-small-alphabet-l3",
            input: build_small_alphabet_pattern(256 * 1024),
            level: CompressionLevel::DEFAULT,
            checksum: false,
            dict_kind: DictKind::None,
        },
        BenchCase {
            name: "plain-repeated-l9",
            input: build_repeated_chunk_pattern(256 * 1024),
            level: CompressionLevel::BEST,
            checksum: false,
            dict_kind: DictKind::None,
        },
        BenchCase {
            name: "raw-dict-l3",
            input: build_raw_dictionary_input(256 * 1024),
            level: CompressionLevel::DEFAULT,
            checksum: false,
            dict_kind: DictKind::Raw,
        },
        BenchCase {
            name: "trained-dict-l3",
            input: build_trained_dictionary_input(256 * 1024),
            level: CompressionLevel::DEFAULT,
            checksum: false,
            dict_kind: DictKind::Trained,
        },
    ];

    println!(
        "case,input_bytes,rust_compressed,upstream_compressed,rust_ratio,upstream_ratio,rust_first_block_bytes,upstream_first_block_bytes,rust_remaining_block_bytes,upstream_remaining_block_bytes,rust_first_literals_bytes,upstream_first_literals_bytes,rust_first_sequence_bytes,upstream_first_sequence_bytes,rust_remaining_literals_bytes,upstream_remaining_literals_bytes,rust_remaining_sequence_bytes,upstream_remaining_sequence_bytes,rust_first_sequence_count,upstream_first_sequence_count,rust_remaining_sequence_count,upstream_remaining_sequence_count,rust_first_avg_match_bytes,upstream_first_avg_match_bytes,rust_remaining_avg_match_bytes,upstream_remaining_avg_match_bytes,rust_block_count,upstream_block_count,rust_encode_mib_s,upstream_encode_mib_s,rust_decode_mib_s,upstream_decode_mib_s"
    );

    for case in &cases {
        let (dictionary, compress_mode, bench_compress_mode_name, bench_decode_mode_name) =
            match case.dict_kind {
                DictKind::None => (
                    None,
                    "compress-regular-configured",
                    "bench-compress-regular",
                    "bench-decompress",
                ),
                DictKind::Raw => (
                    Some(raw_dictionary.as_slice()),
                    "compress-raw-dict-configured",
                    "bench-compress-raw-dict",
                    "bench-decompress-raw-dict",
                ),
                DictKind::Trained => (
                    Some(trained_dictionary.as_slice()),
                    "compress-trained-dict-configured",
                    "bench-compress-trained-dict",
                    "bench-decompress-trained-dict",
                ),
            };

        let rust_encoded = rust_encode_once(case, dictionary);
        let upstream_encoded = compress_once(
            helper,
            compress_mode,
            case.level.as_i32(),
            case.checksum,
            &case.input,
        );

        verify_roundtrip(case, dictionary, &rust_encoded, &upstream_encoded);
        let rust_blocks = parse_frame_block_stats(&rust_encoded);
        let upstream_blocks = parse_frame_block_stats(&upstream_encoded);

        let encode_iterations = choose_iterations(config.encode_target_bytes, case.input.len());
        let decode_iterations = choose_iterations(config.decode_target_bytes, case.input.len());

        let rust_encode_mib_s =
            rust_encode_mib_per_s(case, dictionary, encode_iterations, case.input.len());
        let upstream_encode_metrics = benchmark_compress_mode(
            helper,
            bench_compress_mode_name,
            encode_iterations,
            case.level.as_i32(),
            case.checksum,
            &case.input,
        );
        let upstream_encode_mib_s = mib_per_s(
            case.input.len() as u64 * encode_iterations as u64,
            upstream_encode_metrics.elapsed_ns,
        );
        assert_eq!(
            upstream_encode_metrics.last_output_size,
            upstream_encoded.len()
        );

        let rust_decode_mib_s =
            rust_decode_mib_per_s(case, dictionary, decode_iterations, &upstream_encoded);
        let upstream_decode_metrics = benchmark_mode(
            helper,
            bench_decode_mode_name,
            decode_iterations,
            &upstream_encoded,
        );
        let upstream_decode_mib_s = mib_per_s(
            upstream_decode_metrics.total_output_size,
            upstream_decode_metrics.elapsed_ns,
        );
        assert_eq!(upstream_decode_metrics.last_output_size, case.input.len());

        println!(
            "{},{},{},{},{:.4},{:.4},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{},{},{:.2},{:.2},{:.2},{:.2}",
            case.name,
            case.input.len(),
            rust_encoded.len(),
            upstream_encoded.len(),
            rust_encoded.len() as f64 / case.input.len() as f64,
            upstream_encoded.len() as f64 / case.input.len() as f64,
            rust_blocks.first_block_bytes,
            upstream_blocks.first_block_bytes,
            rust_blocks.remaining_block_bytes,
            upstream_blocks.remaining_block_bytes,
            rust_blocks.first_literals_section_bytes,
            upstream_blocks.first_literals_section_bytes,
            rust_blocks.first_sequence_section_bytes,
            upstream_blocks.first_sequence_section_bytes,
            rust_blocks.remaining_literals_section_bytes,
            upstream_blocks.remaining_literals_section_bytes,
            rust_blocks.remaining_sequence_section_bytes,
            upstream_blocks.remaining_sequence_section_bytes,
            rust_blocks.first_sequence_count,
            upstream_blocks.first_sequence_count,
            rust_blocks.remaining_sequence_count,
            upstream_blocks.remaining_sequence_count,
            average_match_bytes(
                rust_blocks.first_match_bytes,
                rust_blocks.first_sequence_count
            ),
            average_match_bytes(
                upstream_blocks.first_match_bytes,
                upstream_blocks.first_sequence_count
            ),
            average_match_bytes(
                rust_blocks.remaining_match_bytes,
                rust_blocks.remaining_sequence_count
            ),
            average_match_bytes(
                upstream_blocks.remaining_match_bytes,
                upstream_blocks.remaining_sequence_count
            ),
            rust_blocks.block_count,
            upstream_blocks.block_count,
            rust_encode_mib_s,
            upstream_encode_mib_s,
            rust_decode_mib_s,
            upstream_decode_mib_s
        );
    }
}

fn parse_config() -> BenchConfig {
    let quick = std::env::args().any(|arg| arg == "--quick");
    if quick {
        BenchConfig {
            encode_target_bytes: QUICK_ENCODE_TARGET_BYTES,
            decode_target_bytes: QUICK_DECODE_TARGET_BYTES,
        }
    } else {
        BenchConfig {
            encode_target_bytes: DEFAULT_ENCODE_TARGET_BYTES,
            decode_target_bytes: DEFAULT_DECODE_TARGET_BYTES,
        }
    }
}

fn choose_iterations(target_bytes: usize, unit_bytes: usize) -> usize {
    let unit_bytes = unit_bytes.max(1);
    let iterations = target_bytes / unit_bytes;
    iterations.clamp(3, 128)
}

fn rust_encode_once(case: &BenchCase, dictionary: Option<&[u8]>) -> Vec<u8> {
    let options = EncoderOptions {
        block_size: 128 * 1024,
        checksum: case.checksum,
        write_dict_id: true,
        compression_level: case.level,
        ..Default::default()
    };
    match dictionary {
        Some(dict) => encode_all_with_dict_and_options(&case.input, dict, options).unwrap(),
        None => encode_all_with_options(&case.input, options).unwrap(),
    }
}

fn rust_encode_mib_per_s(
    case: &BenchCase,
    dictionary: Option<&[u8]>,
    iterations: usize,
    input_size: usize,
) -> f64 {
    let options = EncoderOptions {
        block_size: 128 * 1024,
        checksum: case.checksum,
        write_dict_id: true,
        compression_level: case.level,
        ..Default::default()
    };
    let start = Instant::now();
    let mut total_output = 0usize;
    for _ in 0..iterations {
        let encoded = match dictionary {
            Some(dict) => {
                encode_all_with_dict_and_options(black_box(&case.input), black_box(dict), options)
                    .unwrap()
            }
            None => encode_all_with_options(black_box(&case.input), options).unwrap(),
        };
        total_output = total_output.wrapping_add(encoded.len());
        black_box(total_output);
    }
    mib_per_s((input_size * iterations) as u64, start.elapsed().as_nanos())
}

fn rust_decode_mib_per_s(
    case: &BenchCase,
    dictionary: Option<&[u8]>,
    iterations: usize,
    encoded: &[u8],
) -> f64 {
    let start = Instant::now();
    let mut total_output = 0usize;
    for _ in 0..iterations {
        let decoded = match dictionary {
            Some(dict) => decode_all_with_dict(black_box(encoded), black_box(dict)).unwrap(),
            None => decode_all(black_box(encoded)).unwrap(),
        };
        total_output = total_output.wrapping_add(decoded.len());
        black_box(total_output);
    }
    let _ = case;
    mib_per_s(total_output as u64, start.elapsed().as_nanos())
}

fn verify_roundtrip(
    case: &BenchCase,
    dictionary: Option<&[u8]>,
    rust_encoded: &[u8],
    upstream_encoded: &[u8],
) {
    let rust_decoded = match dictionary {
        Some(dict) => decode_all_with_dict(rust_encoded, dict).unwrap(),
        None => decode_all(rust_encoded).unwrap(),
    };
    assert_eq!(rust_decoded, case.input);

    let upstream_decoded = match dictionary {
        Some(dict) => decode_all_with_dict(upstream_encoded, dict).unwrap(),
        None => decode_all(upstream_encoded).unwrap(),
    };
    assert_eq!(upstream_decoded, case.input);
}

fn parse_frame_block_stats(frame: &[u8]) -> FrameBlockStats {
    assert!(frame.len() >= 5);
    assert_eq!(&frame[..4], &[0x28, 0xB5, 0x2F, 0xFD]);
    let frame_info = parse_frame_info(frame);
    let mut pos = frame_info.header_size;

    let mut block_count = 0usize;
    let mut first_block_bytes = 0usize;
    let mut remaining_block_bytes = 0usize;
    let mut first_literals_section_bytes = 0usize;
    let mut first_sequence_section_bytes = 0usize;
    let mut remaining_literals_section_bytes = 0usize;
    let mut remaining_sequence_section_bytes = 0usize;
    let mut first_sequence_count = 0usize;
    let mut remaining_sequence_count = 0usize;
    let mut first_match_bytes = 0usize;
    let mut remaining_match_bytes = 0usize;
    let mut remaining_content_size = frame_info.content_size;
    loop {
        assert!(pos + 3 <= frame.len());
        let block_header = u32::from(frame[pos])
            | (u32::from(frame[pos + 1]) << 8)
            | (u32::from(frame[pos + 2]) << 16);
        pos += 3;

        let last_block = block_header & 1 != 0;
        let block_type = (block_header >> 1) & 0x03;
        let block_size = (block_header >> 3) as usize;
        let stored_bytes = match block_type {
            0 | 2 => block_size,
            1 => 1,
            3 => panic!("reserved block type in benchmark frame"),
            _ => unreachable!(),
        };

        if block_count == 0 {
            first_block_bytes = stored_bytes;
        } else {
            remaining_block_bytes += stored_bytes;
        }

        if block_type == 2 {
            let payload = &frame[pos..pos + stored_bytes];
            let literals = parse_literals_header(payload);
            let literals_end = literals.payload_end();
            let literals_bytes = literals_end.min(payload.len());
            let sequence_bytes = payload.len().saturating_sub(literals_bytes);
            let sequence_count = decode_sequence_count(&payload[literals_end..]);
            let block_output_bytes = if last_block {
                remaining_content_size
            } else {
                frame_info.block_size_max.min(remaining_content_size)
            };
            let match_bytes = block_output_bytes.saturating_sub(literals.regenerated_size);
            if block_count == 0 {
                first_literals_section_bytes = literals_bytes;
                first_sequence_section_bytes = sequence_bytes;
                first_sequence_count = sequence_count;
                first_match_bytes = match_bytes;
            } else {
                remaining_literals_section_bytes += literals_bytes;
                remaining_sequence_section_bytes += sequence_bytes;
                remaining_sequence_count += sequence_count;
                remaining_match_bytes += match_bytes;
            }
        }
        block_count += 1;
        pos += stored_bytes;
        remaining_content_size = remaining_content_size.saturating_sub(if last_block {
            remaining_content_size
        } else {
            frame_info.block_size_max.min(remaining_content_size)
        });

        if last_block {
            break;
        }
    }

    FrameBlockStats {
        first_block_bytes,
        remaining_block_bytes,
        first_literals_section_bytes,
        first_sequence_section_bytes,
        remaining_literals_section_bytes,
        remaining_sequence_section_bytes,
        first_sequence_count,
        remaining_sequence_count,
        first_match_bytes,
        remaining_match_bytes,
        block_count,
    }
}

fn average_match_bytes(match_bytes: usize, sequence_count: usize) -> f64 {
    if sequence_count == 0 {
        0.0
    } else {
        match_bytes as f64 / sequence_count as f64
    }
}

#[derive(Clone, Copy)]
struct FrameInfo {
    header_size: usize,
    block_size_max: usize,
    content_size: usize,
}

fn parse_frame_info(frame: &[u8]) -> FrameInfo {
    let descriptor = frame[4];
    let single_segment = (descriptor >> 5) & 1 != 0;
    let dictionary_id_size = match descriptor & 0x03 {
        0 => 0usize,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => unreachable!(),
    };
    let frame_content_size_size = match (descriptor >> 6) & 0x03 {
        0 if single_segment => 1usize,
        0 => 0,
        1 => 2,
        2 => 4,
        3 => 8,
        _ => unreachable!(),
    };

    let mut pos = 5usize;
    let window_size = if single_segment {
        0usize
    } else {
        let descriptor = frame[pos];
        pos += 1;
        decode_window_descriptor(descriptor)
    };
    pos += dictionary_id_size;

    let content_size = if frame_content_size_size == 0 {
        0usize
    } else {
        let end = pos + frame_content_size_size;
        let size = decode_frame_content_size(&frame[pos..end]) as usize;
        pos = end;
        size
    };

    let window_size = if single_segment {
        content_size
    } else {
        window_size
    };

    FrameInfo {
        header_size: pos,
        block_size_max: window_size.min(128 * 1024),
        content_size,
    }
}

fn parse_literals_header(src: &[u8]) -> LiteralsHeader {
    let header0 = src[0];
    let block_type = header0 & 0x3;
    let size_format = (header0 >> 2) & 0x3;

    let (header_size, regenerated_size, compressed_size) = match block_type {
        0 | 1 => match size_format {
            0 | 2 => {
                let size = (header0 >> 3) as usize;
                (1, size, size)
            }
            1 => {
                let size = ((src[0] as usize) >> 4) | ((src[1] as usize) << 4);
                (2, size, size)
            }
            3 => {
                let value =
                    (src[0] as usize) | ((src[1] as usize) << 8) | ((src[2] as usize) << 16);
                let size = value >> 4;
                (3, size, size)
            }
            _ => unreachable!(),
        },
        2 | 3 => match size_format {
            0 | 1 => {
                let value =
                    (src[0] as usize) | ((src[1] as usize) << 8) | ((src[2] as usize) << 16);
                (3, (value >> 4) & 0x03ff, (value >> 14) & 0x03ff)
            }
            2 => {
                let value = u32::from_le_bytes([src[0], src[1], src[2], src[3]]) as usize;
                (4, (value >> 4) & 0x3fff, (value >> 18) & 0x3fff)
            }
            3 => {
                let value = (src[0] as u64)
                    | ((src[1] as u64) << 8)
                    | ((src[2] as u64) << 16)
                    | ((src[3] as u64) << 24)
                    | ((src[4] as u64) << 32);
                (
                    5,
                    ((value >> 4) & 0x3ffff) as usize,
                    ((value >> 22) & 0x3ffff) as usize,
                )
            }
            _ => unreachable!(),
        },
        _ => unreachable!(),
    };

    LiteralsHeader {
        header_size,
        regenerated_size,
        compressed_size,
    }
}

fn decode_sequence_count(src: &[u8]) -> usize {
    let byte0 = src[0] as usize;
    if byte0 < 128 {
        byte0
    } else if byte0 < 255 {
        ((byte0 - 128) << 8) + src[1] as usize
    } else {
        0x7F00 + src[1] as usize + ((src[2] as usize) << 8)
    }
}

fn decode_window_descriptor(descriptor: u8) -> usize {
    let exponent = descriptor >> 3;
    let mantissa = descriptor & 0x7;
    let window_log = 10usize + usize::from(exponent);
    let window_base = 1usize << window_log;
    let window_add = (window_base / 8) * usize::from(mantissa);
    window_base + window_add
}

fn decode_frame_content_size(field: &[u8]) -> u64 {
    match field.len() {
        1 => field[0] as u64,
        2 => u16::from_le_bytes([field[0], field[1]]) as u64 + 256,
        4 => u32::from_le_bytes([field[0], field[1], field[2], field[3]]) as u64,
        8 => u64::from_le_bytes([
            field[0], field[1], field[2], field[3], field[4], field[5], field[6], field[7],
        ]),
        _ => unreachable!(),
    }
}

fn mib_per_s(bytes: u64, elapsed_ns: u128) -> f64 {
    let seconds = elapsed_ns as f64 / 1_000_000_000.0;
    if seconds == 0.0 {
        return 0.0;
    }
    (bytes as f64 / MIB) / seconds
}

fn build_small_alphabet_pattern(size: usize) -> Vec<u8> {
    (0..size)
        .map(|index| match index & 0x0f {
            0..=8 => b'A',
            9..=12 => b'B',
            13..=14 => b'C',
            _ => b'D',
        })
        .collect()
}

fn build_repeated_chunk_pattern(size: usize) -> Vec<u8> {
    const CHUNK: &[u8] = b"zstd-rs-window-repcode-pattern-0123456789ABCDEF";

    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        let remaining = size - out.len();
        let take = remaining.min(CHUNK.len());
        out.extend_from_slice(&CHUNK[..take]);
    }
    out
}

fn build_raw_dictionary_input(size: usize) -> Vec<u8> {
    let statuses = ["active", "pending", "disabled"];
    let roles = ["admin", "analyst", "operator"];
    let regions = ["us-central", "us-east", "eu-west"];

    let mut out = Vec::with_capacity(size);
    let mut user_id = 1_000u32;
    while out.len() < size {
        let status = statuses[user_id as usize % statuses.len()];
        let role = roles[(user_id as usize / 2) % roles.len()];
        let region = regions[(user_id as usize / 3) % regions.len()];
        let record = format!(
            "GET /api/v1/users?id={user_id}&status={status} HTTP/1.1\r\n\
Host: example.internal\r\n\
Accept: application/json\r\n\
{{\"status\":\"{status}\",\"role\":\"{role}\",\"region\":\"{region}\"}}\n"
        );
        let remaining = size - out.len();
        out.extend_from_slice(&record.as_bytes()[..remaining.min(record.len())]);
        user_id += 1;
    }
    out
}

fn build_trained_dictionary_input(size: usize) -> Vec<u8> {
    let order_statuses = ["open", "closed", "pending"];
    let invoice_statuses = ["draft", "final", "paid"];
    let build_states = ["running", "passed", "failed"];
    let branches = ["main", "release", "hotfix"];
    let regions = ["us-east", "eu-west", "ap-south"];

    let mut out = Vec::with_capacity(size);
    let mut index = 0u32;
    while out.len() < size {
        let customer_id = 10_000 + index * 7;
        let project_id = 4_000 + index * 3;
        let build_id = 9_000 + index * 5;
        let status = order_statuses[index as usize % order_statuses.len()];
        let invoice_status = invoice_statuses[index as usize % invoice_statuses.len()];
        let build_state = build_states[index as usize % build_states.len()];
        let branch = branches[index as usize % branches.len()];
        let region = regions[index as usize % regions.len()];
        let record = match index % 3 {
            0 => format!(
                "GET /v2/customers/{customer_id}/orders?status={status}&limit=50\n\
{{\"customer_id\":{customer_id},\"status\":\"{status}\",\"region\":\"{region}\",\"items\":[{{\"sku\":\"A-{sku}\",\"qty\":{qty}}}]}}\n",
                sku = 100 + (index % 17),
                qty = 1 + (index % 4),
            ),
            1 => format!(
                "POST /v2/customers/{customer_id}/invoices\n\
{{\"customer_id\":{customer_id},\"currency\":\"USD\",\"total\":{total},\"status\":\"{invoice_status}\",\"region\":\"{region}\"}}\n",
                total = 1_500 + index * 11,
            ),
            _ => format!(
                "PATCH /v2/projects/{project_id}/builds/{build_id}\n\
{{\"project\":{project_id},\"build\":{build_id},\"state\":\"{build_state}\",\"branch\":\"{branch}\",\"artifact\":\"bundle.tar\"}}\n",
            ),
        };

        let remaining = size - out.len();
        out.extend_from_slice(&record.as_bytes()[..remaining.min(record.len())]);
        index += 1;
    }
    out
}
