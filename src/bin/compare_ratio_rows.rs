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
use upstream_zstd::{compress_once, emit_raw_dictionary, emit_trained_dictionary, require_helper};
use zstandard::{
    BlockType, CompressionLevel, EncoderDictionary, EncoderOptions, FrameHeader,
    encode_all_with_options, encode_all_with_prepared_dict_and_options, parse_block_header,
    parse_frame_header, parse_literals_section_layout,
};

const DEFAULT_INPUT_BYTES: usize = 512 * 1024;
const DEFAULT_TARGETS: &[(&str, u8)] = &[
    ("log-lines", 1),
    ("log-lines", 5),
    ("log-lines", 9),
    ("json-records", 3),
    ("json-records", 4),
    ("json-records", 5),
    ("json-records", 8),
    ("mixed-entropy", 2),
    ("mixed-entropy", 3),
    ("mixed-entropy", 4),
    ("mixed-entropy", 5),
    ("raw-dictionary", 3),
    ("trained-dictionary", 5),
    ("trained-dictionary", 9),
];

/// Which rows to compare, and at what body size.
///
/// The body size is a parameter rather than a constant because several level
/// parameters are only distinguishable by varying it. `windowLog` is the clear
/// example: levels 13 through 16 declare a 4 MiB window, so on a 4 MiB body the
/// window binds and evicts while on a 2 MiB body it never fills. Two rows that
/// look identical at one size can be told apart at another, and holding the
/// size fixed hides that entirely.
struct Args {
    targets: Vec<(String, u8)>,
    input_bytes: usize,
    dump_input: Option<String>,
    input_file: Option<InputFile>,
}

/// A body read from disk instead of generated, optionally a slice of one.
///
/// Generated cases can only be grown from their start, which conflates two
/// questions whenever a divergence appears late in a body: whether the tail is
/// harder to compress, or whether the compressor's behaviour depends on how far
/// into the stream it is. Feeding the same tail bytes back as a body of their
/// own separates them, and that needs an input the generator cannot produce.
struct InputFile {
    path: String,
    skip_bytes: usize,
}

fn parse_args() -> Args {
    let mut case_names: Vec<String> = Vec::new();
    let mut levels: Vec<u8> = Vec::new();
    let mut input_bytes = DEFAULT_INPUT_BYTES;
    let mut dump_input = None;
    let mut input_path = None;
    let mut skip_bytes = 0usize;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--case" => case_names.push(args.next().expect("--case requires a case name")),
            "--input-file" => {
                input_path = Some(args.next().expect("--input-file requires a path"));
            }
            "--skip-bytes" => {
                skip_bytes = args
                    .next()
                    .expect("--skip-bytes requires a value")
                    .parse()
                    .expect("--skip-bytes must be an integer");
            }
            "--levels" => {
                let value = args.next().expect("--levels requires a level list");
                levels.extend(parse_levels(&value));
            }
            "--input-bytes" => {
                input_bytes = args
                    .next()
                    .expect("--input-bytes requires a value")
                    .parse()
                    .expect("--input-bytes must be an integer");
            }
            "--dump-input" => {
                dump_input = Some(args.next().expect("--dump-input requires a path"));
            }
            _ => panic!("unknown argument: {arg}"),
        }
    }

    let input_file = input_path.map(|path| InputFile { path, skip_bytes });
    if let Some(input_file) = input_file {
        assert!(
            case_names.is_empty(),
            "--input-file replaces the generated corpus, so it cannot be combined with --case"
        );
        assert!(
            dump_input.is_none(),
            "--dump-input writes a generated body out; with --input-file the body is already a file"
        );
        assert!(
            !levels.is_empty(),
            "--input-file names a body to sweep, so at least one --levels is required"
        );
        let name = std::path::Path::new(&input_file.path)
            .file_name()
            .map_or_else(
                || input_file.path.clone(),
                |name| name.to_string_lossy().into_owned(),
            );
        return Args {
            targets: levels.iter().map(|&level| (name.clone(), level)).collect(),
            input_bytes,
            dump_input: None,
            input_file: Some(input_file),
        };
    }

    if case_names.is_empty() && levels.is_empty() {
        return Args {
            targets: DEFAULT_TARGETS
                .iter()
                .map(|&(case, level)| (case.to_string(), level))
                .collect(),
            input_bytes,
            dump_input,
            input_file: None,
        };
    }
    assert!(
        !case_names.is_empty(),
        "--levels selects levels within --case, so at least one --case is required"
    );
    assert!(
        !levels.is_empty(),
        "--case selects a case to sweep, so at least one --levels is required"
    );

    let mut targets = Vec::with_capacity(case_names.len() * levels.len());
    for case in &case_names {
        for &level in &levels {
            targets.push((case.clone(), level));
        }
    }
    Args {
        targets,
        input_bytes,
        dump_input,
        input_file: None,
    }
}

