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
use zstandard::{
    CompressionLevel, EncodeStageProfile, Encoder, EncoderDictionary, EncoderOptions,
    ParameterOverrides, PlannerPhases, RowMatchFinderMode,
};

/// Matches `DEFAULT_INPUT_BYTES` in `benchmark_report`, so a profile here
/// describes the same frame the report's encode column timed.
const DEFAULT_INPUT_BYTES: usize = 4 * 1024 * 1024;

struct Args {
    case: String,
    level: u8,
    iters: usize,
    input_bytes: usize,
    /// Profile only the first block, the way `benchmark_report`'s stage table
    /// does. Off by default, because the first block is the one block that can
    /// never reach the splitter: `savings` is still zero there. Kept as a flag
    /// so the report's own numbers stay reproducible from this tool.
    first_block_only: bool,
    /// Whether to also time each phase of the lazy parser. Off by default:
    /// the phase timers cost more than the work they measure once the parser
    /// is lazy rather than double-fast, and every number this tool prints is
    /// an absolute. Turn them on only to read the `planning_*` shares.
    phases: PlannerPhases,
    /// Run a dictionary case on its bytes without its dictionary, which moves
    /// it from the prefixed parser to the contiguous one.
    ///
    /// The two runs are **not** a controlled comparison. A dictionary frame's
    /// applied parameters come from the CDict -- upstream's
    /// `ZSTD_resetCCtx_byCopyingCDict` assigns `params.cParams =
    /// *cdict_cParams` -- so dropping the dictionary also drops the table
    /// geometry and the window down to whatever the source alone selects, and
    /// the frame that comes out is a different size. Pin the parameters with
    /// the overrides before reading anything into the difference.
    no_dict: bool,
    /// Force `ZSTD_c_useRowMatchFinder` off.
    ///
    /// A 4 MiB source resolves the mode to on at every level that supports it,
    /// so the chain parser is otherwise unreachable from here -- and the chain
    /// parser is what the dictionary cases run, for the reason above. Note that
    /// C's `ZSTD_ps_disable` and this do not currently produce the same frame;
    /// see the `Rust ratio` / `zstd ratio` columns in `BENCHMARKS.md`, which
    /// already differ on the default path at these levels.
    no_row_match_finder: bool,
}

fn parse_args() -> Args {
    let mut args = Args {
        case: String::from("binary-structured"),
        level: 4,
        iters: 20,
        input_bytes: DEFAULT_INPUT_BYTES,
        first_block_only: false,
        phases: PlannerPhases::Off,
        no_dict: false,
        no_row_match_finder: false,
    };
    let mut argv = std::env::args().skip(1);
    while let Some(arg) = argv.next() {
        match arg.as_str() {
            "--case" => args.case = argv.next().expect("--case requires a value"),
            "--level" => args.level = parse_usize(&mut argv, "--level") as u8,
            "--iters" => args.iters = parse_usize(&mut argv, "--iters"),
            "--input-bytes" => args.input_bytes = parse_usize(&mut argv, "--input-bytes"),
            "--first-block-only" => args.first_block_only = true,
            "--planner-phases" => args.phases = PlannerPhases::On,
            "--no-dict" => args.no_dict = true,
            "--no-row-match-finder" => args.no_row_match_finder = true,
            other => panic!("unknown argument {other}"),
        }
    }
    args
}

fn parse_usize(args: &mut impl Iterator<Item = String>, flag: &str) -> usize {
    args.next()
        .unwrap_or_else(|| panic!("{flag} requires a value"))
        .parse()
        .unwrap_or_else(|_| panic!("{flag} must be an integer"))
}

/// Everything the profiler did not attribute to a named stage: block headers,
/// frame assembly, the per-frame table fill. Reported rather than hidden, since
/// an unattributed remainder that grows is the signal that the stage boundaries
/// have stopped describing the encoder.
fn other(profile: &EncodeStageProfile) -> Duration {
    profile
        .total
        .saturating_sub(profile.block_split)
        .saturating_sub(profile.planning)
        .saturating_sub(profile.literals)
        .saturating_sub(profile.sequences)
}