/// Accepts `13`, `13,16`, and `13-16`, so a family of adjacent levels can be
/// named the way it is usually thought about.
fn parse_levels(value: &str) -> Vec<u8> {
    let mut levels = Vec::new();
    for part in value.split(',').filter(|part| !part.is_empty()) {
        match part.split_once('-') {
            Some((start, end)) => {
                let start: u8 = start.parse().expect("level range start must be an integer");
                let end: u8 = end.parse().expect("level range end must be an integer");
                assert!(start <= end, "level range {part} runs backwards");
                levels.extend(start..=end);
            }
            None => levels.push(part.parse().expect("level must be an integer")),
        }
    }
    levels
}

fn main() {
    let args = parse_args();
    let helper = require_helper("compare_ratio_rows");
    let raw_dictionary = emit_raw_dictionary(helper);
    let trained_dictionary = emit_trained_dictionary(helper);
    let raw_prepared = EncoderDictionary::new(&raw_dictionary).expect("raw dictionary must parse");
    let trained_prepared =
        EncoderDictionary::new(&trained_dictionary).expect("trained dictionary must parse");
    let cases = match &args.input_file {
        None => benchmark_report_cases(args.input_bytes),
        Some(input_file) => {
            let bytes = std::fs::read(&input_file.path)
                .unwrap_or_else(|err| panic!("failed to read {}: {err}", input_file.path));
            assert!(
                input_file.skip_bytes < bytes.len(),
                "--skip-bytes {} is past the end of {} ({} bytes)",
                input_file.skip_bytes,
                input_file.path,
                bytes.len()
            );
            let body = bytes[input_file.skip_bytes..].to_vec();
            vec![corpora::CorpusCase {
                // Leaked because `BenchmarkCase` names are `&'static str` for
                // the generated corpus, where every name is a literal. One leak
                // per run of a diagnostic binary is not worth widening that
                // type for.
                name: Box::leak(args.targets[0].0.clone().into_boxed_str()),
                description: "body read from --input-file",
                input: body,
                dict_kind: DictKind::None,
            }]
        }
    };

    if let Some(path) = &args.dump_input {
        let case = cases
            .iter()
            .find(|case| Some(&case.name.to_string()) == args.targets.first().map(|(name, _)| name))
            .expect("--dump-input needs exactly one --case");
        std::fs::write(path, &case.input).expect("failed to write --dump-input file");
        println!(
            "wrote {} bytes of {} to {path}",
            case.input.len(),
            case.name
        );
        return;
    }

    for (case_name, level) in &args.targets {
        let (case_name, level) = (case_name.as_str(), *level);
        let case = cases
            .iter()
            .find(|case| case.name == case_name)
            .unwrap_or_else(|| panic!("missing case {case_name}"));
        let options = EncoderOptions {
            block_size: 128 * 1024,
            checksum: false,
            write_dict_id: true,
            compression_level: CompressionLevel::try_new(i32::from(level)).unwrap(),
            ..Default::default()
        };
        let rust = match case.dict_kind {
            DictKind::None => encode_all_with_options(&case.input, options).unwrap(),
            DictKind::Raw => {
                encode_all_with_prepared_dict_and_options(&case.input, &raw_prepared, options)
                    .unwrap()
            }
            DictKind::Trained => {
                encode_all_with_prepared_dict_and_options(&case.input, &trained_prepared, options)
                    .unwrap()
            }
        };
        let mode = match case.dict_kind {
            DictKind::None => "compress-regular-configured",
            DictKind::Raw => "compress-raw-dict-configured",
            DictKind::Trained => "compress-trained-dict-configured",
        };
        let upstream = compress_once(helper, mode, i32::from(level), false, &case.input);
        let rust_blocks = parse_compressed_block_stats(&rust);
        let upstream_blocks = parse_compressed_block_stats(&upstream);
        let first_divergent = rust_blocks
            .iter()
            .zip(upstream_blocks.iter())
            .position(|(a, b)| a != b);

        println!(
            "{case_name} L{level} in={}: rust_size={} upstream_size={} delta={} ({:+.2}%) first_divergent={:?}",
            case.input.len(),
            rust.len(),
            upstream.len(),
            rust.len() as isize - upstream.len() as isize,
            ((rust.len() as f64 - upstream.len() as f64) / upstream.len() as f64) * 100.0,
            first_divergent.map(|idx| (idx, rust_blocks[idx], upstream_blocks[idx])),
        );
        // Per block rather than per frame, because a frame-level excess says
        // nothing about whether the cost is spread evenly or concentrated in a
        // few blocks, and those are different defects.
        println!(
            "  {:>3}  {:>7} {:>7} {:>7}   {:>6} {:>6}   {:>6} {:>6}",
            "blk", "rust", "zstd", "delta", "r_lit", "z_lit", "r_seq", "z_seq"
        );
        for index in 0..rust_blocks.len().max(upstream_blocks.len()) {
            let rust_block = rust_blocks.get(index);
            let upstream_block = upstream_blocks.get(index);
            let field = |block: Option<&EncodedBlockStats>,
                         get: fn(&EncodedBlockStats) -> usize| {
                block.map_or_else(|| "-".to_string(), |block| get(block).to_string())
            };
            let delta = match (rust_block, upstream_block) {
                (Some(rust), Some(upstream)) => {
                    format!(
                        "{:+}",
                        rust.payload_size as isize - upstream.payload_size as isize
                    )
                }
                _ => "-".to_string(),
            };
            println!(
                "  {index:>3}  {:>7} {:>7} {delta:>7}   {:>6} {:>6}   {:>6} {:>6}",
                field(rust_block, |block| block.payload_size),
                field(upstream_block, |block| block.payload_size),
                field(rust_block, |block| block.literal_section_size),
                field(upstream_block, |block| block.literal_section_size),
                field(rust_block, |block| block.sequence_count),
                field(upstream_block, |block| block.sequence_count),
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EncodedBlockStats {
    block_type: BlockType,
    last_block: bool,
    payload_size: usize,
    literal_section_size: usize,
    sequence_section_size: usize,
    sequence_count: usize,
    modes: Option<(u8, u8, u8)>,
}

fn parse_compressed_block_stats(frame: &[u8]) -> Vec<EncodedBlockStats> {
    let header = match parse_frame_header(frame).expect("frame header should parse") {
        FrameHeader::Zstandard(header) => header,
        FrameHeader::Skippable(_) => panic!("unexpected skippable frame"),
    };
    let mut block_header_offset = header.header_size;
    let mut blocks = Vec::new();

    loop {
        let block =
            parse_block_header(&frame[block_header_offset..]).expect("block header should parse");
        let payload_start = block_header_offset + 3;
        let payload_end = payload_start + block_payload_size(block.block_type, block.block_size);
        let payload = &frame[payload_start..payload_end];
        blocks.push(parse_block_stats(
            block.block_type,
            block.last_block,
            payload,
        ));
        block_header_offset = payload_end;
        if block.last_block {
            break;
        }
    }

    blocks
}

fn block_payload_size(block_type: BlockType, block_size: u32) -> usize {
    match block_type {
        BlockType::Raw | BlockType::Rle | BlockType::Compressed => block_size as usize,
    }
}

fn parse_block_stats(block_type: BlockType, last_block: bool, payload: &[u8]) -> EncodedBlockStats {
    if block_type != BlockType::Compressed {
        return EncodedBlockStats {
            block_type,
            last_block,
            payload_size: payload.len(),
            literal_section_size: 0,
            sequence_section_size: 0,
            sequence_count: 0,
            modes: None,
        };
    }

    if payload.is_empty() {
        return EncodedBlockStats {
            block_type,
            last_block,
            payload_size: 0,
            literal_section_size: 0,
            sequence_section_size: 0,
            sequence_count: 0,
            modes: None,
        };
    }

    // Sized by the decoder's own parser rather than re-derived here. The
    // hand-rolled version this replaces read `Regenerated_Size` with the wrong
    // bit widths and then used it as the on-wire length, which for a compressed
    // literals section is `Compressed_Size` instead. Every compressed block
    // therefore fell into the bail-out below and reported
    // `literal_section_size == payload_size` with `sequence_count: 0`, so the
    // whole breakdown this tool exists to print was blank for exactly the
    // blocks worth looking at.
    let layout = match parse_literals_section_layout(payload) {
        Ok(layout) => layout,
        Err(_) => {
            return EncodedBlockStats {
                block_type,
                last_block,
                payload_size: payload.len(),
                literal_section_size: payload.len(),
                sequence_section_size: 0,
                sequence_count: 0,
                modes: None,
            };
        }
    };
    let literal_header_size = layout.header_size;
    let literal_size = layout.section_size - layout.header_size;

    if layout.section_size > payload.len() {
        return EncodedBlockStats {
            block_type,
            last_block,
            payload_size: payload.len(),
            literal_section_size: payload.len(),
            sequence_section_size: 0,
            sequence_count: 0,
            modes: None,
        };
    }

    let sequence_section = &payload[layout.section_size..];
    if sequence_section.is_empty() {
        return EncodedBlockStats {
            block_type,
            last_block,
            payload_size: payload.len(),
            literal_section_size: literal_header_size + literal_size,
            sequence_section_size: 0,
            sequence_count: 0,
            modes: None,
        };
    }
    let sequence_count = if sequence_section[0] == 0 {
        0
    } else if sequence_section[0] < 128 {
        sequence_section[0] as usize
    } else if sequence_section[0] < 255 {
        if sequence_section.len() < 2 {
            return EncodedBlockStats {
                block_type,
                last_block,
                payload_size: payload.len(),
                literal_section_size: literal_header_size + literal_size,
                sequence_section_size: sequence_section.len(),
                sequence_count: 0,
                modes: None,
            };
        }
        (((sequence_section[0] as usize) - 128) << 8) + sequence_section[1] as usize
    } else {
        if sequence_section.len() < 3 {
            return EncodedBlockStats {
                block_type,
                last_block,
                payload_size: payload.len(),
                literal_section_size: literal_header_size + literal_size,
                sequence_section_size: sequence_section.len(),
                sequence_count: 0,
                modes: None,
            };
        }
        (sequence_section[1] as usize) + ((sequence_section[2] as usize) << 8) + 0x7F00
    };

    let seq_count_header = if sequence_count == 0 || sequence_section[0] < 128 {
        1
    } else if sequence_section[0] < 255 {
        2
    } else {
        3
    };

    let modes = if sequence_count == 0 {
        None
    } else {
        if seq_count_header >= sequence_section.len() {
            return EncodedBlockStats {
                block_type,
                last_block,
                payload_size: payload.len(),
                literal_section_size: literal_header_size + literal_size,
                sequence_section_size: sequence_section.len(),
                sequence_count,
                modes: None,
            };
        }
        let bits = sequence_section[seq_count_header];
        Some((
            ((bits >> 6) & 0x3),
            ((bits >> 4) & 0x3),
            ((bits >> 2) & 0x3),
        ))
    };

    EncodedBlockStats {
        block_type,
        last_block,
        payload_size: payload.len(),
        literal_section_size: literal_header_size + literal_size,
        sequence_section_size: payload.len() - (literal_header_size + literal_size),
        sequence_count,
        modes,
    }
}