fn main() {
    let args = parse_args();
    let cases = benchmark_report_cases(args.input_bytes);
    let case = cases
        .iter()
        .find(|case| case.name == args.case)
        .unwrap_or_else(|| panic!("unknown case {}", args.case));

    // The same options `profile_encode` uses, so a stage split here describes
    // the frame that tool times, and the same the instrumented upstream driver
    // sets: no checksum, content size declared.
    let options = EncoderOptions {
        checksum: false,
        write_dict_id: true,
        compression_level: CompressionLevel::try_new(i32::from(args.level))
            .expect("level must be a supported public compression level"),
        parameters: ParameterOverrides {
            use_row_match_finder: if args.no_row_match_finder {
                RowMatchFinderMode::Disabled
            } else {
                RowMatchFinderMode::Auto
            },
            ..Default::default()
        },
        ..Default::default()
    };
    let src = &case.input;

    // The dictionary cases have to be profiled *with* their dictionary. Handing
    // them to the no-dictionary entry point does not merely skip the dictionary,
    // it runs them down the contiguous parser instead of the prefixed one, and
    // `raw-dictionary` L4 then reads 1.52 ms/frame against the real path's 6.42.
    let dictionary = match case.dict_kind {
        _ if args.no_dict => None,
        DictKind::None => None,
        DictKind::Raw => Some(upstream_zstd::emit_raw_dictionary(
            upstream_zstd::helper_path().expect("dictionary cases need the upstream checkout"),
        )),
        DictKind::Trained => Some(upstream_zstd::emit_trained_dictionary(
            upstream_zstd::helper_path().expect("dictionary cases need the upstream checkout"),
        )),
    };
    let dictionary = dictionary
        .as_deref()
        .map(|dict| EncoderDictionary::new(dict).expect("dictionary fixture must parse"));

    let profile_frame = |encoder: &mut Encoder| match (dictionary.as_ref(), args.first_block_only) {
        (None, true) => encoder.profile_first_block_with_options(src, options, args.phases),
        (None, false) => encoder.profile_encode_all_with_options(src, options, args.phases),
        (Some(dict), true) => encoder.profile_first_block_with_prepared_dict_and_options(
            src,
            dict,
            options,
            args.phases,
        ),
        (Some(dict), false) => encoder.profile_encode_all_with_prepared_dict_and_options(
            src,
            dict,
            options,
            args.phases,
        ),
    };

    let mut encoder = Encoder::new();

    // One untimed frame, so allocation and first touch are not measured.
    let warm = profile_frame(&mut encoder).expect("profiling the warm-up frame");

    // Reported because a stage split is only a comparison while both sides are
    // encoding the same frame. The flags above can move the parse -- that is
    // what they are for -- and a length that stops matching upstream's is the
    // signal that the two harnesses have drifted apart.
    let encoded_len = match dictionary.as_ref() {
        None => encoder
            .encode_all_with_options(src, options)
            .expect("encoding the frame")
            .len(),
        Some(dict) => encoder
            .encode_all_with_prepared_dict_and_options(src, dict, options)
            .expect("encoding the frame")
            .len(),
    };

    let mut total = Duration::ZERO;
    let mut split = Duration::ZERO;
    let mut planning = Duration::ZERO;
    let mut literals = Duration::ZERO;
    let mut sequences = Duration::ZERO;
    let mut seq_codes = Duration::ZERO;
    let mut seq_stats = Duration::ZERO;
    let mut seq_bits = Duration::ZERO;
    let mut seq_assembly = Duration::ZERO;
    let mut rest = Duration::ZERO;
    for _ in 0..args.iters {
        let profile = profile_frame(&mut encoder).expect("profiling a frame");
        total += profile.total;
        split += profile.block_split;
        planning += profile.planning;
        literals += profile.literals;
        sequences += profile.sequences;
        seq_codes += profile.sequence_codes;
        seq_stats += profile.sequence_statistics;
        seq_bits += profile.sequence_bitstream;
        seq_assembly += profile.sequence_assembly;
        rest += other(&profile);
    }

    let ns = |duration: Duration| duration.as_nanos();
    println!("case {} level {}", case.name, args.level);
    println!("compressed_len {encoded_len}");
    println!("dictionary {}", u8::from(dictionary.is_some()));
    println!("row_match_finder {}", u8::from(!args.no_row_match_finder));
    println!("planner_phases {}", args.phases == PlannerPhases::On);
    println!("iters {}", args.iters);
    println!("blocks {}", warm.blocks * args.iters);
    println!("total_ns {}", ns(total));
    println!("split_ns {}", ns(split));
    println!("parser_ns {}", ns(planning));
    println!("lit_ns {}", ns(literals));
    println!("seq_ns {}", ns(sequences));
    println!("codes_ns {}", ns(seq_codes));
    println!("stats_ns {}", ns(seq_stats));
    println!("bits_ns {}", ns(seq_bits));
    println!("assembly_ns {}", ns(seq_assembly));
    println!("other_ns {}", ns(rest));
}
