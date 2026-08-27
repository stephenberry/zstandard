// This file is pulled in as a module by eight separate targets: the `interop`
// bench, the `upstream_interop` integration test, the `benchmark_report`,
// `profile_encode`, `profile_decode`, `profile_decode_stage`,
// `trace_bad_blocks` and `compare_ratio_rows` binaries, and the unit tests in
// `src/encode.rs`. Each compiles the whole module and uses a different subset
// of it, so every target sees the helpers it does not call as dead. That is a
// property of sharing one file across targets, not of the code, and it cannot
// be fixed by deleting anything: whatever one target stops using, another
// still needs.
//
// It lives under `src/` rather than beside the benches because one of those
// eight is `src/encode.rs`, whose tests therefore cannot compile unless this
// file ships in the published crate. Everything under `src/` ships; `benches/`
// is in the manifest's `exclude` list. There is deliberately no `mod support;`
// in `lib.rs` — this is not part of the library, only a file the library's
// tests and the workspace's tooling share.

use std::{
    env, fs,
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::OnceLock,
};

static HELPER: OnceLock<Option<PathBuf>> = OnceLock::new();

/// The upstream `zstd` commit-ish the parity tests and benchmarks are written
/// against, read from the one file CI and the local harness both consult.
///
/// Byte-exact comparisons against upstream only mean something at a known
/// version: upstream changes its level mapping, parser heuristics and block
/// splitter between releases, so the same input legitimately compresses to
/// different bytes across them. Pinning is what makes a local run and a CI run
/// comparable.
pub fn pinned_upstream_ref() -> &'static str {
    // Resolved relative to this file rather than to the including one, so the
    // path is the same for all eight inclusion sites: `../..` is the crate
    // root from `src/support/`. `upstream-zstd.ref` ships in the published
    // crate, which is what lets this compile from an unpacked tarball.
    include_str!("../../upstream-zstd.ref").trim()
}

/// Read an environment variable, treating empty as unset.
///
/// A GitHub Actions conditional expression yields `''` rather than omitting
/// the variable, so "set but empty" has to mean "off" for the workflow to be
/// able to enable these per matrix leg.
fn env_setting(name: &str) -> Option<String> {
    let value = env::var(name).ok()?;
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Where the upstream checkout may live, in the order the harness tries them.
///
/// `ZSTANDARD_UPSTREAM_DIR` names one explicitly and is then the only candidate,
/// because an explicit answer that is wrong should be reported rather than
/// quietly worked around.
///
/// Otherwise `upstream-zstd/` inside the crate comes first and `../zstd`
/// second. That order matters: `../zstd` is wherever the developer keeps
/// upstream for their own work, and there is no reason for it to sit at the
/// pinned ref — it drifts to whatever they last pulled. `upstream-zstd/` is
/// gitignored and is the layout CI creates, so preferring it makes a local run
/// compare against the same bytes CI does without anyone having to remember an
/// environment variable. The fallback stays because a checkout at `../zstd`
/// held at the pin is still a perfectly good answer.
fn upstream_dir_candidates() -> Vec<PathBuf> {
    if let Some(dir) = env_setting("ZSTANDARD_UPSTREAM_DIR") {
        return vec![PathBuf::from(dir)];
    }
    let crate_root = Path::new(env!("CARGO_MANIFEST_DIR"));
    vec![crate_root.join("upstream-zstd"), crate_root.join("../zstd")]
}

/// The upstream checkout other paths should be resolved against.
///
/// Prefers a candidate that sits at the pinned ref, so this and
/// [`locate_upstream`] never name different directories; falls back to the
/// first that exists, and finally to the first candidate so the caller has
/// something to put in an error message.
///
/// For callers that need to reach files *inside* the checkout, such as the
/// golden corpora under `tests/`. The parity harness itself goes through
/// [`locate_upstream`], so this is unused in the binaries that only compress.
#[allow(dead_code)]
pub fn upstream_dir() -> PathBuf {
    let candidates = upstream_dir_candidates();
    if let Some(pinned) = candidates
        .iter()
        .find(|dir| verify_pinned_ref(dir, pinned_upstream_ref()).is_ok())
    {
        return pinned.clone();
    }
    candidates
        .iter()
        .find(|dir| dir.exists())
        .cloned()
        .unwrap_or_else(|| candidates[0].clone())
}

fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

/// Check one candidate directory, describing in `Err` why it cannot be used.
fn verify_pinned_ref(dir: &Path, expected: &str) -> Result<(), String> {
    if !dir.exists() {
        return Err(format!(
            "no upstream checkout at {}; clone facebook/zstd there at {expected}, \
             or point ZSTANDARD_UPSTREAM_DIR at one",
            dir.display()
        ));
    }

    let Some(head) = git_stdout(dir, &["rev-parse", "HEAD"]) else {
        return Err(format!(
            "cannot read the git revision of {}, so it cannot be confirmed to be {expected}; \
             byte-exact comparisons against an unknown upstream are not meaningful",
            dir.display()
        ));
    };

    // Prefer comparing resolved commits. A shallow checkout made directly at a
    // tag may not have the tag object locally, so fall back to asking what tag
    // HEAD itself is.
    let matches = match git_stdout(dir, &["rev-parse", &format!("{expected}^{{commit}}")]) {
        Some(pinned) => pinned == head,
        None => git_stdout(dir, &["describe", "--tags", "--exact-match", "HEAD"])
            .is_some_and(|tag| tag == expected),
    };
    if !matches {
        let actual = git_stdout(dir, &["describe", "--tags", "--always", "--dirty"])
            .unwrap_or_else(|| head.clone());
        return Err(format!(
            "upstream checkout at {} is at {actual}, but this crate is pinned to {expected} \
             (see upstream-zstd.ref); results would not be comparable with CI",
            dir.display()
        ));
    }
    Ok(())
}

/// Resolve the upstream checkout, verifying it sits at the pinned ref.
///
/// Returns `Err` with a message describing what to do about it, rather than a
/// bare `None`: a parity test that quietly does nothing is worse than one that
/// fails, because it reports success without having compared anything. When
/// several candidates were tried, every rejection is reported, because "the
/// one you were thinking of is at the wrong revision" is the useful half of
/// the message and it is not always the first.
pub fn locate_upstream() -> Result<PathBuf, String> {
    let expected = pinned_upstream_ref();
    let mut rejections = Vec::new();
    for dir in upstream_dir_candidates() {
        match verify_pinned_ref(&dir, expected) {
            Ok(()) => return Ok(dir),
            Err(reason) => rejections.push(reason),
        }
    }
    Err(rejections.join("; and "))
}

/// Resolve the upstream checkout or explain why the caller should skip.
///
/// Skipping keeps `cargo test` green on a machine that has no pinned checkout,
/// which is the common case for a contributor touching decoder or API code.
/// CI sets `ZSTANDARD_REQUIRE_UPSTREAM` so that the same condition is a hard
/// failure there and the parity suite cannot silently stop running.
pub fn upstream_dir_or_skip(context: &str) -> Option<PathBuf> {
    // Every route to upstream runs a subprocess: `git` to verify the pinned
    // revision, `cc` to build the helper, then the helper itself. Miri cannot
    // spawn processes at all, so under Miri this is not a configuration
    // question and `ZSTANDARD_REQUIRE_UPSTREAM` cannot change the answer. Skipping
    // here rather than at each call site means the parity tests take the same
    // route they already take when no checkout is configured.
    if cfg!(miri) {
        let _ = writeln!(
            io::stderr(),
            "skipping {context}: Miri cannot spawn the upstream helper"
        );
        return None;
    }
    match locate_upstream() {
        Ok(dir) => Some(dir),
        Err(reason) => {
            if env_setting("ZSTANDARD_REQUIRE_UPSTREAM").is_some() {
                panic!(
                    "{context}: ZSTANDARD_REQUIRE_UPSTREAM is set and the pinned upstream \
                     checkout is unusable: {reason}"
                );
            }
            // Written to the stderr handle rather than with `eprintln!`,
            // because libtest captures the print macros and only replays them
            // for tests that fail. A skip that reports nothing is the failure
            // mode this whole check exists to prevent, so it has to be visible
            // on a passing run.
            let _ = writeln!(io::stderr(), "skipping {context}: {reason}");
            None
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct BenchTiming {
    pub elapsed_ns: u128,
    pub last_output_size: usize,
    pub total_output_size: u64,
}

pub struct BenchCompressOutput {
    pub timing: BenchTiming,
    pub encoded: Vec<u8>,
}

/// The long-distance-matching parameters upstream resolved, read off
/// `cctx->appliedParams.ldmParams` after a frame has been compressed.
///
/// Every field but `enabled` is derived when it is left unset, and the
/// derivation is ordered and interdependent, so this is the only way to say
/// what a given set of overrides actually became.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamAppliedLdmParams {
    /// `ZSTD_ParamSwitch_e`: 0 auto, 1 enabled, 2 disabled.
    pub enabled: u32,
    pub hash_log: u32,
    pub min_match_length: u32,
    pub bucket_size_log: u32,
    pub hash_rate_log: u32,
    pub window_log: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamAppliedCParams {
    pub window_log: u32,
    pub chain_log: u32,
    pub hash_log: u32,
    pub search_log: u32,
    pub min_match: u32,
    pub target_length: u32,
    pub strategy: u32,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamSequenceKind {
    Regular,
    Rep,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamSequenceSource {
    Dict,
    Prefix,
    Source,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamSequenceTrace {
    pub kind: UpstreamSequenceKind,
    pub source: UpstreamSequenceSource,
    pub start: usize,
    pub literal_length: usize,
    pub match_length: usize,
    pub off_base: usize,
    pub raw_offset: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamChainProbeVisit {
    pub index: usize,
    pub length: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamChainLinkSource {
    Dict,
    Prefix,
    Source,
    Unknown,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamChainLink {
    pub index: usize,
    pub source: UpstreamChainLinkSource,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamChainProbeWinner {
    Dict,
    Source,
    Unknown,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamChainProbeBackend {
    NoDict,
    ExtDict,
    DictMatchState,
    DedicatedDictSearch,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamChainRegularProbe {
    pub pos: usize,
    pub backend: UpstreamChainProbeBackend,
    pub hash_slot: usize,
    pub raw_head: usize,
    pub next_to_update: usize,
    pub low_limit: usize,
    pub min_chain: usize,
    pub chain_links: Vec<UpstreamChainLink>,
    pub source_head: usize,
    pub dict_head: usize,
    pub attempts_left_before_dict: usize,
    pub source_best_length: usize,
    pub source_best_offset: usize,
    pub dict_best_length: usize,
    pub dict_best_offset: usize,
    pub winner: UpstreamChainProbeWinner,
    pub winner_length: usize,
    pub winner_raw_offset: usize,
    pub winner_off_base: usize,
    pub source_visits: Vec<UpstreamChainProbeVisit>,
    pub dict_visits: Vec<UpstreamChainProbeVisit>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamLazyProbeMatchKind {
    None,
    Regular,
    Rep,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamNoDictRowLazyStopReason {
    None,
    NoBaseline,
    Depth0,
    Limit,
    NoRegularImprove,
}

#[allow(dead_code)]
pub const UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS: usize = 8;

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamExtDictProbeSource {
    None,
    Prefix,
    Source,
    Rep,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamExtDictLazyProbe {
    pub pos: usize,
    pub backend: UpstreamChainProbeBackend,
    pub anchor: usize,
    pub offset_1: usize,
    pub offset_2: usize,
    pub baseline_rep_length: usize,
    pub baseline_regular_source: UpstreamExtDictProbeSource,
    pub baseline_regular_length: usize,
    pub baseline_regular_off_base: usize,
    pub depth1_rep_length: usize,
    pub depth1_regular_source: UpstreamExtDictProbeSource,
    pub depth1_regular_length: usize,
    pub depth1_regular_off_base: usize,
    pub depth2_rep_length: usize,
    pub depth2_regular_source: UpstreamExtDictProbeSource,
    pub depth2_regular_length: usize,
    pub depth2_regular_off_base: usize,
    pub chosen_kind: UpstreamLazyProbeMatchKind,
    pub chosen_source: UpstreamExtDictProbeSource,
    pub chosen_start: usize,
    pub chosen_length: usize,
    pub chosen_off_base: usize,
    pub literal_length: usize,
    pub immediate_rep2_length: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamNoDictRowLazyProbe {
    pub pos: usize,
    pub backend: UpstreamChainProbeBackend,
    pub depth: usize,
    pub applied_window_log: usize,
    pub applied_hash_log: usize,
    pub applied_search_log: usize,
    pub applied_min_match: usize,
    pub applied_strategy: usize,
    pub applied_row_hash_log: usize,
    pub applied_row_log: usize,
    pub applied_dict_limit: usize,
    pub applied_hash_salt: u64,
    pub visited: bool,
    pub anchor: usize,
    pub offset_1: usize,
    pub offset_2: usize,
    pub baseline_rep_length: usize,
    pub baseline_regular_next_to_update: usize,
    pub baseline_regular_hash: usize,
    pub baseline_regular_rel_row: usize,
    pub baseline_regular_tag: usize,
    pub baseline_regular_low_limit: usize,
    pub baseline_regular_attempt_budget: usize,
    pub baseline_regular_head_index: usize,
    pub baseline_regular_insert_index: usize,
    pub baseline_regular_group_width: usize,
    pub baseline_regular_match_count: usize,
    pub baseline_regular_match_positions: [usize; 4],
    pub baseline_regular_match_indices: [usize; 4],
    pub baseline_regular_visit_count: usize,
    pub baseline_regular_visit_positions: [usize; 4],
    pub baseline_regular_visit_indices: [usize; 4],
    pub baseline_regular_visit_lengths: [usize; 4],
    pub baseline_regular_length: usize,
    pub baseline_regular_off_base: usize,
    pub depth1_rep_length: usize,
    pub depth1_regular_length: usize,
    pub depth1_regular_off_base: usize,
    pub depth2_rep_length: usize,
    pub depth2_regular_length: usize,
    pub depth2_regular_off_base: usize,
    pub chosen_kind: UpstreamLazyProbeMatchKind,
    pub chosen_start: usize,
    pub chosen_length: usize,
    pub chosen_off_base: usize,
    pub literal_length: usize,
    pub immediate_rep2_length: usize,
    pub continue_step_count: usize,
    pub continue_positions: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_rep_lengths: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_rep_improved: [bool; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_regular_lengths: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_regular_off_bases: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_regular_improved: [bool; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_current_kinds:
        [UpstreamLazyProbeMatchKind; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_current_starts: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_current_lengths: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub continue_current_off_bases: [usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS],
    pub stop_reason: UpstreamNoDictRowLazyStopReason,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamNoDictRowSearchProbe {
    pub state_pos: usize,
    pub probe_pos: usize,
    pub visited: bool,
    pub anchor: usize,
    pub offset_1: usize,
    pub offset_2: usize,
    pub next_to_update_before_search: usize,
    pub hash: usize,
    pub rel_row: usize,
    pub tag: usize,
    pub low_limit: usize,
    pub attempt_budget: usize,
    pub head_index: usize,
    pub insert_index: usize,
    pub group_width: usize,
    pub match_count: usize,
    pub match_positions: [usize; 4],
    pub match_indices: [usize; 4],
    pub visit_count: usize,
    pub visit_positions: [usize; 4],
    pub visit_indices: [usize; 4],
    pub visit_lengths: [usize; 4],
    pub visit_gate_passes: [bool; 4],
    pub visit_winner_lengths: [usize; 4],
    pub visit_winner_off_bases: [usize; 4],
    pub match_length: usize,
    pub off_base: usize,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamLiteralsBlockType {
    Raw,
    Rle,
    Compressed,
    Treeless,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UpstreamHuffmanTableMode {
    None,
    Raw4BitWeights,
    FseCompressedWeights,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamFirstBlockLiterals {
    pub block_type: UpstreamLiteralsBlockType,
    pub section_size: usize,
    pub literals_header_size: usize,
    pub regenerated_size: usize,
    pub compressed_size: usize,
    pub huffman_table_mode: UpstreamHuffmanTableMode,
    pub huffman_table_size: usize,
    pub payload_size: usize,
    pub section_bytes: Vec<u8>,
    pub section_prefix: Vec<u8>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpstreamSequenceModes {
    pub literal_lengths: u8,
    pub offsets: u8,
    pub match_lengths: u8,
}

#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamFirstBlockSections {
    pub last_block: bool,
    pub payload_size: usize,
    pub payload_bytes: Vec<u8>,
    pub payload_prefix: Vec<u8>,
    pub literals: UpstreamFirstBlockLiterals,
    pub sequence_section_size: usize,
    pub sequence_section_bytes: Vec<u8>,
    pub sequence_section_prefix: Vec<u8>,
    pub sequence_count: usize,
    pub sequence_modes: Option<UpstreamSequenceModes>,
}

pub fn helper_path() -> Option<&'static PathBuf> {
    HELPER.get_or_init(build_helper).as_ref()
}

/// The helper, or a panic naming the actual reason it is unavailable.
///
/// For the diagnostic binaries, which cannot do anything without upstream and
/// so have no skip path. They used to assert "requires sibling ../zstd
/// checkout", which names neither the real cause (usually a checkout at the
/// wrong revision) nor the override that fixes it.
#[allow(dead_code)]
pub fn require_helper(tool: &str) -> &'static PathBuf {
    if let Some(helper) = helper_path() {
        return helper;
    }
    match locate_upstream() {
        Ok(dir) => panic!(
            "{tool} needs the upstream helper, and the checkout at {} is at the pinned {}, \
             so the build of the helper itself failed; see the compiler output above",
            dir.display(),
            pinned_upstream_ref(),
        ),
        Err(reason) => panic!("{tool} needs the pinned upstream checkout: {reason}"),
    }
}

pub fn emit_raw_dictionary(helper: &Path) -> Vec<u8> {
    run_helper(helper, &["emit-raw-dict"], &[])
}

pub fn emit_trained_dictionary(helper: &Path) -> Vec<u8> {
    run_helper(helper, &["emit-trained-dict"], &[])
}

/// Upstream's `ZDICT_trainFromBuffer` over caller-supplied samples.
///
/// Unlike [`emit_trained_dictionary`], which trains on a corpus baked into the
/// helper, this trains on whatever the test provides, so a parity check can use
/// the same bytes on both sides without reproducing C's sample generation in
/// Rust.
#[allow(dead_code)]
pub fn train_dictionary(helper: &Path, capacity: usize, samples: &[&[u8]]) -> Vec<u8> {
    train_dictionary_inner(helper, capacity, samples, None)
}

/// Upstream's fastCover trainer pinned to one `(k, d)`.
///
/// With the search reduced to a single candidate the selected content no longer
/// depends on which candidate compressed the samples best, which is the only
/// way to compare content selection without the measurement half deciding the
/// answer.
#[allow(dead_code)]
pub fn train_dictionary_fixed(
    helper: &Path,
    capacity: usize,
    samples: &[&[u8]],
    k: u32,
    d: u32,
) -> Vec<u8> {
    train_dictionary_inner(helper, capacity, samples, Some((k, d)))
}

fn train_dictionary_inner(
    helper: &Path,
    capacity: usize,
    samples: &[&[u8]],
    fixed: Option<(u32, u32)>,
) -> Vec<u8> {
    let mut input = Vec::new();
    input.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for sample in samples {
        input.extend_from_slice(&(sample.len() as u32).to_le_bytes());
    }
    for sample in samples {
        input.extend_from_slice(sample);
    }
    let capacity = capacity.to_string();
    let mut args = vec!["train-dict", &capacity];
    let (k, d, steps);
    if let Some((fixed_k, fixed_d)) = fixed {
        k = fixed_k.to_string();
        d = fixed_d.to_string();
        steps = String::from("1");
        args.extend_from_slice(&[&k, &d, &steps]);
    }
    run_helper(helper, &args, &input)
}

/// Where the entropy header ends in `dictionary`, via upstream's
/// `ZDICT_getDictHeaderSize`.
///
/// Applied to both sides' dictionaries. The content is a prefix of the selected
/// material trimmed to whatever the header left over, so two dictionaries with
/// headers of different sizes hold different-length content and their tails are
/// not comparable — only their content prefixes are.
#[allow(dead_code)]
pub fn dictionary_header_size(helper: &Path, dictionary: &[u8]) -> usize {
    let stdout = run_helper(helper, &["dict-header-size"], dictionary);
    String::from_utf8(stdout)
        .expect("helper output is text")
        .trim()
        .parse()
        .expect("a header size")
}

/// Upstream's `ZDICT_analyzeEntropy` histograms for a given content and sample
/// set, as `(literals, offset codes, match lengths, literal lengths)`.
///
/// The point of instrumenting here rather than comparing finished dictionaries
/// is that a table is a lossy function of its histogram: two different
/// histograms routinely produce the same table, so a matching table proves less
/// than it appears to and a differing one does not say which input moved.
#[allow(dead_code)]
pub fn dictionary_entropy_stats(
    helper: &Path,
    level: i32,
    content: &[u8],
    samples: &[&[u8]],
) -> (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>) {
    let mut input = Vec::new();
    input.extend_from_slice(&(content.len() as u32).to_le_bytes());
    input.extend_from_slice(content);
    input.extend_from_slice(&(samples.len() as u32).to_le_bytes());
    for sample in samples {
        input.extend_from_slice(&(sample.len() as u32).to_le_bytes());
    }
    for sample in samples {
        input.extend_from_slice(sample);
    }
    let stdout = run_helper(helper, &["dict-entropy-stats", &level.to_string()], &input);
    let text = String::from_utf8(stdout).expect("helper stats are text");
    if env::var_os("ZSTANDARD_DEBUG_DICT_TRAIN").is_some() {
        eprintln!(
            "upstream {}",
            text.lines().next().unwrap_or("(no params line)")
        );
    }
    let row = |prefix: &str| -> Vec<u32> {
        text.lines()
            .find(|line| line.starts_with(&format!("{prefix} ")))
            .unwrap_or_else(|| panic!("helper did not report {prefix}:\n{text}"))
            .split_whitespace()
            .skip(1)
            .map(|value| value.parse().expect("a count"))
            .collect()
    };
    (row("lit"), row("off"), row("ml"), row("ll"))
}

/// Which dictionary the advanced modes load: `"none"`, `"raw"` or `"trained"`.
///
/// The same three the narrow `compress-*-configured` modes each hardcode, and
/// the same bytes: `"raw"` is [`emit_raw_dictionary`]'s content and `"trained"`
/// is [`emit_trained_dictionary`]'s.
#[allow(dead_code)]
pub const DICT_NONE: &str = "none";
#[allow(dead_code)]
pub const DICT_RAW: &str = "raw";
#[allow(dead_code)]
pub const DICT_TRAINED: &str = "trained";

/// Compress `input` with an arbitrary set of `ZSTD_c_*` settings.
///
/// `settings` are `name=value` strings — `"windowLog=15"`, `"strategy=9"`,
/// `"contentSizeFlag=0"`. The helper rejects a name it does not know rather
/// than ignoring it, so a typo fails the test instead of quietly comparing two
/// unconfigured frames. Anything not named keeps upstream's own default, which
/// is why this cannot be expressed through [`compress_once`]: that mode forces
/// `contentSizeFlag` on.
#[allow(dead_code)]
pub fn compress_advanced(
    helper: &Path,
    dict_mode: &str,
    settings: &[String],
    input: &[u8],
) -> Vec<u8> {
    run_helper(
        helper,
        &advanced_args("compress-advanced", dict_mode, settings),
        input,
    )
}

/// [`compress_advanced`] against `dictionary` rather than one of the helper's
/// three built-in dictionary modes.
///
/// What this exists for is comparing a `tests/baseline.rs` row against
/// upstream. That grid builds its own dictionaries so it can run with no helper
/// present, and they are much larger than the built-in ones -- large enough
/// that a CDict resolves a different strategy and window for the same level.
/// Reproducing such a row through [`compress_advanced`] is therefore not
/// possible: naming the same level asks upstream a different question.
#[allow(dead_code)]
pub fn compress_advanced_with_dict(
    helper: &Path,
    dictionary: &[u8],
    settings: &[String],
    input: &[u8],
) -> Vec<u8> {
    let mut framed = Vec::with_capacity(4 + dictionary.len() + input.len());
    framed.extend_from_slice(&(dictionary.len() as u32).to_le_bytes());
    framed.extend_from_slice(dictionary);
    framed.extend_from_slice(input);
    let mut args = vec!["compress-advanced-with-dict"];
    args.extend(settings.iter().map(String::as_str));
    run_helper(helper, &args, &framed)
}

/// [`trace_advanced_sequences`] against `dictionary` rather than one of the
/// helper's three built-in dictionary modes.
///
/// [`compress_advanced_with_dict`] answers "is our frame bigger", this answers
/// "where did the parse first go somewhere else". The two need the same
/// dictionary to be comparable, which is why this exists alongside it.
#[allow(dead_code)]
pub fn trace_advanced_sequences_with_dict(
    helper: &Path,
    dictionary: &[u8],
    settings: &[String],
    input: &[u8],
) -> String {
    let mut framed = Vec::with_capacity(4 + dictionary.len() + input.len());
    framed.extend_from_slice(&(dictionary.len() as u32).to_le_bytes());
    framed.extend_from_slice(dictionary);
    framed.extend_from_slice(input);
    let mut args = vec!["trace-advanced-sequences-with-dict"];
    args.extend(settings.iter().map(String::as_str));
    let output = run_helper(helper, &args, &framed);
    String::from_utf8(output).expect("sequence trace is text")
}

/// [`compress_advanced_with_dict`], driven through upstream's streaming API
/// `piece` bytes at a time.
#[allow(dead_code)]
pub fn compress_advanced_streaming_with_dict(
    helper: &Path,
    dictionary: &[u8],
    piece: usize,
    settings: &[String],
    input: &[u8],
) -> Vec<u8> {
    let mut framed = Vec::with_capacity(4 + dictionary.len() + input.len());
    framed.extend_from_slice(&(dictionary.len() as u32).to_le_bytes());
    framed.extend_from_slice(dictionary);
    framed.extend_from_slice(input);
    let piece_arg = piece.to_string();
    let mut args = vec!["compress-advanced-streaming-with-dict", &piece_arg];
    args.extend(settings.iter().map(String::as_str));
    run_helper(helper, &args, &framed)
}

/// [`compress_advanced`], driven through upstream's streaming API `piece` bytes
/// at a time.
///
/// The pairing of [`compress_streaming_once`] with `compress_advanced`: the
/// former can only ask for a level, and a one-shot frame is not the artifact a
/// streaming encoder produces.
#[allow(dead_code)]
pub fn compress_advanced_streaming(
    helper: &Path,
    dict_mode: &str,
    piece: usize,
    settings: &[String],
    input: &[u8],
) -> Vec<u8> {
    let piece_arg = piece.to_string();
    let mut args = vec!["compress-advanced-streaming", dict_mode, &piece_arg];
    args.extend(settings.iter().map(String::as_str));
    run_helper(helper, &args, input)
}

/// The sequences [`compress_advanced`] would parse, as raw helper lines.
///
/// `block-end <position> <literals>` marks a block boundary; every other line
/// is `<match start> <literal length> <match length> <offset code> <offset>`.
#[allow(dead_code)]
pub fn trace_advanced_sequences(
    helper: &Path,
    dict_mode: &str,
    settings: &[String],
    input: &[u8],
) -> String {
    let output = run_helper(
        helper,
        &advanced_args("trace-advanced-sequences", dict_mode, settings),
        input,
    );
    String::from_utf8(output).expect("sequence trace is text")
}

/// The compression parameters [`compress_advanced`] would apply, read back off
/// upstream's context after it has compressed with them.
#[allow(dead_code)]
pub fn trace_advanced_applied_cparams(
    helper: &Path,
    dict_mode: &str,
    settings: &[String],
    input: &[u8],
) -> UpstreamAppliedCParams {
    let output = run_helper(
        helper,
        &advanced_args("trace-advanced-cparams", dict_mode, settings),
        input,
    );
    parse_applied_cparams(&output)
}

/// The long-distance-matching parameters upstream resolved from `settings`.
///
/// [`trace_advanced_applied_cparams`] for the other half of the applied
/// parameters; these two are derived by different code and neither implies the
/// other.
#[allow(dead_code)]
pub fn trace_advanced_applied_ldm_params(
    helper: &Path,
    dict_mode: &str,
    settings: &[String],
    input: &[u8],
) -> UpstreamAppliedLdmParams {
    let output = run_helper(
        helper,
        &advanced_args("trace-advanced-ldm-params", dict_mode, settings),
        input,
    );
    let text = std::str::from_utf8(&output).expect("helper returned non-utf8 ldm params");
    let mut fields = text.split_whitespace().map(|field| {
        field
            .parse::<u32>()
            .unwrap_or_else(|_| panic!("invalid upstream ldm param field {field:?} in: {text}"))
    });
    let mut next = |name: &str| {
        fields
            .next()
            .unwrap_or_else(|| panic!("missing upstream ldm param {name} in: {text}"))
    };
    let resolved = UpstreamAppliedLdmParams {
        enabled: next("enabled"),
        hash_log: next("hash_log"),
        min_match_length: next("min_match_length"),
        bucket_size_log: next("bucket_size_log"),
        hash_rate_log: next("hash_rate_log"),
        window_log: next("window_log"),
    };
    assert!(
        fields.next().is_none(),
        "unexpected extra upstream ldm param fields: {text}"
    );
    resolved
}

/// [`trace_advanced_applied_cparams`] for the streaming path, which resolves its
/// parameters without a source size where the one-shot API resolves them with
/// one.
#[allow(dead_code)]
pub fn trace_advanced_streaming_applied_cparams(
    helper: &Path,
    dict_mode: &str,
    piece: usize,
    settings: &[String],
    input: &[u8],
) -> UpstreamAppliedCParams {
    let piece_arg = piece.to_string();
    let mut args = vec!["trace-advanced-streaming-cparams", dict_mode, &piece_arg];
    args.extend(settings.iter().map(String::as_str));
    let output = run_helper(helper, &args, input);
    parse_applied_cparams(&output)
}

#[allow(dead_code)]
fn advanced_args<'a>(mode: &'a str, dict_mode: &'a str, settings: &'a [String]) -> Vec<&'a str> {
    let mut args = vec![mode, dict_mode];
    args.extend(settings.iter().map(String::as_str));
    args
}

pub fn compress_once(
    helper: &Path,
    mode: &str,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> Vec<u8> {
    try_compress_once(helper, mode, level, checksum, input).unwrap_or_else(|error| {
        panic!("helper {mode} failed: {error}");
    })
}

pub fn try_compress_once(
    helper: &Path,
    mode: &str,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    try_run_helper(helper, &[mode, &level_arg, checksum_arg], input)
}

/// Upstream's streaming encoder over `input`, fed `piece` bytes at a time and
/// with no pledged source size.
///
/// This is the only mode that reports upstream's *streaming* block boundaries.
/// Every other compress mode here is one-shot, and one-shot output cannot
/// stand in: upstream's streaming frame declares a window instead of a content
/// size and may split the frame differently, so comparing our streaming output
/// against upstream's one-shot output measures two differences at once.
#[allow(dead_code)]
pub fn compress_streaming_once(
    helper: &Path,
    level: i32,
    checksum: bool,
    piece: usize,
    input: &[u8],
) -> Vec<u8> {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let piece_arg = piece.to_string();
    run_helper(
        helper,
        &[
            "compress-regular-streaming-configured",
            &level_arg,
            checksum_arg,
            &piece_arg,
        ],
        input,
    )
}

pub fn decompress_once(helper: &Path, mode: &str, input: &[u8]) -> Vec<u8> {
    run_helper(helper, &[mode], input)
}

pub fn benchmark_mode(helper: &Path, mode: &str, iterations: usize, input: &[u8]) -> BenchTiming {
    let iterations_arg = iterations.to_string();
    let output = run_helper(helper, &[mode, &iterations_arg], input);
    parse_bench_timing(&output)
}

#[allow(dead_code)]
pub fn benchmark_compress_mode(
    helper: &Path,
    mode: &str,
    iterations: usize,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> BenchTiming {
    let iterations_arg = iterations.to_string();
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let output = run_helper(
        helper,
        &[mode, &iterations_arg, &level_arg, checksum_arg],
        input,
    );
    parse_bench_timing(&output)
}

pub fn benchmark_compress_mode_with_output(
    helper: &Path,
    mode: &str,
    iterations: usize,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> BenchCompressOutput {
    let iterations_arg = iterations.to_string();
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let output = run_helper_output(
        helper,
        &[mode, &iterations_arg, &level_arg, checksum_arg],
        input,
    );
    BenchCompressOutput {
        timing: parse_bench_timing(&output.stderr),
        encoded: output.stdout,
    }
}

#[allow(dead_code)]
pub fn trace_trained_dict_sequences(
    helper: &Path,
    level: i32,
    checksum: bool,
    max_sequences: usize,
    input: &[u8],
) -> Vec<UpstreamSequenceTrace> {
    trace_sequences_configured(
        helper,
        "trace-trained-dict-sequences-configured",
        level,
        checksum,
        max_sequences,
        input,
    )
}

#[allow(dead_code)]
pub fn trace_raw_dict_sequences(
    helper: &Path,
    level: i32,
    checksum: bool,
    max_sequences: usize,
    input: &[u8],
) -> Vec<UpstreamSequenceTrace> {
    trace_sequences_configured(
        helper,
        "trace-raw-dict-sequences-configured",
        level,
        checksum,
        max_sequences,
        input,
    )
}

#[allow(dead_code)]
pub fn trace_regular_sequences(
    helper: &Path,
    level: i32,
    checksum: bool,
    max_sequences: usize,
    input: &[u8],
) -> Vec<UpstreamSequenceTrace> {
    trace_sequences_configured(
        helper,
        "trace-regular-sequences-configured",
        level,
        checksum,
        max_sequences,
        input,
    )
}

#[allow(dead_code)]
pub fn dump_cdict_bt_state(
    helper: &Path,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> std::collections::HashMap<String, String> {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let output = run_helper(
        helper,
        &["dump-cdict-bt-state", &level_arg, checksum_arg],
        input,
    );
    let text = String::from_utf8_lossy(&output);
    let mut map = std::collections::HashMap::new();
    for line in text.lines() {
        if let Some((key, value)) = line.split_once(' ') {
            map.insert(key.to_string(), value.to_string());
        }
    }
    map
}

pub fn trace_trained_dict_applied_cparams(
    helper: &Path,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> UpstreamAppliedCParams {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let output = run_helper(
        helper,
        &[
            "trace-trained-dict-cparams-configured",
            &level_arg,
            checksum_arg,
        ],
        input,
    );
    parse_applied_cparams(&output)
}

#[allow(dead_code)]
pub fn trace_regular_applied_cparams(
    helper: &Path,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> UpstreamAppliedCParams {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let output = run_helper(
        helper,
        &["trace-regular-cparams-configured", &level_arg, checksum_arg],
        input,
    );
    parse_applied_cparams(&output)
}

#[allow(dead_code)]
pub fn trace_trained_dict_hc_probe(
    helper: &Path,
    level: i32,
    checksum: bool,
    pos: usize,
    input: &[u8],
) -> UpstreamChainRegularProbe {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let pos_arg = pos.to_string();
    let output = run_helper(
        helper,
        &[
            "trace-trained-dict-hc-probe-configured",
            &level_arg,
            checksum_arg,
            &pos_arg,
        ],
        input,
    );
    parse_chain_probe(&output)
}

#[allow(dead_code)]
pub fn trace_trained_dict_extdict_lazy_probe(
    helper: &Path,
    level: i32,
    checksum: bool,
    pos: usize,
    input: &[u8],
) -> UpstreamExtDictLazyProbe {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let pos_arg = pos.to_string();
    let output = run_helper(
        helper,
        &[
            "trace-trained-dict-extdict-lazy-probe-configured",
            &level_arg,
            checksum_arg,
            &pos_arg,
        ],
        input,
    );
    parse_ext_dict_lazy_probe(&output)
}

#[allow(dead_code)]
pub fn trace_trained_dict_extdict_block_lazy_probe(
    helper: &Path,
    level: i32,
    checksum: bool,
    block_index: usize,
    pos: usize,
    input: &[u8],
) -> UpstreamExtDictLazyProbe {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let block_index_arg = block_index.to_string();
    let pos_arg = pos.to_string();
    let output = run_helper(
        helper,
        &[
            "trace-trained-dict-extdict-block-lazy-probe-configured",
            &level_arg,
            checksum_arg,
            &block_index_arg,
            &pos_arg,
        ],
        input,
    );
    parse_ext_dict_lazy_probe(&output)
}

#[allow(dead_code)]
pub fn trace_no_dict_row_lazy_probe(
    helper: &Path,
    level: i32,
    checksum: bool,
    pos: usize,
    input: &[u8],
) -> UpstreamNoDictRowLazyProbe {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let pos_arg = pos.to_string();
    let output = run_helper(
        helper,
        &[
            "trace-no-dict-row-lazy-probe-configured",
            &level_arg,
            checksum_arg,
            &pos_arg,
        ],
        input,
    );
    parse_no_dict_row_lazy_probe(&output)
}

#[allow(dead_code)]
pub fn trace_no_dict_row_search_probe(
    helper: &Path,
    level: i32,
    checksum: bool,
    state_pos: usize,
    probe_pos: usize,
    input: &[u8],
) -> UpstreamNoDictRowSearchProbe {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let state_pos_arg = state_pos.to_string();
    let probe_pos_arg = probe_pos.to_string();
    let output = run_helper(
        helper,
        &[
            "trace-no-dict-row-search-probe-configured",
            &level_arg,
            checksum_arg,
            &state_pos_arg,
            &probe_pos_arg,
        ],
        input,
    );
    parse_no_dict_row_search_probe(&output)
}

#[allow(dead_code)]
pub fn trace_raw_dict_extdict_double_fast_probe(
    helper: &Path,
    level: i32,
    checksum: bool,
    pos: usize,
    input: &[u8],
) -> String {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let pos_arg = pos.to_string();
    let output = run_helper(
        helper,
        &[
            "trace-raw-dict-extdict-double-fast-probe-configured",
            &level_arg,
            checksum_arg,
            &pos_arg,
        ],
        input,
    );
    String::from_utf8(output).expect("helper output must be utf8")
}

#[allow(dead_code)]
pub fn trace_first_block_literals(
    helper: &Path,
    mode: &str,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> UpstreamFirstBlockLiterals {
    let encoded = compress_once(helper, mode, level, checksum, input);
    parse_first_block_literals(&encoded)
}

#[allow(dead_code)]
pub fn trace_first_block_sections(
    helper: &Path,
    mode: &str,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> UpstreamFirstBlockSections {
    trace_compressed_block_sections(helper, mode, level, checksum, 0, input)
}

#[allow(dead_code)]
pub fn trace_compressed_block_sections(
    helper: &Path,
    mode: &str,
    level: i32,
    checksum: bool,
    block_index: usize,
    input: &[u8],
) -> UpstreamFirstBlockSections {
    let encoded = compress_once(helper, mode, level, checksum, input);
    parse_compressed_block_sections(&encoded, block_index)
}

#[allow(dead_code)]
pub fn trace_trained_dict_first_block_literals(
    helper: &Path,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> UpstreamFirstBlockLiterals {
    trace_first_block_literals(
        helper,
        "compress-trained-dict-configured",
        level,
        checksum,
        input,
    )
}

#[allow(dead_code)]
pub fn trace_trained_dict_first_block_sections(
    helper: &Path,
    level: i32,
    checksum: bool,
    input: &[u8],
) -> UpstreamFirstBlockSections {
    trace_compressed_block_sections(
        helper,
        "compress-trained-dict-configured",
        level,
        checksum,
        0,
        input,
    )
}

#[allow(dead_code)]
pub fn trace_trained_dict_block_sections(
    helper: &Path,
    level: i32,
    checksum: bool,
    block_index: usize,
    input: &[u8],
) -> UpstreamFirstBlockSections {
    trace_compressed_block_sections(
        helper,
        "compress-trained-dict-configured",
        level,
        checksum,
        block_index,
        input,
    )
}

#[allow(dead_code)]
pub fn trace_trained_dict_block_literals(
    helper: &Path,
    level: i32,
    checksum: bool,
    block_index: usize,
    input: &[u8],
) -> UpstreamFirstBlockLiterals {
    trace_trained_dict_block_sections(helper, level, checksum, block_index, input).literals
}

#[allow(dead_code)]
fn trace_sequences_configured(
    helper: &Path,
    mode: &str,
    level: i32,
    checksum: bool,
    max_sequences: usize,
    input: &[u8],
) -> Vec<UpstreamSequenceTrace> {
    let level_arg = level.to_string();
    let checksum_arg = if checksum { "1" } else { "0" };
    let max_sequences_arg = max_sequences.to_string();
    let output = run_helper(
        helper,
        &[mode, &level_arg, checksum_arg, &max_sequences_arg],
        input,
    );
    parse_sequence_trace(&output)
}

fn parse_first_block_literals(frame: &[u8]) -> UpstreamFirstBlockLiterals {
    parse_first_block_sections(frame).literals
}

#[allow(dead_code)]
pub fn parse_first_block_sections(frame: &[u8]) -> UpstreamFirstBlockSections {
    parse_compressed_block_sections(frame, 0)
}

fn parse_compressed_block_sections(frame: &[u8], block_index: usize) -> UpstreamFirstBlockSections {
    let (last_block, payload) = compressed_block_payload(frame, block_index);
    let header = parse_literals_header(&payload);
    let literals_section = &payload[..header.payload_end()];
    let sequence_section = &payload[header.payload_end()..];
    let (huffman_table_mode, huffman_table_size) =
        parse_huffman_table_mode(header.block_type, &payload[header.header_size..]);
    let sequence_count = decode_sequence_count(sequence_section);
    let sequence_modes = parse_sequence_modes(sequence_section, sequence_count);

    let literals = UpstreamFirstBlockLiterals {
        block_type: header.block_type,
        section_size: literals_section.len(),
        literals_header_size: header.header_size,
        regenerated_size: header.regenerated_size,
        compressed_size: header.compressed_size,
        huffman_table_mode,
        huffman_table_size,
        payload_size: header.compressed_size.saturating_sub(huffman_table_size),
        section_bytes: literals_section.to_vec(),
        section_prefix: literals_section[..literals_section.len().min(16)].to_vec(),
    };

    UpstreamFirstBlockSections {
        last_block,
        payload_size: payload.len(),
        payload_bytes: payload.clone(),
        payload_prefix: payload[..payload.len().min(16)].to_vec(),
        literals,
        sequence_section_size: sequence_section.len(),
        sequence_section_bytes: sequence_section.to_vec(),
        sequence_section_prefix: sequence_section[..sequence_section.len().min(16)].to_vec(),
        sequence_count,
        sequence_modes,
    }
}

fn compressed_block_payload(frame: &[u8], block_index: usize) -> (bool, Vec<u8>) {
    let header_size = frame_header_size(frame);
    let mut block_header_offset = header_size;

    for current_block_index in 0.. {
        let block_header = read_u24_le(&frame[block_header_offset..block_header_offset + 3]);
        let last_block = (block_header & 1) != 0;
        let block_type = (block_header >> 1) & 0x3;
        let block_size = block_header >> 3;
        let payload_start = block_header_offset + 3;
        let payload_end = payload_start + block_payload_size(block_type, block_size);

        if current_block_index == block_index {
            assert_eq!(
                block_type, 2,
                "expected block {block_index} to be compressed, got type {block_type}",
            );
            return (last_block, frame[payload_start..payload_end].to_vec());
        }

        assert!(
            !last_block,
            "requested compressed block {block_index}, but frame ended after block {current_block_index}",
        );
        block_header_offset = payload_end;
    }

    unreachable!()
}

fn block_payload_size(block_type: u32, block_size: u32) -> usize {
    match block_type {
        0 | 2 => block_size as usize,
        1 => 1,
        _ => panic!("unexpected block type: {block_type}"),
    }
}

fn frame_header_size(src: &[u8]) -> usize {
    const ZSTD_MAGIC_NUMBER: u32 = 0xFD2F_B528;
    assert!(src.len() >= 5, "frame too short");
    assert_eq!(
        read_u32_le(&src[..4]),
        ZSTD_MAGIC_NUMBER,
        "unexpected frame magic"
    );

    let descriptor = src[4];
    assert_eq!(descriptor & (1 << 3), 0, "frame reserved bit is set");

    let single_segment = descriptor & (1 << 5) != 0;
    let dict_id_size = match descriptor & 0x3 {
        0 => 0usize,
        1 => 1,
        2 => 2,
        3 => 4,
        _ => unreachable!(),
    };
    let fcs_size = frame_content_size_field_size(descriptor >> 6, single_segment);

    5 + usize::from(!single_segment) + dict_id_size + fcs_size
}

fn frame_content_size_field_size(flag: u8, single_segment: bool) -> usize {
    match flag {
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        3 => 8,
        _ => unreachable!(),
    }
}

fn read_u24_le(src: &[u8]) -> u32 {
    (src[0] as u32) | ((src[1] as u32) << 8) | ((src[2] as u32) << 16)
}

fn read_u32_le(src: &[u8]) -> u32 {
    u32::from_le_bytes([src[0], src[1], src[2], src[3]])
}

#[derive(Debug, Clone, Copy)]
struct LiteralsHeader {
    block_type: UpstreamLiteralsBlockType,
    header_size: usize,
    regenerated_size: usize,
    compressed_size: usize,
}

impl LiteralsHeader {
    fn payload_end(self) -> usize {
        self.header_size + self.compressed_size
    }
}

fn parse_literals_header(src: &[u8]) -> LiteralsHeader {
    let header0 = src[0];
    let block_type = match header0 & 0x3 {
        0 => UpstreamLiteralsBlockType::Raw,
        1 => UpstreamLiteralsBlockType::Rle,
        2 => UpstreamLiteralsBlockType::Compressed,
        3 => UpstreamLiteralsBlockType::Treeless,
        _ => unreachable!(),
    };
    let size_format = (header0 >> 2) & 0x3;

    let (header_size, regenerated_size, compressed_size) = match block_type {
        UpstreamLiteralsBlockType::Raw | UpstreamLiteralsBlockType::Rle => match size_format {
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
        UpstreamLiteralsBlockType::Compressed | UpstreamLiteralsBlockType::Treeless => {
            match size_format {
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
            }
        }
    };

    LiteralsHeader {
        block_type,
        header_size,
        regenerated_size,
        compressed_size,
    }
}

fn parse_sequence_modes(src: &[u8], sequence_count: usize) -> Option<UpstreamSequenceModes> {
    if sequence_count == 0 {
        return None;
    }

    let mode_index = if sequence_count < 128 {
        1
    } else if sequence_count < 0x7F00 {
        2
    } else {
        3
    };
    let modes = src[mode_index];
    Some(UpstreamSequenceModes {
        literal_lengths: (modes >> 6) & 0x3,
        offsets: (modes >> 4) & 0x3,
        match_lengths: (modes >> 2) & 0x3,
    })
}

fn decode_sequence_count(src: &[u8]) -> usize {
    let byte0 = src[0];
    if byte0 < 128 {
        byte0 as usize
    } else if byte0 < 255 {
        ((byte0 as usize - 128) << 8) + src[1] as usize
    } else {
        0x7F00 + src[1] as usize + ((src[2] as usize) << 8)
    }
}

fn parse_huffman_table_mode(
    block_type: UpstreamLiteralsBlockType,
    payload: &[u8],
) -> (UpstreamHuffmanTableMode, usize) {
    match block_type {
        UpstreamLiteralsBlockType::Compressed => {
            let descriptor = payload[0];
            if descriptor >= 128 {
                let highest_symbol = usize::from(descriptor - 127);
                (
                    UpstreamHuffmanTableMode::Raw4BitWeights,
                    1 + highest_symbol.div_ceil(2),
                )
            } else {
                (
                    UpstreamHuffmanTableMode::FseCompressedWeights,
                    1 + descriptor as usize,
                )
            }
        }
        UpstreamLiteralsBlockType::Treeless
        | UpstreamLiteralsBlockType::Raw
        | UpstreamLiteralsBlockType::Rle => (UpstreamHuffmanTableMode::None, 0),
    }
}

fn parse_bench_timing(output: &[u8]) -> BenchTiming {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 benchmark metrics");
    let mut fields = text.split_whitespace();
    let elapsed_ns = fields
        .next()
        .expect("missing elapsed_ns")
        .parse::<u128>()
        .expect("invalid elapsed_ns");
    let last_output_size = fields
        .next()
        .expect("missing last_output_size")
        .parse::<usize>()
        .expect("invalid last_output_size");
    let total_output_size = fields
        .next()
        .expect("missing total_output_size")
        .parse::<u64>()
        .expect("invalid total_output_size");
    BenchTiming {
        elapsed_ns,
        last_output_size,
        total_output_size,
    }
}

#[allow(dead_code)]
fn parse_sequence_trace(output: &[u8]) -> Vec<UpstreamSequenceTrace> {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 sequence trace");
    text.lines()
        .filter(|line| !line.is_empty())
        .map(|line| {
            let mut fields = line.split_whitespace();
            let kind = match fields.next().expect("missing sequence kind") {
                "regular" => UpstreamSequenceKind::Regular,
                "rep" => UpstreamSequenceKind::Rep,
                other => panic!("unexpected sequence kind: {other}"),
            };
            let source = match fields.next().expect("missing sequence source") {
                "dict" => UpstreamSequenceSource::Dict,
                "prefix" => UpstreamSequenceSource::Prefix,
                "source" => UpstreamSequenceSource::Source,
                other => panic!("unexpected sequence source: {other}"),
            };
            let start = fields
                .next()
                .expect("missing sequence start")
                .parse::<usize>()
                .expect("invalid sequence start");
            let literal_length = fields
                .next()
                .expect("missing sequence literal length")
                .parse::<usize>()
                .expect("invalid sequence literal length");
            let match_length = fields
                .next()
                .expect("missing sequence match length")
                .parse::<usize>()
                .expect("invalid sequence match length");
            let off_base = fields
                .next()
                .expect("missing sequence offbase")
                .parse::<usize>()
                .expect("invalid sequence offbase");
            let raw_offset = fields
                .next()
                .expect("missing sequence raw offset")
                .parse::<usize>()
                .expect("invalid sequence raw offset");
            assert!(
                fields.next().is_none(),
                "unexpected extra upstream sequence fields in line: {line}"
            );
            UpstreamSequenceTrace {
                kind,
                source,
                start,
                literal_length,
                match_length,
                off_base,
                raw_offset,
            }
        })
        .collect()
}

fn parse_applied_cparams(output: &[u8]) -> UpstreamAppliedCParams {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 cparams");
    let mut fields = text.split_whitespace();
    let window_log = fields
        .next()
        .expect("missing window_log")
        .parse::<u32>()
        .expect("invalid window_log");
    let chain_log = fields
        .next()
        .expect("missing chain_log")
        .parse::<u32>()
        .expect("invalid chain_log");
    let hash_log = fields
        .next()
        .expect("missing hash_log")
        .parse::<u32>()
        .expect("invalid hash_log");
    let search_log = fields
        .next()
        .expect("missing search_log")
        .parse::<u32>()
        .expect("invalid search_log");
    let min_match = fields
        .next()
        .expect("missing min_match")
        .parse::<u32>()
        .expect("invalid min_match");
    let target_length = fields
        .next()
        .expect("missing target_length")
        .parse::<u32>()
        .expect("invalid target_length");
    let strategy = fields
        .next()
        .expect("missing strategy")
        .parse::<u32>()
        .expect("invalid strategy");
    assert!(
        fields.next().is_none(),
        "unexpected extra upstream cparam fields: {text}"
    );
    UpstreamAppliedCParams {
        window_log,
        chain_log,
        hash_log,
        search_log,
        min_match,
        target_length,
        strategy,
    }
}

#[allow(dead_code)]
fn parse_chain_probe(output: &[u8]) -> UpstreamChainRegularProbe {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 chain probe");
    let mut pos = None;
    let mut backend = UpstreamChainProbeBackend::NoDict;
    let mut hash_slot = 0usize;
    let mut raw_head = 0usize;
    let mut next_to_update = 0usize;
    let mut low_limit = 0usize;
    let mut min_chain = 0usize;
    let mut chain_links = Vec::new();
    let mut source_head = 0usize;
    let mut dict_head = 0usize;
    let mut attempts_left_before_dict = 0usize;
    let mut source_best_length = 0usize;
    let mut source_best_offset = 0usize;
    let mut dict_best_length = 0usize;
    let mut dict_best_offset = 0usize;
    let mut winner = UpstreamChainProbeWinner::Unknown;
    let mut winner_length = 0usize;
    let mut winner_raw_offset = 0usize;
    let mut winner_off_base = 0usize;
    let mut source_visits = Vec::new();
    let mut dict_visits = Vec::new();

    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split_whitespace();
        let kind = fields.next().expect("missing chain probe field name");
        match kind {
            "probe" => {
                pos = Some(
                    fields
                        .next()
                        .expect("missing probe pos")
                        .parse::<usize>()
                        .expect("invalid probe pos"),
                );
            }
            "backend" => {
                backend = match fields.next().expect("missing backend") {
                    "nodict" => UpstreamChainProbeBackend::NoDict,
                    "extdict" => UpstreamChainProbeBackend::ExtDict,
                    "dictmatch" => UpstreamChainProbeBackend::DictMatchState,
                    "dds" => UpstreamChainProbeBackend::DedicatedDictSearch,
                    other => panic!("unexpected upstream chain probe backend: {other}"),
                };
            }
            "hash_slot" => {
                hash_slot = fields
                    .next()
                    .expect("missing hash_slot")
                    .parse::<usize>()
                    .expect("invalid hash_slot");
            }
            "raw_head" => {
                raw_head = fields
                    .next()
                    .expect("missing raw_head")
                    .parse::<usize>()
                    .expect("invalid raw_head");
            }
            "next_to_update" => {
                next_to_update = fields
                    .next()
                    .expect("missing next_to_update")
                    .parse::<usize>()
                    .expect("invalid next_to_update");
            }
            "low_limit" => {
                low_limit = fields
                    .next()
                    .expect("missing low_limit")
                    .parse::<usize>()
                    .expect("invalid low_limit");
            }
            "min_chain" => {
                min_chain = fields
                    .next()
                    .expect("missing min_chain")
                    .parse::<usize>()
                    .expect("invalid min_chain");
            }
            "chain_links" => {
                chain_links = parse_chain_links(fields.collect::<Vec<_>>().as_slice());
            }
            "source_head" => {
                source_head = fields
                    .next()
                    .expect("missing source_head")
                    .parse::<usize>()
                    .expect("invalid source_head");
            }
            "dict_head" => {
                dict_head = fields
                    .next()
                    .expect("missing dict_head")
                    .parse::<usize>()
                    .expect("invalid dict_head");
            }
            "attempts_before_dict" => {
                attempts_left_before_dict = fields
                    .next()
                    .expect("missing attempts_before_dict")
                    .parse::<usize>()
                    .expect("invalid attempts_before_dict");
            }
            "source_best" => {
                source_best_length = fields
                    .next()
                    .expect("missing source_best length")
                    .parse::<usize>()
                    .expect("invalid source_best length");
                source_best_offset = fields
                    .next()
                    .expect("missing source_best offset")
                    .parse::<usize>()
                    .expect("invalid source_best offset");
            }
            "dict_best" => {
                dict_best_length = fields
                    .next()
                    .expect("missing dict_best length")
                    .parse::<usize>()
                    .expect("invalid dict_best length");
                dict_best_offset = fields
                    .next()
                    .expect("missing dict_best offset")
                    .parse::<usize>()
                    .expect("invalid dict_best offset");
            }
            "winner" => {
                winner = match fields.next().expect("missing winner kind") {
                    "source" => UpstreamChainProbeWinner::Source,
                    "dict" => UpstreamChainProbeWinner::Dict,
                    "unknown" => UpstreamChainProbeWinner::Unknown,
                    other => panic!("unexpected upstream chain probe winner: {other}"),
                };
                winner_length = fields
                    .next()
                    .expect("missing winner length")
                    .parse::<usize>()
                    .expect("invalid winner length");
                winner_raw_offset = fields
                    .next()
                    .expect("missing winner raw offset")
                    .parse::<usize>()
                    .expect("invalid winner raw offset");
                winner_off_base = fields
                    .next()
                    .expect("missing winner offbase")
                    .parse::<usize>()
                    .expect("invalid winner offbase");
            }
            "source_visits" => {
                source_visits = parse_chain_probe_visits(fields.collect::<Vec<_>>().as_slice());
            }
            "dict_visits" => {
                dict_visits = parse_chain_probe_visits(fields.collect::<Vec<_>>().as_slice());
            }
            other => panic!("unexpected upstream chain probe field: {other}"),
        }
    }

    UpstreamChainRegularProbe {
        pos: pos.expect("missing probe pos"),
        backend,
        hash_slot,
        raw_head,
        next_to_update,
        low_limit,
        min_chain,
        chain_links,
        source_head,
        dict_head,
        attempts_left_before_dict,
        source_best_length,
        source_best_offset,
        dict_best_length,
        dict_best_offset,
        winner,
        winner_length,
        winner_raw_offset,
        winner_off_base,
        source_visits,
        dict_visits,
    }
}

fn parse_chain_probe_visits(fields: &[&str]) -> Vec<UpstreamChainProbeVisit> {
    fields
        .iter()
        .map(|field| {
            let (index, length) = field
                .split_once(':')
                .unwrap_or_else(|| panic!("invalid chain probe visit: {field}"));
            UpstreamChainProbeVisit {
                index: index
                    .parse::<usize>()
                    .expect("invalid chain probe visit index"),
                length: length
                    .parse::<usize>()
                    .expect("invalid chain probe visit length"),
            }
        })
        .collect()
}

#[allow(dead_code)]
fn parse_ext_dict_probe_source(field: &str) -> UpstreamExtDictProbeSource {
    match field {
        "none" => UpstreamExtDictProbeSource::None,
        "prefix" => UpstreamExtDictProbeSource::Prefix,
        "source" => UpstreamExtDictProbeSource::Source,
        "rep" => UpstreamExtDictProbeSource::Rep,
        other => panic!("unexpected upstream extdict probe source: {other}"),
    }
}

#[allow(dead_code)]
fn parse_ext_dict_lazy_probe(output: &[u8]) -> UpstreamExtDictLazyProbe {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 extdict lazy probe");
    let mut pos = None;
    let mut backend = UpstreamChainProbeBackend::NoDict;
    let mut anchor = 0usize;
    let mut offset_1 = 0usize;
    let mut offset_2 = 0usize;
    let mut baseline_rep_length = 0usize;
    let mut baseline_regular_source = UpstreamExtDictProbeSource::None;
    let mut baseline_regular_length = 0usize;
    let mut baseline_regular_off_base = 0usize;
    let mut depth1_rep_length = 0usize;
    let mut depth1_regular_source = UpstreamExtDictProbeSource::None;
    let mut depth1_regular_length = 0usize;
    let mut depth1_regular_off_base = 0usize;
    let mut depth2_rep_length = 0usize;
    let mut depth2_regular_source = UpstreamExtDictProbeSource::None;
    let mut depth2_regular_length = 0usize;
    let mut depth2_regular_off_base = 0usize;
    let mut chosen_kind = UpstreamLazyProbeMatchKind::None;
    let mut chosen_source = UpstreamExtDictProbeSource::None;
    let mut chosen_start = 0usize;
    let mut chosen_length = 0usize;
    let mut chosen_off_base = 0usize;
    let mut literal_length = 0usize;
    let mut immediate_rep2_length = 0usize;

    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split_whitespace();
        let kind = fields.next().expect("missing extdict lazy probe field");
        match kind {
            "probe" => {
                pos = Some(
                    fields
                        .next()
                        .expect("missing probe pos")
                        .parse::<usize>()
                        .expect("invalid probe pos"),
                );
            }
            "backend" => {
                backend = match fields.next().expect("missing backend") {
                    "nodict" => UpstreamChainProbeBackend::NoDict,
                    "extdict" => UpstreamChainProbeBackend::ExtDict,
                    "dictmatch" => UpstreamChainProbeBackend::DictMatchState,
                    "dds" => UpstreamChainProbeBackend::DedicatedDictSearch,
                    other => panic!("unexpected upstream extdict backend: {other}"),
                };
            }
            "anchor" => {
                anchor = fields
                    .next()
                    .expect("missing anchor")
                    .parse::<usize>()
                    .expect("invalid anchor");
            }
            "offsets" => {
                offset_1 = fields
                    .next()
                    .expect("missing offset_1")
                    .parse::<usize>()
                    .expect("invalid offset_1");
                offset_2 = fields
                    .next()
                    .expect("missing offset_2")
                    .parse::<usize>()
                    .expect("invalid offset_2");
            }
            "baseline_rep" => {
                baseline_rep_length = fields
                    .next()
                    .expect("missing baseline rep")
                    .parse::<usize>()
                    .expect("invalid baseline rep");
            }
            "baseline_regular" => {
                baseline_regular_source =
                    parse_ext_dict_probe_source(fields.next().expect("missing baseline source"));
                baseline_regular_length = fields
                    .next()
                    .expect("missing baseline regular length")
                    .parse::<usize>()
                    .expect("invalid baseline regular length");
                baseline_regular_off_base = fields
                    .next()
                    .expect("missing baseline regular offbase")
                    .parse::<usize>()
                    .expect("invalid baseline regular offbase");
            }
            "depth1_rep" => {
                depth1_rep_length = fields
                    .next()
                    .expect("missing depth1 rep")
                    .parse::<usize>()
                    .expect("invalid depth1 rep");
            }
            "depth1_regular" => {
                depth1_regular_source =
                    parse_ext_dict_probe_source(fields.next().expect("missing depth1 source"));
                depth1_regular_length = fields
                    .next()
                    .expect("missing depth1 regular length")
                    .parse::<usize>()
                    .expect("invalid depth1 regular length");
                depth1_regular_off_base = fields
                    .next()
                    .expect("missing depth1 regular offbase")
                    .parse::<usize>()
                    .expect("invalid depth1 regular offbase");
            }
            "depth2_rep" => {
                depth2_rep_length = fields
                    .next()
                    .expect("missing depth2 rep")
                    .parse::<usize>()
                    .expect("invalid depth2 rep");
            }
            "depth2_regular" => {
                depth2_regular_source =
                    parse_ext_dict_probe_source(fields.next().expect("missing depth2 source"));
                depth2_regular_length = fields
                    .next()
                    .expect("missing depth2 regular length")
                    .parse::<usize>()
                    .expect("invalid depth2 regular length");
                depth2_regular_off_base = fields
                    .next()
                    .expect("missing depth2 regular offbase")
                    .parse::<usize>()
                    .expect("invalid depth2 regular offbase");
            }
            "chosen" => {
                chosen_kind = match fields.next().expect("missing chosen kind") {
                    "none" => UpstreamLazyProbeMatchKind::None,
                    "regular" => UpstreamLazyProbeMatchKind::Regular,
                    "rep" => UpstreamLazyProbeMatchKind::Rep,
                    other => panic!("unexpected extdict chosen kind: {other}"),
                };
                chosen_source =
                    parse_ext_dict_probe_source(fields.next().expect("missing chosen source"));
                chosen_start = fields
                    .next()
                    .expect("missing chosen start")
                    .parse::<usize>()
                    .expect("invalid chosen start");
                chosen_length = fields
                    .next()
                    .expect("missing chosen length")
                    .parse::<usize>()
                    .expect("invalid chosen length");
                chosen_off_base = fields
                    .next()
                    .expect("missing chosen offbase")
                    .parse::<usize>()
                    .expect("invalid chosen offbase");
            }
            "literal_length" => {
                literal_length = fields
                    .next()
                    .expect("missing literal length")
                    .parse::<usize>()
                    .expect("invalid literal length");
            }
            "immediate_rep2" => {
                immediate_rep2_length = fields
                    .next()
                    .expect("missing immediate rep2")
                    .parse::<usize>()
                    .expect("invalid immediate rep2");
            }
            "unsupported" => {}
            other => panic!("unexpected upstream extdict lazy probe field: {other}"),
        }
    }

    UpstreamExtDictLazyProbe {
        pos: pos.expect("missing extdict lazy probe pos"),
        backend,
        anchor,
        offset_1,
        offset_2,
        baseline_rep_length,
        baseline_regular_source,
        baseline_regular_length,
        baseline_regular_off_base,
        depth1_rep_length,
        depth1_regular_source,
        depth1_regular_length,
        depth1_regular_off_base,
        depth2_rep_length,
        depth2_regular_source,
        depth2_regular_length,
        depth2_regular_off_base,
        chosen_kind,
        chosen_source,
        chosen_start,
        chosen_length,
        chosen_off_base,
        literal_length,
        immediate_rep2_length,
    }
}

#[allow(dead_code)]
fn parse_no_dict_row_lazy_probe(output: &[u8]) -> UpstreamNoDictRowLazyProbe {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 no-dict row probe");
    let mut pos = None;
    let mut backend = UpstreamChainProbeBackend::NoDict;
    let mut depth = 0usize;
    let mut applied_window_log = 0usize;
    let mut applied_hash_log = 0usize;
    let mut applied_search_log = 0usize;
    let mut applied_min_match = 0usize;
    let mut applied_strategy = 0usize;
    let mut applied_row_hash_log = 0usize;
    let mut applied_row_log = 0usize;
    let mut applied_dict_limit = 0usize;
    let mut applied_hash_salt = 0u64;
    let mut visited = false;
    let mut anchor = 0usize;
    let mut offset_1 = 0usize;
    let mut offset_2 = 0usize;
    let mut baseline_rep_length = 0usize;
    let mut baseline_regular_next_to_update = 0usize;
    let mut baseline_regular_hash = 0usize;
    let mut baseline_regular_rel_row = 0usize;
    let mut baseline_regular_tag = 0usize;
    let mut baseline_regular_low_limit = 0usize;
    let mut baseline_regular_attempt_budget = 0usize;
    let mut baseline_regular_head_index = 0usize;
    let mut baseline_regular_insert_index = 0usize;
    let mut baseline_regular_group_width = 0usize;
    let mut baseline_regular_match_count = 0usize;
    let mut baseline_regular_match_positions = [0usize; 4];
    let mut baseline_regular_match_indices = [0usize; 4];
    let mut baseline_regular_visit_count = 0usize;
    let mut baseline_regular_visit_positions = [0usize; 4];
    let mut baseline_regular_visit_indices = [0usize; 4];
    let mut baseline_regular_visit_lengths = [0usize; 4];
    let mut baseline_regular_length = 0usize;
    let mut baseline_regular_off_base = 0usize;
    let mut depth1_rep_length = 0usize;
    let mut depth1_regular_length = 0usize;
    let mut depth1_regular_off_base = 0usize;
    let mut depth2_rep_length = 0usize;
    let mut depth2_regular_length = 0usize;
    let mut depth2_regular_off_base = 0usize;
    let mut chosen_kind = UpstreamLazyProbeMatchKind::None;
    let mut chosen_start = 0usize;
    let mut chosen_length = 0usize;
    let mut chosen_off_base = 0usize;
    let mut literal_length = 0usize;
    let mut immediate_rep2_length = 0usize;
    let mut continue_step_count = 0usize;
    let mut continue_positions = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_rep_lengths = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_rep_improved = [false; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_regular_lengths = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_regular_off_bases = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_regular_improved = [false; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_current_kinds =
        [UpstreamLazyProbeMatchKind::None; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_current_starts = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_current_lengths = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut continue_current_off_bases = [0usize; UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS];
    let mut stop_reason = UpstreamNoDictRowLazyStopReason::None;

    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split_whitespace();
        let kind = fields.next().expect("missing no-dict row probe field");
        match kind {
            "probe" => {
                pos = Some(
                    fields
                        .next()
                        .expect("missing probe pos")
                        .parse::<usize>()
                        .expect("invalid probe pos"),
                );
            }
            "backend" => {
                backend = match fields.next().expect("missing backend") {
                    "nodict" => UpstreamChainProbeBackend::NoDict,
                    "extdict" => UpstreamChainProbeBackend::ExtDict,
                    "dictmatch" => UpstreamChainProbeBackend::DictMatchState,
                    "dds" => UpstreamChainProbeBackend::DedicatedDictSearch,
                    other => panic!("unexpected upstream row probe backend: {other}"),
                };
            }
            "depth" => {
                depth = fields
                    .next()
                    .expect("missing depth")
                    .parse::<usize>()
                    .expect("invalid depth");
            }
            "applied" => {
                applied_window_log = fields
                    .next()
                    .expect("missing applied window_log")
                    .parse::<usize>()
                    .expect("invalid applied window_log");
                applied_hash_log = fields
                    .next()
                    .expect("missing applied hash_log")
                    .parse::<usize>()
                    .expect("invalid applied hash_log");
                applied_search_log = fields
                    .next()
                    .expect("missing applied search_log")
                    .parse::<usize>()
                    .expect("invalid applied search_log");
                applied_min_match = fields
                    .next()
                    .expect("missing applied min_match")
                    .parse::<usize>()
                    .expect("invalid applied min_match");
                applied_strategy = fields
                    .next()
                    .expect("missing applied strategy")
                    .parse::<usize>()
                    .expect("invalid applied strategy");
                applied_row_hash_log = fields
                    .next()
                    .expect("missing applied row_hash_log")
                    .parse::<usize>()
                    .expect("invalid applied row_hash_log");
                applied_row_log = fields
                    .next()
                    .expect("missing applied row_log")
                    .parse::<usize>()
                    .expect("invalid applied row_log");
                applied_dict_limit = fields
                    .next()
                    .expect("missing applied dict_limit")
                    .parse::<usize>()
                    .expect("invalid applied dict_limit");
                applied_hash_salt = fields
                    .next()
                    .expect("missing applied hash_salt")
                    .parse::<u64>()
                    .expect("invalid applied hash_salt");
            }
            "visited" => {
                visited = match fields.next().expect("missing visited flag") {
                    "0" => false,
                    "1" => true,
                    other => panic!("unexpected visited flag: {other}"),
                };
            }
            "anchor" => {
                anchor = fields
                    .next()
                    .expect("missing anchor")
                    .parse::<usize>()
                    .expect("invalid anchor");
            }
            "offsets" => {
                offset_1 = fields
                    .next()
                    .expect("missing offset_1")
                    .parse::<usize>()
                    .expect("invalid offset_1");
                offset_2 = fields
                    .next()
                    .expect("missing offset_2")
                    .parse::<usize>()
                    .expect("invalid offset_2");
            }
            "baseline_rep" => {
                baseline_rep_length = fields
                    .next()
                    .expect("missing baseline rep")
                    .parse::<usize>()
                    .expect("invalid baseline rep");
            }
            "baseline_regular_state" => {
                baseline_regular_next_to_update = fields
                    .next()
                    .expect("missing baseline regular next_to_update")
                    .parse::<usize>()
                    .expect("invalid baseline regular next_to_update");
                baseline_regular_hash = fields
                    .next()
                    .expect("missing baseline regular hash")
                    .parse::<usize>()
                    .expect("invalid baseline regular hash");
                baseline_regular_rel_row = fields
                    .next()
                    .expect("missing baseline regular rel_row")
                    .parse::<usize>()
                    .expect("invalid baseline regular rel_row");
                baseline_regular_tag = fields
                    .next()
                    .expect("missing baseline regular tag")
                    .parse::<usize>()
                    .expect("invalid baseline regular tag");
                baseline_regular_low_limit = fields
                    .next()
                    .expect("missing baseline regular low_limit")
                    .parse::<usize>()
                    .expect("invalid baseline regular low_limit");
                baseline_regular_attempt_budget = fields
                    .next()
                    .expect("missing baseline regular attempt budget")
                    .parse::<usize>()
                    .expect("invalid baseline regular attempt budget");
                baseline_regular_head_index = fields
                    .next()
                    .expect("missing baseline regular head index")
                    .parse::<usize>()
                    .expect("invalid baseline regular head index");
                baseline_regular_insert_index = fields
                    .next()
                    .expect("missing baseline regular insert index")
                    .parse::<usize>()
                    .expect("invalid baseline regular insert index");
                baseline_regular_group_width = fields
                    .next()
                    .expect("missing baseline regular group width")
                    .parse::<usize>()
                    .expect("invalid baseline regular group width");
            }
            "baseline_regular_matches" => {
                for (slot, field) in fields.enumerate() {
                    let (match_pos, match_index) = field
                        .split_once(':')
                        .unwrap_or_else(|| panic!("invalid baseline regular match: {field}"));
                    if slot < baseline_regular_match_positions.len() {
                        baseline_regular_match_positions[slot] = match_pos
                            .parse::<usize>()
                            .expect("invalid baseline regular match position");
                        baseline_regular_match_indices[slot] = match_index
                            .parse::<usize>()
                            .expect("invalid baseline regular match index");
                    }
                    baseline_regular_match_count += 1;
                }
            }
            "baseline_regular_visits" => {
                for (slot, field) in fields.enumerate() {
                    let mut parts = field.split(':');
                    let match_pos = parts
                        .next()
                        .expect("missing baseline regular visit position")
                        .parse::<usize>()
                        .expect("invalid baseline regular visit position");
                    let match_index = parts
                        .next()
                        .expect("missing baseline regular visit index")
                        .parse::<usize>()
                        .expect("invalid baseline regular visit index");
                    let match_length = parts
                        .next()
                        .expect("missing baseline regular visit length")
                        .parse::<usize>()
                        .expect("invalid baseline regular visit length");
                    if slot < baseline_regular_visit_positions.len() {
                        baseline_regular_visit_positions[slot] = match_pos;
                        baseline_regular_visit_indices[slot] = match_index;
                        baseline_regular_visit_lengths[slot] = match_length;
                    }
                    baseline_regular_visit_count += 1;
                }
            }
            "baseline_regular" => {
                baseline_regular_length = fields
                    .next()
                    .expect("missing baseline regular length")
                    .parse::<usize>()
                    .expect("invalid baseline regular length");
                baseline_regular_off_base = fields
                    .next()
                    .expect("missing baseline regular offbase")
                    .parse::<usize>()
                    .expect("invalid baseline regular offbase");
            }
            "depth1_rep" => {
                depth1_rep_length = fields
                    .next()
                    .expect("missing depth1 rep")
                    .parse::<usize>()
                    .expect("invalid depth1 rep");
            }
            "depth1_regular" => {
                depth1_regular_length = fields
                    .next()
                    .expect("missing depth1 regular length")
                    .parse::<usize>()
                    .expect("invalid depth1 regular length");
                depth1_regular_off_base = fields
                    .next()
                    .expect("missing depth1 regular offbase")
                    .parse::<usize>()
                    .expect("invalid depth1 regular offbase");
            }
            "depth2_rep" => {
                depth2_rep_length = fields
                    .next()
                    .expect("missing depth2 rep")
                    .parse::<usize>()
                    .expect("invalid depth2 rep");
            }
            "depth2_regular" => {
                depth2_regular_length = fields
                    .next()
                    .expect("missing depth2 regular length")
                    .parse::<usize>()
                    .expect("invalid depth2 regular length");
                depth2_regular_off_base = fields
                    .next()
                    .expect("missing depth2 regular offbase")
                    .parse::<usize>()
                    .expect("invalid depth2 regular offbase");
            }
            "chosen" => {
                chosen_kind = match fields.next().expect("missing chosen kind") {
                    "none" => UpstreamLazyProbeMatchKind::None,
                    "regular" => UpstreamLazyProbeMatchKind::Regular,
                    "rep" => UpstreamLazyProbeMatchKind::Rep,
                    other => panic!("unexpected chosen kind: {other}"),
                };
                chosen_start = fields
                    .next()
                    .expect("missing chosen start")
                    .parse::<usize>()
                    .expect("invalid chosen start");
                chosen_length = fields
                    .next()
                    .expect("missing chosen length")
                    .parse::<usize>()
                    .expect("invalid chosen length");
                chosen_off_base = fields
                    .next()
                    .expect("missing chosen offbase")
                    .parse::<usize>()
                    .expect("invalid chosen offbase");
            }
            "literal_length" => {
                literal_length = fields
                    .next()
                    .expect("missing literal length")
                    .parse::<usize>()
                    .expect("invalid literal length");
            }
            "immediate_rep2" => {
                immediate_rep2_length = fields
                    .next()
                    .expect("missing immediate rep2")
                    .parse::<usize>()
                    .expect("invalid immediate rep2");
            }
            "continue_step" => {
                let step = fields
                    .next()
                    .expect("missing continue step index")
                    .parse::<usize>()
                    .expect("invalid continue step index");
                let pos = fields
                    .next()
                    .expect("missing continue step pos")
                    .parse::<usize>()
                    .expect("invalid continue step pos");
                let rep_length = fields
                    .next()
                    .expect("missing continue rep length")
                    .parse::<usize>()
                    .expect("invalid continue rep length");
                let rep_improved = match fields.next().expect("missing continue rep improved") {
                    "0" => false,
                    "1" => true,
                    other => panic!("unexpected continue rep improved flag: {other}"),
                };
                let regular_length = fields
                    .next()
                    .expect("missing continue regular length")
                    .parse::<usize>()
                    .expect("invalid continue regular length");
                let regular_off_base = fields
                    .next()
                    .expect("missing continue regular offbase")
                    .parse::<usize>()
                    .expect("invalid continue regular offbase");
                let regular_improved =
                    match fields.next().expect("missing continue regular improved") {
                        "0" => false,
                        "1" => true,
                        other => panic!("unexpected continue regular improved flag: {other}"),
                    };
                let current_kind = match fields.next().expect("missing continue current kind") {
                    "none" => UpstreamLazyProbeMatchKind::None,
                    "regular" => UpstreamLazyProbeMatchKind::Regular,
                    "rep" => UpstreamLazyProbeMatchKind::Rep,
                    other => panic!("unexpected continue current kind: {other}"),
                };
                let current_start = fields
                    .next()
                    .expect("missing continue current start")
                    .parse::<usize>()
                    .expect("invalid continue current start");
                let current_length = fields
                    .next()
                    .expect("missing continue current length")
                    .parse::<usize>()
                    .expect("invalid continue current length");
                let current_off_base = fields
                    .next()
                    .expect("missing continue current offbase")
                    .parse::<usize>()
                    .expect("invalid continue current offbase");
                if step < UPSTREAM_NO_DICT_ROW_LAZY_TRACE_MAX_STEPS {
                    continue_positions[step] = pos;
                    continue_rep_lengths[step] = rep_length;
                    continue_rep_improved[step] = rep_improved;
                    continue_regular_lengths[step] = regular_length;
                    continue_regular_off_bases[step] = regular_off_base;
                    continue_regular_improved[step] = regular_improved;
                    continue_current_kinds[step] = current_kind;
                    continue_current_starts[step] = current_start;
                    continue_current_lengths[step] = current_length;
                    continue_current_off_bases[step] = current_off_base;
                }
                continue_step_count = continue_step_count.max(step.saturating_add(1));
            }
            "stop_reason" => {
                stop_reason = match fields.next().expect("missing stop_reason") {
                    "none" => UpstreamNoDictRowLazyStopReason::None,
                    "no_baseline" => UpstreamNoDictRowLazyStopReason::NoBaseline,
                    "depth0" => UpstreamNoDictRowLazyStopReason::Depth0,
                    "limit" => UpstreamNoDictRowLazyStopReason::Limit,
                    "no_regular_improve" => UpstreamNoDictRowLazyStopReason::NoRegularImprove,
                    other => panic!("unexpected stop_reason: {other}"),
                };
            }
            other => panic!("unexpected upstream row probe field: {other}"),
        }
    }

    UpstreamNoDictRowLazyProbe {
        pos: pos.expect("missing no-dict row probe pos"),
        backend,
        depth,
        applied_window_log,
        applied_hash_log,
        applied_search_log,
        applied_min_match,
        applied_strategy,
        applied_row_hash_log,
        applied_row_log,
        applied_dict_limit,
        applied_hash_salt,
        visited,
        anchor,
        offset_1,
        offset_2,
        baseline_rep_length,
        baseline_regular_next_to_update,
        baseline_regular_hash,
        baseline_regular_rel_row,
        baseline_regular_tag,
        baseline_regular_low_limit,
        baseline_regular_attempt_budget,
        baseline_regular_head_index,
        baseline_regular_insert_index,
        baseline_regular_group_width,
        baseline_regular_match_count,
        baseline_regular_match_positions,
        baseline_regular_match_indices,
        baseline_regular_visit_count,
        baseline_regular_visit_positions,
        baseline_regular_visit_indices,
        baseline_regular_visit_lengths,
        baseline_regular_length,
        baseline_regular_off_base,
        depth1_rep_length,
        depth1_regular_length,
        depth1_regular_off_base,
        depth2_rep_length,
        depth2_regular_length,
        depth2_regular_off_base,
        chosen_kind,
        chosen_start,
        chosen_length,
        chosen_off_base,
        literal_length,
        immediate_rep2_length,
        continue_step_count,
        continue_positions,
        continue_rep_lengths,
        continue_rep_improved,
        continue_regular_lengths,
        continue_regular_off_bases,
        continue_regular_improved,
        continue_current_kinds,
        continue_current_starts,
        continue_current_lengths,
        continue_current_off_bases,
        stop_reason,
    }
}

#[allow(dead_code)]
fn parse_no_dict_row_search_probe(output: &[u8]) -> UpstreamNoDictRowSearchProbe {
    let text = std::str::from_utf8(output).expect("helper returned non-utf8 no-dict row search");
    let mut state_pos = None;
    let mut probe_pos = None;
    let mut visited = false;
    let mut anchor = 0usize;
    let mut offset_1 = 0usize;
    let mut offset_2 = 0usize;
    let mut next_to_update_before_search = 0usize;
    let mut hash = 0usize;
    let mut rel_row = 0usize;
    let mut tag = 0usize;
    let mut low_limit = 0usize;
    let mut attempt_budget = 0usize;
    let mut head_index = 0usize;
    let mut insert_index = 0usize;
    let mut group_width = 0usize;
    let mut match_count = 0usize;
    let mut match_positions = [0usize; 4];
    let mut match_indices = [0usize; 4];
    let mut visit_count = 0usize;
    let mut visit_positions = [0usize; 4];
    let mut visit_indices = [0usize; 4];
    let mut visit_lengths = [0usize; 4];
    let mut visit_gate_passes = [false; 4];
    let mut visit_winner_lengths = [0usize; 4];
    let mut visit_winner_off_bases = [0usize; 4];
    let mut match_length = 0usize;
    let mut off_base = 0usize;

    for line in text.lines().filter(|line| !line.is_empty()) {
        let mut fields = line.split_whitespace();
        let kind = fields.next().expect("missing no-dict row search field");
        match kind {
            "state_pos" => {
                state_pos = Some(
                    fields
                        .next()
                        .expect("missing state_pos")
                        .parse::<usize>()
                        .expect("invalid state_pos"),
                );
            }
            "probe_pos" => {
                probe_pos = Some(
                    fields
                        .next()
                        .expect("missing probe_pos")
                        .parse::<usize>()
                        .expect("invalid probe_pos"),
                );
            }
            "visited" => {
                visited = match fields.next().expect("missing visited flag") {
                    "0" => false,
                    "1" => true,
                    other => panic!("unexpected visited flag: {other}"),
                };
            }
            "anchor" => {
                anchor = fields
                    .next()
                    .expect("missing anchor")
                    .parse::<usize>()
                    .expect("invalid anchor");
            }
            "offsets" => {
                offset_1 = fields
                    .next()
                    .expect("missing offset_1")
                    .parse::<usize>()
                    .expect("invalid offset_1");
                offset_2 = fields
                    .next()
                    .expect("missing offset_2")
                    .parse::<usize>()
                    .expect("invalid offset_2");
            }
            "search_state" => {
                next_to_update_before_search = fields
                    .next()
                    .expect("missing next_to_update_before_search")
                    .parse::<usize>()
                    .expect("invalid next_to_update_before_search");
                hash = fields
                    .next()
                    .expect("missing hash")
                    .parse::<usize>()
                    .expect("invalid hash");
                rel_row = fields
                    .next()
                    .expect("missing rel_row")
                    .parse::<usize>()
                    .expect("invalid rel_row");
                tag = fields
                    .next()
                    .expect("missing tag")
                    .parse::<usize>()
                    .expect("invalid tag");
                low_limit = fields
                    .next()
                    .expect("missing low_limit")
                    .parse::<usize>()
                    .expect("invalid low_limit");
                attempt_budget = fields
                    .next()
                    .expect("missing attempt_budget")
                    .parse::<usize>()
                    .expect("invalid attempt_budget");
                head_index = fields
                    .next()
                    .expect("missing head_index")
                    .parse::<usize>()
                    .expect("invalid head_index");
                insert_index = fields
                    .next()
                    .expect("missing insert_index")
                    .parse::<usize>()
                    .expect("invalid insert_index");
                group_width = fields
                    .next()
                    .expect("missing group_width")
                    .parse::<usize>()
                    .expect("invalid group_width");
            }
            "search_matches" => {
                for (slot, field) in fields.enumerate() {
                    let (match_pos, match_index) = field
                        .split_once(':')
                        .unwrap_or_else(|| panic!("invalid search match: {field}"));
                    if slot < match_positions.len() {
                        match_positions[slot] = match_pos
                            .parse::<usize>()
                            .expect("invalid search match position");
                        match_indices[slot] = match_index
                            .parse::<usize>()
                            .expect("invalid search match index");
                    }
                    match_count += 1;
                }
            }
            "search_visits" => {
                for (slot, field) in fields.enumerate() {
                    let mut parts = field.split(':');
                    let match_pos = parts
                        .next()
                        .expect("missing search visit position")
                        .parse::<usize>()
                        .expect("invalid search visit position");
                    let match_index = parts
                        .next()
                        .expect("missing search visit index")
                        .parse::<usize>()
                        .expect("invalid search visit index");
                    let length = parts
                        .next()
                        .expect("missing search visit length")
                        .parse::<usize>()
                        .expect("invalid search visit length");
                    if slot < visit_positions.len() {
                        visit_positions[slot] = match_pos;
                        visit_indices[slot] = match_index;
                        visit_lengths[slot] = length;
                    }
                    visit_count += 1;
                }
            }
            "search_visit_states" => {
                for (slot, field) in fields.enumerate() {
                    let mut parts = field.split(':');
                    let gate_passed = match parts.next().expect("missing search visit gate pass") {
                        "0" => false,
                        "1" => true,
                        other => panic!("unexpected search visit gate pass: {other}"),
                    };
                    let winner_length = parts
                        .next()
                        .expect("missing search visit winner length")
                        .parse::<usize>()
                        .expect("invalid search visit winner length");
                    let winner_off_base = parts
                        .next()
                        .expect("missing search visit winner offbase")
                        .parse::<usize>()
                        .expect("invalid search visit winner offbase");
                    if slot < visit_gate_passes.len() {
                        visit_gate_passes[slot] = gate_passed;
                        visit_winner_lengths[slot] = winner_length;
                        visit_winner_off_bases[slot] = winner_off_base;
                    }
                }
            }
            "search_result" => {
                match_length = fields
                    .next()
                    .expect("missing match_length")
                    .parse::<usize>()
                    .expect("invalid match_length");
                off_base = fields
                    .next()
                    .expect("missing off_base")
                    .parse::<usize>()
                    .expect("invalid off_base");
            }
            other => panic!("unexpected no-dict row search field: {other}"),
        }
    }

    UpstreamNoDictRowSearchProbe {
        state_pos: state_pos.expect("missing state_pos"),
        probe_pos: probe_pos.expect("missing probe_pos"),
        visited,
        anchor,
        offset_1,
        offset_2,
        next_to_update_before_search,
        hash,
        rel_row,
        tag,
        low_limit,
        attempt_budget,
        head_index,
        insert_index,
        group_width,
        match_count,
        match_positions,
        match_indices,
        visit_count,
        visit_positions,
        visit_indices,
        visit_lengths,
        visit_gate_passes,
        visit_winner_lengths,
        visit_winner_off_bases,
        match_length,
        off_base,
    }
}

fn parse_chain_links(fields: &[&str]) -> Vec<UpstreamChainLink> {
    fields
        .iter()
        .map(|field| {
            let mut parts = field.split(':');
            let index = parts
                .next()
                .expect("missing chain link index")
                .parse::<usize>()
                .expect("invalid chain link index");
            let source = match parts.next().expect("missing chain link source") {
                "dict" => UpstreamChainLinkSource::Dict,
                "prefix" => UpstreamChainLinkSource::Prefix,
                "source" => UpstreamChainLinkSource::Source,
                "unknown" => UpstreamChainLinkSource::Unknown,
                other => panic!("unexpected chain link source: {other}"),
            };
            assert!(
                parts.next().is_none(),
                "unexpected extra chain link fields: {field}"
            );
            UpstreamChainLink { index, source }
        })
        .collect()
}

/// Compiler flags the helper is built with. Named because the cache key has to
/// cover them: a binary built with different flags is a different binary.
const HELPER_CFLAGS: [&str; 4] = [
    "-O2",
    "-std=c99",
    "-Wno-deprecated-declarations",
    "-DZSTD_DISABLE_ASM",
];

fn build_helper() -> Option<PathBuf> {
    let upstream = upstream_dir_or_skip("upstream interop harness")?;
    let Some(cc_version) = cc_version() else {
        eprintln!("skipping interop bench: cc is not available");
        return None;
    };

    let sources = upstream_c_sources(&upstream);

    // Cached across processes, keyed by everything that can change the binary.
    //
    // This used to key the directory on the process id, so every process that
    // touched the helper recompiled the whole of upstream's `lib/` first —
    // about four seconds each. That is charged to `cargo test`, to every
    // profiling binary, and to `benchmark_report`; a sweep that spawns thirty
    // processes spent minutes on it. Worse for the measurements it exists to
    // take, it ran a heavy parallel compile immediately before each timing run.
    let key = helper_cache_key(&upstream, &sources, &cc_version);
    let cache_dir = env::temp_dir().join("zstandard-upstream-helper");
    let binary_path = cache_dir.join(format!("helper-{key:016x}"));
    if binary_path.is_file() {
        return Some(binary_path);
    }

    if let Err(err) = fs::create_dir_all(&cache_dir) {
        panic!("failed to create helper cache directory {cache_dir:?}: {err}");
    }

    // Built under a private name and moved into place, so a concurrent process
    // either sees no helper or sees a complete one, never a half-linked file.
    // `rename` is atomic within a directory, and replacing a binary another
    // process is currently *executing* is safe in a way that writing over it in
    // place is not.
    let staging = cache_dir.join(format!("staging-{}", std::process::id()));
    if let Err(err) = fs::create_dir_all(&staging) {
        panic!("failed to create helper staging directory {staging:?}: {err}");
    }
    let source_path = staging.join("helper.c");
    let staged_binary = staging.join("helper");
    fs::write(&source_path, helper_source()).unwrap();

    let mut command = Command::new("cc");
    command
        .args(HELPER_CFLAGS)
        .arg(format!("-I{}", upstream.join("lib").display()))
        .arg(format!("-I{}", upstream.join("lib/common").display()))
        .arg(format!("-I{}", upstream.join("lib/compress").display()))
        .arg(format!("-I{}", upstream.join("lib/decompress").display()))
        .arg(format!("-I{}", upstream.join("lib/dictBuilder").display()))
        .arg(&source_path);
    for source in &sources {
        command.arg(source);
    }
    let status = command.arg("-o").arg(&staged_binary).status().unwrap();
    assert!(
        status.success(),
        "failed to build upstream zstd bench helper"
    );

    if let Err(err) = fs::rename(&staged_binary, &binary_path) {
        panic!("failed to publish helper to {binary_path:?}: {err}");
    }
    let _ = fs::remove_dir_all(&staging);

    Some(binary_path)
}

/// `cc --version`, or `None` when there is no C compiler to be had.
///
/// Doubles as the availability check, so the compiler is probed once rather
/// than once for the check and again for the cache key.
fn cc_version() -> Option<String> {
    let output = Command::new("cc")
        .arg("--version")
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// The upstream `.c` files the helper links, in a stable order.
fn upstream_c_sources(upstream: &Path) -> Vec<PathBuf> {
    let mut sources = collect_c_sources(&upstream.join("lib/common"), &[]);
    sources.extend(collect_c_sources(
        &upstream.join("lib/compress"),
        &["zstdmt_compress.c", "zstd_lazy.c", "zstd_compress.c"],
    ));
    sources.extend(collect_c_sources(&upstream.join("lib/decompress"), &[]));
    sources.extend(collect_c_sources(&upstream.join("lib/dictBuilder"), &[]));
    sources
}

/// Everything that can change the compiled helper, folded into one number.
///
/// The revision alone would not do it. A checkout can be at the pinned ref and
/// still have edits in the working tree, and reusing a stale binary against
/// edited C is exactly the failure this crate's parity numbers cannot survive —
/// they would be measured against something other than what is on disk. So the
/// key covers each source file's length and modification time as well, which
/// catches an edit without reading a few megabytes of C on every startup.
///
/// A hash collision, or a `DefaultHasher` that changes between toolchains,
/// costs a rebuild and nothing else.
fn helper_cache_key(upstream: &Path, sources: &[PathBuf], cc_version: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    upstream.hash(&mut hasher);
    cc_version.hash(&mut hasher);
    HELPER_CFLAGS.hash(&mut hasher);
    helper_source().hash(&mut hasher);
    // The headers are not in `sources`, and an edit to one changes the binary
    // without changing any file that is. The revision is what covers them on a
    // clean checkout; on a dirty one it does not, which is a limitation worth
    // naming rather than papering over with a full directory walk.
    git_stdout(upstream, &["rev-parse", "HEAD"]).hash(&mut hasher);
    git_stdout(upstream, &["status", "--porcelain"]).hash(&mut hasher);
    for source in sources {
        source.hash(&mut hasher);
        if let Ok(metadata) = fs::metadata(source) {
            metadata.len().hash(&mut hasher);
            if let Ok(modified) = metadata.modified() {
                if let Ok(since_epoch) = modified.duration_since(std::time::UNIX_EPOCH) {
                    since_epoch.as_nanos().hash(&mut hasher);
                }
            }
        }
    }
    hasher.finish()
}

fn run_helper(helper: &Path, args: &[&str], input: &[u8]) -> Vec<u8> {
    try_run_helper(helper, args, input).unwrap_or_else(|error| {
        panic!("helper {} failed: {error}", args.join(" "));
    })
}

fn run_helper_output(helper: &Path, args: &[&str], input: &[u8]) -> Output {
    try_run_helper_output(helper, args, input).unwrap_or_else(|error| {
        panic!("helper {} failed: {error}", args.join(" "));
    })
}

fn try_run_helper(
    helper: &Path,
    args: &[&str],
    input: &[u8],
) -> std::result::Result<Vec<u8>, String> {
    Ok(try_run_helper_output(helper, args, input)?.stdout)
}

fn try_run_helper_output(
    helper: &Path,
    args: &[&str],
    input: &[u8],
) -> std::result::Result<Output, String> {
    let command_name = args.join(" ");
    let mut child = Command::new(helper)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("spawn failed: {error}"))?;

    child
        .stdin
        .as_mut()
        .ok_or_else(|| "stdin unavailable".to_string())?
        .write_all(input)
        .map_err(|error| format!("stdin write failed: {error}"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| format!("wait failed: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "{command_name}: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(output)
}

fn collect_c_sources(dir: &Path, exclude: &[&str]) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(dir)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("c"))
        .filter(|path| {
            !exclude
                .iter()
                .any(|name| path.file_name().and_then(|file| file.to_str()) == Some(*name))
        })
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

fn helper_source() -> &'static str {
    r#"
#define _POSIX_C_SOURCE 200809L
#define ZSTD_STATIC_LINKING_ONLY
/* ZDICT_fastCover_params_t and ZDICT_optimizeTrainFromBuffer_fastCover, which
 * let the trainer be pinned to one (k, d) instead of searching. */
#define ZDICT_STATIC_LINKING_ONLY
#include "zstd.h"
#include "zstd_compress_internal.h"
#include "zstd_lazy.h"
#include "zdict.h"

#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "zstd_lazy.c"
#include "zstd_compress.c"

static unsigned char* read_all_stdin(size_t* size_out) {
    size_t cap = 4096;
    size_t size = 0;
    unsigned char* buf = (unsigned char*)malloc(cap ? cap : 1);
    if (buf == NULL) {
        fprintf(stderr, "malloc failed\n");
        exit(1);
    }

    for (;;) {
        size_t remaining = cap - size;
        size_t n = fread(buf + size, 1, remaining, stdin);
        size += n;
        if (n < remaining) {
            if (feof(stdin)) {
                *size_out = size;
                return buf;
            }
            fprintf(stderr, "stdin read failed\n");
            exit(1);
        }
        cap *= 2;
        unsigned char* next = (unsigned char*)realloc(buf, cap);
        if (next == NULL) {
            fprintf(stderr, "realloc failed\n");
            exit(1);
        }
        buf = next;
    }
}

static void write_all_stdout(const void* data, size_t size) {
    if (size == 0) {
        return;
    }
    if (fwrite(data, 1, size, stdout) != size) {
        fprintf(stderr, "stdout write failed\n");
        exit(1);
    }
}

static uint64_t now_ns(void) {
    struct timespec ts;
    clock_gettime(CLOCK_MONOTONIC, &ts);
    return ((uint64_t)ts.tv_sec * 1000000000ULL) + (uint64_t)ts.tv_nsec;
}

static const unsigned char RAW_DICT_BYTES[] =
    "GET /api/v1/users?id=123&status=active HTTP/1.1\r\n"
    "Host: example.internal\r\n"
    "Accept: application/json\r\n"
    "{\"status\":\"active\",\"role\":\"admin\",\"region\":\"us-central\"}\n";

static void write_raw_dict_extdict_double_fast_probe(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t target_pos
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_MatchState_t* ms;
    const ZSTD_compressionParameters* cParams;
    ZSTD_parameters params;
    const BYTE* const istart = (const BYTE*)src;
    const BYTE* ip = istart;
    const BYTE* anchor = istart;
    const BYTE* const iend = istart + src_size;
    const BYTE* const ilimit = iend - 8;
    const BYTE* base;
    const BYTE* dictBase;
    const BYTE* prefixStart;
    const BYTE* dictStart;
    const BYTE* dictEnd;
    U32 const* hashLong;
    U32 const* hashSmall;
    U32 hBitsL;
    U32 hBitsS;
    U32 endIndex;
    U32 lowLimit;
    U32 dictStartIndex;
    U32 dictLimit;
    U32 prefixStartIndex;
    U32 offset_1 = repStartValue[0];
    U32 offset_2 = repStartValue[1];

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (target_pos + MINMATCH > src_size) {
        fprintf(stderr, "probe position out of range\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    params.cParams = ZSTD_getCParams(level, src_size, sizeof(RAW_DICT_BYTES) - 1);
    params.fParams.contentSizeFlag = 1;
    params.fParams.checksumFlag = (unsigned)checksum;
    params.fParams.noDictIDFlag = 0;
    {
        size_t const begin_result = ZSTD_compressBegin_advanced(
            cctx,
            RAW_DICT_BYTES,
            sizeof(RAW_DICT_BYTES) - 1,
            params,
            src_size
        );
        if (ZSTD_isError(begin_result)) {
            fprintf(stderr, "compress begin failed: %s\n", ZSTD_getErrorName(begin_result));
            exit(2);
        }
    }

    ms = &cctx->blockState.matchState;
    if (!ZSTD_window_update(&ms->window, src, src_size, ms->forceNonContiguous)) {
        ms->forceNonContiguous = 0;
        ms->nextToUpdate = ms->window.dictLimit;
    }
    {
        size_t const maxDistance = 1U << cctx->appliedParams.cParams.windowLog;
        ZSTD_checkDictValidity(&ms->window, src + src_size, (U32)maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
        ZSTD_window_enforceMaxDist(&ms->window, src, (U32)maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
        if (ms->nextToUpdate < ms->window.lowLimit) {
            ms->nextToUpdate = ms->window.lowLimit;
        }
    }

    if (ZSTD_matchState_dictMode(ms) != ZSTD_extDict ||
        ms->cParams.strategy != ZSTD_dfast) {
        fprintf(stderr, "probe requires raw-dictionary extdict double-fast backend\n");
        exit(2);
    }

    cParams = &ms->cParams;
    printf(
        "applied %u %u %u %u %u %u\n",
        cParams->windowLog,
        cParams->chainLog,
        cParams->hashLog,
        cParams->searchLog,
        cParams->minMatch,
        cParams->strategy
    );
    hashLong = ms->hashTable;
    hBitsL = cParams->hashLog;
    hashSmall = ms->chainTable;
    hBitsS = cParams->chainLog;
    base = ms->window.base;
    dictBase = ms->window.dictBase;
    endIndex = (U32)((size_t)(istart - base) + src_size);
    lowLimit = ZSTD_getLowestMatchIndex(ms, endIndex, cParams->windowLog);
    dictStartIndex = lowLimit;
    dictLimit = ms->window.dictLimit;
    prefixStartIndex = (dictLimit > lowLimit) ? dictLimit : lowLimit;
    prefixStart = base + prefixStartIndex;
    dictStart = dictBase + dictStartIndex;
    dictEnd = dictBase + prefixStartIndex;

    printf("probe %zu\n", target_pos);
    printf("dict_limits %u %u %u\n", dictStartIndex, dictLimit, prefixStartIndex);
    {
        const BYTE* const targetIp = istart + target_pos;
        size_t const targetHSmall = ZSTD_hashPtr(targetIp, hBitsS, cParams->minMatch);
        size_t const targetHLong = ZSTD_hashPtr(targetIp, hBitsL, 8);
        U32 const targetMatchIndex = hashSmall[targetHSmall];
        U32 const targetMatchLongIndex = hashLong[targetHLong];
        const BYTE* const targetMatchBase =
            targetMatchIndex < prefixStartIndex ? dictBase : base;
        const BYTE* const targetMatchLongBase =
            targetMatchLongIndex < prefixStartIndex ? dictBase : base;
        int const targetShortValid =
            (targetMatchIndex > dictStartIndex)
            && (MEM_read32(targetMatchBase + targetMatchIndex) == MEM_read32(targetIp));
        int const targetLongValid =
            (targetMatchLongIndex > dictStartIndex)
            && (MEM_read64(targetMatchLongBase + targetMatchLongIndex) == MEM_read64(targetIp));
        printf(
            "initial_slot %zu %zu %u %u %d %d\n",
            targetHSmall,
            targetHLong,
            targetMatchIndex,
            targetMatchLongIndex,
            targetShortValid,
            targetLongValid
        );
    }

    while (ip < ilimit) {
        size_t const hSmall = ZSTD_hashPtr(ip, hBitsS, cParams->minMatch);
        U32 const matchIndex = hashSmall[hSmall];
        const BYTE* const matchBase = matchIndex < prefixStartIndex ? dictBase : base;
        const BYTE* match = matchBase + matchIndex;

        size_t const hLong = ZSTD_hashPtr(ip, hBitsL, 8);
        U32 const matchLongIndex = hashLong[hLong];
        const BYTE* const matchLongBase = matchLongIndex < prefixStartIndex ? dictBase : base;
        const BYTE* matchLong = matchLongBase + matchLongIndex;

        U32 const curr = (U32)(ip - base);
        U32 const repIndex = curr + 1 - offset_1;
        const BYTE* const repBase = repIndex < prefixStartIndex ? dictBase : base;
        const BYTE* const repMatch = repBase + repIndex;

        if ((size_t)(ip - istart) == target_pos || (size_t)(ip - istart) + 1 == target_pos) {
            size_t const h3 = ZSTD_hashPtr(ip + 1, hBitsL, 8);
            U32 const matchIndex3 = hashLong[h3];
            const BYTE* const match3Base = matchIndex3 < prefixStartIndex ? dictBase : base;
            const BYTE* const match3 = match3Base + matchIndex3;
            int const repValid = ((ZSTD_index_overlap_check(prefixStartIndex, repIndex))
                & (offset_1 <= curr + 1 - dictStartIndex))
                && (MEM_read32(repMatch) == MEM_read32(ip + 1));
            int const longValid =
                (matchLongIndex > dictStartIndex) && (MEM_read64(matchLong) == MEM_read64(ip));
            int const shortValid =
                (matchIndex > dictStartIndex) && (MEM_read32(match) == MEM_read32(ip));
            int const nextLongValid =
                (matchIndex3 > dictStartIndex) && (MEM_read64(match3) == MEM_read64(ip + 1));
            printf(
                "visit %zu %u %zu %zu %u %u %u %d %d %d %d\n",
                (size_t)(ip - istart),
                curr,
                hSmall,
                hLong,
                matchIndex,
                matchLongIndex,
                matchIndex3,
                repValid,
                shortValid,
                longValid,
                nextLongValid
            );
        }

        {
            int const repValid = ((ZSTD_index_overlap_check(prefixStartIndex, repIndex))
                & (offset_1 <= curr + 1 - dictStartIndex))
                && (MEM_read32(repMatch) == MEM_read32(ip + 1));
            int const longValid =
                (matchLongIndex > dictStartIndex) && (MEM_read64(matchLong) == MEM_read64(ip));
            int const shortValid =
                (matchIndex > dictStartIndex) && (MEM_read32(match) == MEM_read32(ip));
            size_t const h3 = ZSTD_hashPtr(ip + 1, hBitsL, 8);
            U32 const matchIndex3 = hashLong[h3];
            const BYTE* const match3Base = matchIndex3 < prefixStartIndex ? dictBase : base;
            const BYTE* const match3 = match3Base + matchIndex3;
            int const nextLongValid =
                (matchIndex3 > dictStartIndex) && (MEM_read64(match3) == MEM_read64(ip + 1));

            if (repValid && (size_t)(ip + 1 - istart) == target_pos) {
                size_t const repLength = ZSTD_count_2segments(
                    ip + 1 + 4,
                    repMatch + 4,
                    iend,
                    repIndex < prefixStartIndex ? dictEnd : iend,
                    prefixStart
                ) + 4;
                printf(
                    "state %u %u %u %zu %zu %u %u\n",
                    curr,
                    offset_1,
                    offset_2,
                    hSmall,
                    hLong,
                    matchIndex,
                    matchLongIndex
                );
                printf("chosen rep %zu %zu 1\n", target_pos, repLength);
                printf(
                    "rep %d %u %s %zu\n",
                    repValid,
                    repIndex,
                    repIndex < prefixStartIndex ? "dict" : "source",
                    repLength
                );
                printf(
                    "long %d %u %s %d\n",
                    longValid,
                    matchLongIndex,
                    matchLongIndex < prefixStartIndex ? "dict" : "source",
                    longValid
                );
                printf(
                    "short %d %u %s %d\n",
                    shortValid,
                    matchIndex,
                    matchIndex < prefixStartIndex ? "dict" : "source",
                    shortValid
                );
                printf(
                    "next_long %zu %d %u %s %d\n",
                    h3,
                    nextLongValid,
                    matchIndex3,
                    matchIndex3 < prefixStartIndex ? "dict" : "source",
                    nextLongValid
                );
                ZSTD_freeCCtx(cctx);
                return;
            }
        }

        ((U32*)hashSmall)[hSmall] = curr;
        ((U32*)hashLong)[hLong] = curr;

        if (((ZSTD_index_overlap_check(prefixStartIndex, repIndex))
            & (offset_1 <= curr+1 - dictStartIndex))
          && (MEM_read32(repMatch) == MEM_read32(ip+1)) ) {
            const BYTE* repMatchEnd = repIndex < prefixStartIndex ? dictEnd : iend;
            size_t const mLength =
                ZSTD_count_2segments(ip+1+4, repMatch+4, iend, repMatchEnd, prefixStart) + 4;
            ip += 1 + mLength;
            anchor = ip;
        } else if ((matchLongIndex > dictStartIndex) && (MEM_read64(matchLong) == MEM_read64(ip))) {
            const BYTE* const matchEnd = matchLongIndex < prefixStartIndex ? dictEnd : iend;
            const BYTE* const lowMatchPtr = matchLongIndex < prefixStartIndex ? dictStart : prefixStart;
            size_t mLength = ZSTD_count_2segments(ip+8, matchLong+8, iend, matchEnd, prefixStart) + 8;
            U32 const offset = curr - matchLongIndex;
            while (((ip>anchor) & (matchLong>lowMatchPtr)) && (ip[-1] == matchLong[-1])) { ip--; matchLong--; mLength++; }
            if ((size_t)(ip - istart) == target_pos) {
                printf(
                    "state %u %u %u %zu %zu %u %u\n",
                    curr,
                    offset_1,
                    offset_2,
                    hSmall,
                    hLong,
                    matchIndex,
                    matchLongIndex
                );
                printf(
                    "chosen long %zu %zu %u\n",
                    target_pos,
                    mLength,
                    offset + 3
                );
                printf(
                    "long %d %u %s %zu\n",
                    1,
                    matchLongIndex,
                    matchLongIndex < prefixStartIndex ? "dict" : "source",
                    mLength
                );
                ZSTD_freeCCtx(cctx);
                return;
            }
            offset_2 = offset_1;
            offset_1 = offset;
            ip += mLength;
            anchor = ip;
        } else if ((matchIndex > dictStartIndex) && (MEM_read32(match) == MEM_read32(ip))) {
            size_t const h3 = ZSTD_hashPtr(ip+1, hBitsL, 8);
            U32 const matchIndex3 = ((U32*)hashLong)[h3];
            const BYTE* const match3Base = matchIndex3 < prefixStartIndex ? dictBase : base;
            const BYTE* match3 = match3Base + matchIndex3;
            U32 offset;
            ((U32*)hashLong)[h3] = curr + 1;
            if ((matchIndex3 > dictStartIndex) && (MEM_read64(match3) == MEM_read64(ip+1))) {
                const BYTE* const matchEnd = matchIndex3 < prefixStartIndex ? dictEnd : iend;
                const BYTE* const lowMatchPtr = matchIndex3 < prefixStartIndex ? dictStart : prefixStart;
                size_t mLength = ZSTD_count_2segments(ip+9, match3+8, iend, matchEnd, prefixStart) + 8;
                ip++;
                offset = curr+1 - matchIndex3;
                while (((ip>anchor) & (match3>lowMatchPtr)) && (ip[-1] == match3[-1])) { ip--; match3--; mLength++; }
                if ((size_t)(ip - istart) == target_pos) {
                    printf(
                        "state %u %u %u %zu %zu %u %u\n",
                        curr,
                        offset_1,
                        offset_2,
                        hSmall,
                        hLong,
                        matchIndex,
                        matchLongIndex
                    );
                    printf(
                        "chosen next_long %zu %zu %u\n",
                        target_pos,
                        mLength,
                        offset + 3
                    );
                    printf(
                        "short %d %u %s %d\n",
                        1,
                        matchIndex,
                        matchIndex < prefixStartIndex ? "dict" : "source",
                        1
                    );
                    printf(
                        "next_long %zu %d %u %s %zu\n",
                        h3,
                        1,
                        matchIndex3,
                        matchIndex3 < prefixStartIndex ? "dict" : "source",
                        mLength
                    );
                    ZSTD_freeCCtx(cctx);
                    return;
                }
                offset_2 = offset_1;
                offset_1 = offset;
                ip += mLength;
                anchor = ip;
            } else {
                const BYTE* const matchEnd = matchIndex < prefixStartIndex ? dictEnd : iend;
                const BYTE* const lowMatchPtr = matchIndex < prefixStartIndex ? dictStart : prefixStart;
                size_t mLength = ZSTD_count_2segments(ip+4, match+4, iend, matchEnd, prefixStart) + 4;
                offset = curr - matchIndex;
                while (((ip>anchor) & (match>lowMatchPtr)) && (ip[-1] == match[-1])) { ip--; match--; mLength++; }
                if ((size_t)(ip - istart) == target_pos) {
                    printf(
                        "state %u %u %u %zu %zu %u %u\n",
                        curr,
                        offset_1,
                        offset_2,
                        hSmall,
                        hLong,
                        matchIndex,
                        matchLongIndex
                    );
                    printf(
                        "chosen short %zu %zu %u\n",
                        target_pos,
                        mLength,
                        offset + 3
                    );
                    printf(
                        "short %d %u %s %zu\n",
                        1,
                        matchIndex,
                        matchIndex < prefixStartIndex ? "dict" : "source",
                        mLength
                    );
                    printf(
                        "next_long %zu %d %u %s %d\n",
                        h3,
                        0,
                        matchIndex3,
                        matchIndex3 < prefixStartIndex ? "dict" : "source",
                        0
                    );
                    ZSTD_freeCCtx(cctx);
                    return;
                }
                offset_2 = offset_1;
                offset_1 = offset;
                ip += mLength;
                anchor = ip;
            }
        } else {
            ip += ((ip - anchor) >> kSearchStrength) + 1;
        }

        if (ip <= ilimit) {
            U32 const indexToInsert = curr + 2;
            ((U32*)hashLong)[ZSTD_hashPtr(base + indexToInsert, hBitsL, 8)] = indexToInsert;
            ((U32*)hashLong)[ZSTD_hashPtr(ip - 2, hBitsL, 8)] = (U32)(ip - 2 - base);
            ((U32*)hashSmall)[ZSTD_hashPtr(base + indexToInsert, hBitsS, cParams->minMatch)] = indexToInsert;
            ((U32*)hashSmall)[ZSTD_hashPtr(ip - 1, hBitsS, cParams->minMatch)] = (U32)(ip - 1 - base);

            while (ip <= ilimit) {
                U32 const current2 = (U32)(ip - base);
                U32 const repIndex2 = current2 - offset_2;
                const BYTE* repMatch2 = repIndex2 < prefixStartIndex ? dictBase + repIndex2 : base + repIndex2;
                if (((ZSTD_index_overlap_check(prefixStartIndex, repIndex2))
                    & (offset_2 <= current2 - dictStartIndex))
                  && (MEM_read32(repMatch2) == MEM_read32(ip))) {
                    size_t const repLength2 = ZSTD_count_2segments(
                        ip + 4,
                        repMatch2 + 4,
                        iend,
                        repIndex2 < prefixStartIndex ? dictEnd : iend,
                        prefixStart
                    ) + 4;
                    U32 const tmpOffset = offset_2; offset_2 = offset_1; offset_1 = tmpOffset;
                    ((U32*)hashSmall)[ZSTD_hashPtr(ip, hBitsS, cParams->minMatch)] = current2;
                    ((U32*)hashLong)[ZSTD_hashPtr(ip, hBitsL, 8)] = current2;
                    ip += repLength2;
                    anchor = ip;
                    continue;
                }
                break;
            }
        }
    }

    printf("probe_not_reached 1\n");
    ZSTD_freeCCtx(cctx);
}

static void build_trained_dict(unsigned char** dict_out, size_t* dict_size_out) {
    static const char* ORDER_STATUSES[] = { "open", "closed", "pending" };
    static const char* INVOICE_STATUSES[] = { "draft", "final", "paid" };
    static const char* BUILD_STATES[] = { "running", "passed", "failed" };
    static const char* BRANCHES[] = { "main", "release", "hotfix" };
    static const char* REGIONS[] = { "us-east", "eu-west", "ap-south" };
    enum { TRAIN_SAMPLE_COUNT = 64, SAMPLE_CAPACITY = 320, DICT_CAPACITY = 512 };
    size_t sample_sizes[TRAIN_SAMPLE_COUNT];
    unsigned char* samples;
    unsigned char* dict;
    unsigned char* cursor;
    size_t dict_size;
    size_t i;

    samples = (unsigned char*)malloc(TRAIN_SAMPLE_COUNT * SAMPLE_CAPACITY);
    dict = (unsigned char*)malloc(DICT_CAPACITY);
    if (samples == NULL || dict == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(1);
    }

    cursor = samples;
    for (i = 0; i < TRAIN_SAMPLE_COUNT; ++i) {
        char sample[SAMPLE_CAPACITY];
        unsigned customer_id = 10000u + (unsigned)i * 7u;
        unsigned project_id = 4000u + (unsigned)i * 3u;
        unsigned build_id = 9000u + (unsigned)i * 5u;
        int written;

        if ((i % 3) == 0) {
            written = snprintf(
                sample,
                sizeof(sample),
                "GET /v2/customers/%u/orders?status=%s&limit=50\n"
                "{\"customer_id\":%u,\"status\":\"%s\",\"region\":\"%s\","
                "\"items\":[{\"sku\":\"A-%u\",\"qty\":%u}]}\n",
                customer_id,
                ORDER_STATUSES[i % 3],
                customer_id,
                ORDER_STATUSES[i % 3],
                REGIONS[i % 3],
                100u + (unsigned)(i % 17),
                1u + (unsigned)(i % 4)
            );
        } else if ((i % 3) == 1) {
            written = snprintf(
                sample,
                sizeof(sample),
                "POST /v2/customers/%u/invoices\n"
                "{\"customer_id\":%u,\"currency\":\"USD\",\"total\":%u,"
                "\"status\":\"%s\",\"region\":\"%s\"}\n",
                customer_id,
                customer_id,
                1500u + (unsigned)i * 11u,
                INVOICE_STATUSES[i % 3],
                REGIONS[i % 3]
            );
        } else {
            written = snprintf(
                sample,
                sizeof(sample),
                "PATCH /v2/projects/%u/builds/%u\n"
                "{\"project\":%u,\"build\":%u,\"state\":\"%s\","
                "\"branch\":\"%s\",\"artifact\":\"bundle.tar\"}\n",
                project_id,
                build_id,
                project_id,
                build_id,
                BUILD_STATES[i % 3],
                BRANCHES[i % 3]
            );
        }

        if (written < 0 || written >= SAMPLE_CAPACITY) {
            fprintf(stderr, "failed to generate training sample\n");
            exit(1);
        }

        sample_sizes[i] = (size_t)written;
        memcpy(cursor, sample, sample_sizes[i]);
        cursor += sample_sizes[i];
    }

    dict_size = ZDICT_trainFromBuffer(dict, DICT_CAPACITY, samples, sample_sizes, TRAIN_SAMPLE_COUNT);
    free(samples);
    if (ZDICT_isError(dict_size)) {
        fprintf(stderr, "train-dict failed: %s\n", ZDICT_getErrorName(dict_size));
        exit(1);
    }

    *dict_out = dict;
    *dict_size_out = dict_size;
}

static int parse_int_arg(const char* value, const char* name) {
    char* end = NULL;
    long parsed = strtol(value, &end, 10);
    if (value[0] == '\0' || end == value || *end != '\0') {
        fprintf(stderr, "invalid %s: %s\n", name, value);
        exit(2);
    }
    return (int)parsed;
}

static int parse_bool_arg(const char* value, const char* name) {
    int parsed = parse_int_arg(value, name);
    if (parsed != 0 && parsed != 1) {
        fprintf(stderr, "%s must be 0 or 1\n", name);
        exit(2);
    }
    return parsed;
}

#define UPSTREAM_LAZY_SKIPPING_STEP 8
#define UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS 8

#define UPSTREAM_HC_PROBE_VISIT_LIMIT 8
#define UPSTREAM_HC_PROBE_LINK_LIMIT 4

typedef struct {
    U32 index;
    size_t length;
} upstream_hc_probe_visit_t;

typedef struct {
    U32 index;
    const char* source;
} upstream_hc_probe_link_t;

static void upstream_hc_probe_record_visit(
    upstream_hc_probe_visit_t* visits,
    size_t* count,
    U32 index,
    size_t length
) {
    if (*count >= UPSTREAM_HC_PROBE_VISIT_LIMIT) {
        return;
    }
    visits[*count].index = index;
    visits[*count].length = length;
    *count += 1;
}

static void upstream_hc_probe_write_visits(
    const char* label,
    const upstream_hc_probe_visit_t* visits,
    size_t count
) {
    size_t i;
    printf("%s", label);
    for (i = 0; i < count; ++i) {
        printf(" %u:%zu", visits[i].index, visits[i].length);
    }
    printf("\n");
}

static void upstream_hc_probe_record_link(
    upstream_hc_probe_link_t* links,
    size_t* count,
    U32 index,
    const char* source
) {
    if (*count >= UPSTREAM_HC_PROBE_LINK_LIMIT) {
        return;
    }
    links[*count].index = index;
    links[*count].source = source;
    *count += 1;
}

static void upstream_hc_probe_write_links(
    const char* label,
    const upstream_hc_probe_link_t* links,
    size_t count
) {
    size_t i;
    printf("%s", label);
    for (i = 0; i < count; ++i) {
        printf(" %u:%s", links[i].index, links[i].source);
    }
    printf("\n");
}

static U32 upstream_insert_and_find_first_index(
    ZSTD_MatchState_t* ms,
    const ZSTD_compressionParameters* cParams,
    const BYTE* ip,
    U32 mls,
    U32 lazySkipping
) {
    U32* const hashTable = ms->hashTable;
    U32* const chainTable = ms->chainTable;
    const U32 chainMask = (1U << cParams->chainLog) - 1U;
    const BYTE* const base = ms->window.base;
    const U32 target = (U32)(ip - base);
    U32 idx = ms->nextToUpdate;

    while (idx < target) {
        size_t const h = ZSTD_hashPtr(base + idx, cParams->hashLog, mls);
        chainTable[idx & chainMask] = hashTable[h];
        hashTable[h] = idx;
        idx++;
        if (lazySkipping) {
            break;
        }
    }

    ms->nextToUpdate = target;
    return hashTable[ZSTD_hashPtr(ip, cParams->hashLog, mls)];
}

static void write_trained_dict_hc_probe(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t pos
) {
    unsigned char* dict;
    size_t dict_size;
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_CDict* cdict;
    ZSTD_MatchState_t* ms;
    const ZSTD_MatchState_t* dms;
    const ZSTD_compressionParameters* cParams;
    const BYTE* const ip = src + pos;
    const BYTE* const iLimit = src + src_size;
    const BYTE* base;
    const BYTE* dictBase;
    const BYTE* prefixStart;
    const BYTE* dictEnd;
    const BYTE* dmsBase;
    const BYTE* dmsEnd;
    U32 dictLimit;
    U32 curr;
    U32 lowestValid;
    U32 withinMaxDistance;
    U32 lowLimit;
    U32 minChain;
    U32 matchIndex;
    U32 nbAttempts;
    U32 mls;
    size_t ml = 4 - 1;
    size_t sourceBestLength = 0;
    size_t sourceBestOffset = 0;
    size_t dictBestLength = 0;
    size_t dictBestOffset = 0;
    size_t winnerLength = 0;
    size_t winnerRawOffset = 0;
    size_t winnerOffBase = 0;
    const char* winner = "unknown";
    U32 sourceHead = 0;
    U32 dictHead = 0;
    U32 attemptsBeforeDict = 0;
    upstream_hc_probe_visit_t sourceVisits[UPSTREAM_HC_PROBE_VISIT_LIMIT];
    upstream_hc_probe_visit_t dictVisits[UPSTREAM_HC_PROBE_VISIT_LIMIT];
    upstream_hc_probe_link_t chainLinks[UPSTREAM_HC_PROBE_LINK_LIMIT];
    size_t sourceVisitCount = 0;
    size_t dictVisitCount = 0;
    size_t chainLinkCount = 0;
    size_t maxDistance;
    U32 dmsLowestIndex;
    U32 dmsChainSize;
    U32 dmsChainMask;
    U32 dmsSize;
    U32 dmsIndexDelta;
    U32 dmsMinChain;
    U32 dmsHash;
    ZSTD_frameParameters fParams;
    ZSTD_dictMode_e dictMode;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (pos + MINMATCH > src_size) {
        fprintf(stderr, "probe position out of range\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_useRowMatchFinder, ZSTD_ps_enable))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }

    build_trained_dict(&dict, &dict_size);
    cdict = ZSTD_createCDict_advanced2(
        dict,
        dict_size,
        ZSTD_dlm_byRef,
        ZSTD_dct_auto,
        &cctx->requestedParams,
        ZSTD_defaultCMem
    );
    if (cdict == NULL) {
        fprintf(stderr, "create cdict failed\n");
        exit(2);
    }
    fParams.contentSizeFlag = 1;
    fParams.checksumFlag = (unsigned)checksum;
    fParams.noDictIDFlag = 0;
    {
        size_t const begin_result =
            ZSTD_compressBegin_usingCDict_advanced(cctx, cdict, fParams, src_size);
        if (ZSTD_isError(begin_result)) {
            fprintf(stderr, "compress begin failed: %s\n", ZSTD_getErrorName(begin_result));
            exit(2);
        }
    }

    ms = &cctx->blockState.matchState;
    if (!ZSTD_window_update(&ms->window, src, src_size, ms->forceNonContiguous)) {
        ms->forceNonContiguous = 0;
        ms->nextToUpdate = ms->window.dictLimit;
    }
    maxDistance = 1U << cctx->appliedParams.cParams.windowLog;
    ZSTD_checkDictValidity(&ms->window, src + src_size, (U32)maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
    ZSTD_window_enforceMaxDist(&ms->window, src, (U32)maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
    if (ms->nextToUpdate < ms->window.lowLimit) {
        ms->nextToUpdate = ms->window.lowLimit;
    }

    dictMode = ZSTD_matchState_dictMode(ms);
    printf("probe %zu\n", pos);
    printf(
        "backend %s\n",
        dictMode == ZSTD_noDict ? "nodict" :
        dictMode == ZSTD_extDict ? "extdict" :
        dictMode == ZSTD_dictMatchState ? "dictmatch" : "dds"
    );

    dms = ms->dictMatchState;
    if (dictMode != ZSTD_extDict &&
        (dictMode != ZSTD_dictMatchState || dms == NULL || dms->dedicatedDictSearch)) {
        printf("hash_slot 0\n");
        printf("raw_head 0\n");
        printf("next_to_update %u\n", ms->nextToUpdate);
        printf("low_limit %u\n", ms->window.lowLimit);
        printf("min_chain 0\n");
        printf("chain_links\n");
        printf("source_head 0\n");
        printf("attempts_before_dict 0\n");
        printf("source_best 0 0\n");
        printf("source_visits\n");
        printf("dict_head 0\n");
        printf("dict_best 0 0\n");
        printf("dict_visits\n");
        printf("winner unknown 0 0 0\n");
        ZSTD_freeCDict(cdict);
        ZSTD_freeCCtx(cctx);
        free(dict);
        return;
    }

    cParams = &ms->cParams;
    mls = cParams->minMatch;
    base = ms->window.base;
    dictBase = ms->window.dictBase;
    dictLimit = ms->window.dictLimit;
    prefixStart = base + dictLimit;
    dictEnd = dictBase + dictLimit;
    curr = (U32)(ip - base);
    lowestValid = ms->window.lowLimit;
    withinMaxDistance = (curr - lowestValid > maxDistance)
        ? curr - (U32)maxDistance
        : lowestValid;
    lowLimit = ms->loadedDictEnd != 0 ? lowestValid : withinMaxDistance;
    minChain = curr > (1U << cParams->chainLog) ? curr - (1U << cParams->chainLog) : 0;
    nbAttempts = 1U << cParams->searchLog;
    printf("hash_slot %zu\n", ZSTD_hashPtr(ip, cParams->hashLog, mls));
    printf("next_to_update %u\n", ms->nextToUpdate);
    printf("low_limit %u\n", lowLimit);
    printf("min_chain %u\n", minChain);

    matchIndex = upstream_insert_and_find_first_index(ms, cParams, ip, mls, ms->lazySkipping);
    printf("raw_head %u\n", matchIndex);
    sourceHead = matchIndex;
    {
        const U32 chainMask = (1U << cParams->chainLog) - 1U;
        U32 link = matchIndex;
        while (chainLinkCount < UPSTREAM_HC_PROBE_LINK_LIMIT && link >= lowLimit) {
            upstream_hc_probe_record_link(
                chainLinks,
                &chainLinkCount,
                link,
                link < dictLimit ? "prefix" : "source"
            );
            if (link <= minChain) {
                break;
            }
            link = ms->chainTable[link & chainMask];
        }
    }
    upstream_hc_probe_write_links("chain_links", chainLinks, chainLinkCount);
    for (; (matchIndex >= lowLimit) && (nbAttempts > 0); nbAttempts--) {
        size_t currentMl = 0;
        size_t rawOffset;

        if (matchIndex >= dictLimit) {
            const BYTE* const match = base + matchIndex;
            if (MEM_read32(match + ml - 3) == MEM_read32(ip + ml - 3)) {
                currentMl = ZSTD_count(ip, match, iLimit);
            }
        } else if (matchIndex < dictLimit) {
            const BYTE* const match = dictBase + matchIndex;
            if (match + 4 <= dictEnd && MEM_read32(match) == MEM_read32(ip)) {
                currentMl = ZSTD_count_2segments(ip + 4, match + 4, iLimit, dictEnd, prefixStart) + 4;
            }
        }

        rawOffset = curr - matchIndex;
        if (matchIndex >= dictLimit) {
            upstream_hc_probe_record_visit(sourceVisits, &sourceVisitCount, matchIndex, currentMl);
            if (currentMl > sourceBestLength) {
                sourceBestLength = currentMl;
                sourceBestOffset = rawOffset;
            }
        } else {
            if (attemptsBeforeDict == 0) {
                attemptsBeforeDict = nbAttempts;
                dictHead = matchIndex;
            }
            upstream_hc_probe_record_visit(dictVisits, &dictVisitCount, matchIndex, currentMl);
            if (currentMl > dictBestLength) {
                dictBestLength = currentMl;
                dictBestOffset = rawOffset;
            }
        }
        if (currentMl > ml) {
            winner = matchIndex < dictLimit ? "dict" : "source";
            winnerLength = currentMl;
            winnerRawOffset = rawOffset;
            winnerOffBase = winnerRawOffset + 3;
            ml = currentMl;
            if (ip + currentMl == iLimit) {
                break;
            }
        }
        if (matchIndex <= minChain) {
            break;
        }
        matchIndex = ms->chainTable[matchIndex & ((1U << cParams->chainLog) - 1U)];
    }

    if (dictMode == ZSTD_extDict) {
        printf("source_head %u\n", sourceHead);
        printf("attempts_before_dict %u\n", attemptsBeforeDict);
        printf("source_best %zu %zu\n", sourceBestLength, sourceBestOffset);
        upstream_hc_probe_write_visits("source_visits", sourceVisits, sourceVisitCount);
        printf("dict_head %u\n", dictHead);
        printf("dict_best %zu %zu\n", dictBestLength, dictBestOffset);
        upstream_hc_probe_write_visits("dict_visits", dictVisits, dictVisitCount);
        printf("winner %s %zu %zu %zu\n", winner, winnerLength, winnerRawOffset, winnerOffBase);

        ZSTD_freeCDict(cdict);
        ZSTD_freeCCtx(cctx);
        free(dict);
        return;
    }

    attemptsBeforeDict = nbAttempts;
    dmsBase = dms->window.base;
    dmsEnd = dms->window.nextSrc;
    dmsLowestIndex = dms->window.dictLimit;
    dmsChainSize = 1U << dms->cParams.chainLog;
    dmsChainMask = dmsChainSize - 1U;
    dmsSize = (U32)(dmsEnd - dmsBase);
    dmsIndexDelta = dictLimit - dmsSize;
    dmsMinChain = dmsSize > dmsChainSize ? dmsSize - dmsChainSize : 0;
    dmsHash = ZSTD_hashPtr(ip, dms->cParams.hashLog, mls);
    matchIndex = dms->hashTable[dmsHash];
    dictHead = matchIndex;

    for (; (matchIndex >= dmsLowestIndex) && (nbAttempts > 0); nbAttempts--) {
        size_t currentMl = 0;
        const BYTE* const match = dmsBase + matchIndex;
        size_t rawOffset;

        if (match + 4 <= dmsEnd && MEM_read32(match) == MEM_read32(ip)) {
            currentMl = ZSTD_count_2segments(ip + 4, match + 4, iLimit, dmsEnd, prefixStart) + 4;
        }

        upstream_hc_probe_record_visit(dictVisits, &dictVisitCount, matchIndex, currentMl);
        if (currentMl > ml) {
            rawOffset = curr - (matchIndex + dmsIndexDelta);
            dictBestLength = currentMl;
            dictBestOffset = rawOffset;
            winner = "dict";
            winnerLength = currentMl;
            winnerRawOffset = rawOffset;
            winnerOffBase = rawOffset + 3;
            ml = currentMl;
            if (ip + currentMl == iLimit) {
                break;
            }
        }

        if (matchIndex <= dmsMinChain) {
            break;
        }
        matchIndex = dms->chainTable[matchIndex & dmsChainMask];
    }

    printf("source_head %u\n", sourceHead);
    printf("attempts_before_dict %u\n", attemptsBeforeDict);
    printf("source_best %zu %zu\n", sourceBestLength, sourceBestOffset);
    upstream_hc_probe_write_visits("source_visits", sourceVisits, sourceVisitCount);
    printf("dict_head %u\n", dictHead);
    printf("dict_best %zu %zu\n", dictBestLength, dictBestOffset);
    upstream_hc_probe_write_visits("dict_visits", dictVisits, dictVisitCount);
    printf("winner %s %zu %zu %zu\n", winner, winnerLength, winnerRawOffset, winnerOffBase);

    ZSTD_freeCDict(cdict);
    ZSTD_freeCCtx(cctx);
    free(dict);
}

static const char* upstream_probe_source_from_offbase(
    size_t offBase,
    const BYTE* start,
    const BYTE* prefixStart
) {
    if (!OFFBASE_IS_OFFSET(offBase)) {
        return "rep";
    }
    return OFFBASE_TO_OFFSET(offBase) > (size_t)(start - prefixStart) ? "prefix" : "source";
}

typedef struct {
    U32 nextToUpdateBeforeSearch;
    U32 hash;
    U32 relRow;
    U32 tag;
    U32 lowLimit;
    U32 attemptBudget;
    U32 headIndex;
    U32 insertIndex;
    U32 groupWidth;
    size_t matchCount;
    U32 matchPositions[4];
    U32 matchIndices[4];
    size_t visitCount;
    U32 visitPositions[4];
    U32 visitIndices[4];
    size_t visitLengths[4];
    unsigned visitGatePasses[4];
    size_t visitWinnerLengths[4];
    size_t visitWinnerOffBases[4];
} UpstreamNoDictRowSearchTrace;

static void upstream_row_probe_record_match(
    UpstreamNoDictRowSearchTrace* trace,
    U32 matchPos,
    U32 matchIndex
) {
    if (trace->matchCount < 4) {
        trace->matchPositions[trace->matchCount] = matchPos;
        trace->matchIndices[trace->matchCount] = matchIndex;
    }
    trace->matchCount += 1;
}

static void upstream_row_probe_record_visit(
    UpstreamNoDictRowSearchTrace* trace,
    U32 matchPos,
    U32 matchIndex,
    size_t matchLength,
    unsigned gatePassed,
    size_t winnerLength,
    size_t winnerOffBase
) {
    if (trace->visitCount < 4) {
        trace->visitPositions[trace->visitCount] = matchPos;
        trace->visitIndices[trace->visitCount] = matchIndex;
        trace->visitLengths[trace->visitCount] = matchLength;
        trace->visitGatePasses[trace->visitCount] = gatePassed;
        trace->visitWinnerLengths[trace->visitCount] = winnerLength;
        trace->visitWinnerOffBases[trace->visitCount] = winnerOffBase;
    }
    trace->visitCount += 1;
}

static void upstream_row_probe_write_matches(
    const char* label,
    const U32* matchPositions,
    const U32* matchIndices,
    size_t count
) {
    size_t i;
    printf("%s", label);
    for (i = 0; i < count && i < 4; ++i) {
        printf(" %u:%u", matchPositions[i], matchIndices[i]);
    }
    printf("\n");
}

static void upstream_row_probe_write_visits(
    const char* label,
    const U32* visitPositions,
    const U32* visitIndices,
    const size_t* visitLengths,
    size_t count
) {
    size_t i;
    printf("%s", label);
    for (i = 0; i < count && i < 4; ++i) {
        printf(" %u:%u:%zu", visitPositions[i], visitIndices[i], visitLengths[i]);
    }
    printf("\n");
}

static void upstream_row_probe_write_visit_states(
    const char* label,
    const unsigned* visitGatePasses,
    const size_t* visitWinnerLengths,
    const size_t* visitWinnerOffBases,
    size_t count
) {
    size_t i;
    printf("%s", label);
    for (i = 0; i < count && i < 4; ++i) {
        printf(" %u:%zu:%zu", visitGatePasses[i], visitWinnerLengths[i], visitWinnerOffBases[i]);
    }
    printf("\n");
}

static size_t upstream_no_dict_row_search_probe(
    ZSTD_MatchState_t* ms,
    const BYTE* const ip,
    const BYTE* const iLimit,
    const U32 mls,
    const U32 rowLog,
    size_t* offsetPtr,
    UpstreamNoDictRowSearchTrace* trace
) {
    U32* const hashTable = ms->hashTable;
    BYTE* const tagTable = ms->tagTable;
    U32* const hashCache = ms->hashCache;
    const U32 hashLog = ms->rowHashLog;
    const ZSTD_compressionParameters* const cParams = &ms->cParams;
    const BYTE* const base = ms->window.base;
    const U32 curr = (U32)(ip - base);
    const U32 maxDistance = 1U << cParams->windowLog;
    const U32 lowestValid = ms->window.lowLimit;
    const U32 lowLimit =
        (curr - lowestValid > maxDistance) ? curr - maxDistance : lowestValid;
    const U32 rowEntries = (1U << rowLog);
    const U32 rowMask = rowEntries - 1;
    const U32 cappedSearchLog = MIN(cParams->searchLog, rowLog);
    const U32 groupWidth = ZSTD_row_matchMaskGroupWidth(rowEntries);
    const U64 hashSalt = ms->hashSalt;
    U32 nbAttempts = 1U << cappedSearchLog;
    size_t ml = 4 - 1;
    U32 hash;

    *trace = (UpstreamNoDictRowSearchTrace){0};
    trace->nextToUpdateBeforeSearch = ms->nextToUpdate;
    trace->lowLimit = lowLimit;
    trace->attemptBudget = nbAttempts;
    trace->groupWidth = groupWidth;

    if (!ms->lazySkipping) {
        ZSTD_row_update_internal(ms, ip, mls, rowLog, rowMask, 1 /* useCache */);
        hash = ZSTD_row_nextCachedHash(hashCache, hashTable, tagTable, base, curr, hashLog, rowLog, mls, hashSalt);
    } else {
        hash = (U32)ZSTD_hashPtrSalted(ip, hashLog + ZSTD_ROW_HASH_TAG_BITS, mls, hashSalt);
        ms->nextToUpdate = curr;
    }
    ms->hashSaltEntropy += hash;

    {
        U32 const relRow = (hash >> ZSTD_ROW_HASH_TAG_BITS) << rowLog;
        U32 const tag = hash & ZSTD_ROW_HASH_TAG_MASK;
        U32* const row = hashTable + relRow;
        BYTE* tagRow = (BYTE*)(tagTable + relRow);
        U32 const headGrouped = (*tagRow & rowMask) * groupWidth;
        U32 matchBuffer[ZSTD_ROW_HASH_MAX_ENTRIES];
        U32 matchPositions[ZSTD_ROW_HASH_MAX_ENTRIES];
        size_t numMatches = 0;
        size_t currMatch = 0;
        ZSTD_VecMask matches = ZSTD_row_getMatchMask(tagRow, (BYTE)tag, headGrouped, rowEntries);

        trace->hash = hash;
        trace->relRow = relRow;
        trace->tag = tag;
        trace->headIndex = (*tagRow & rowMask);
        trace->insertIndex = (((*tagRow - 1) & rowMask) + ((((U32)(*tagRow - 1) & rowMask) == 0) ? rowMask : 0));

        for (; (matches > 0) && (nbAttempts > 0); matches &= (matches - 1)) {
            U32 const matchPos = ((headGrouped + ZSTD_VecMask_next(matches)) / groupWidth) & rowMask;
            U32 const matchIndex = row[matchPos];
            if (matchPos == 0) continue;
            if (matchIndex < lowLimit) break;
            upstream_row_probe_record_match(trace, matchPos, matchIndex);
            matchBuffer[numMatches] = matchIndex;
            matchPositions[numMatches] = matchPos;
            ++numMatches;
            --nbAttempts;
        }

        {
            U32 const pos = ZSTD_row_nextIndex(tagRow, rowMask);
            tagRow[pos] = (BYTE)tag;
            row[pos] = ms->nextToUpdate++;
        }

        for (; currMatch < numMatches; ++currMatch) {
            U32 const matchIndex = matchBuffer[currMatch];
            size_t currentMl = 0;
            unsigned gatePassed = 0;
            const BYTE* const match = base + matchIndex;
            if (MEM_read32(match + ml - 3) == MEM_read32(ip + ml - 3)) {
                gatePassed = 1;
                currentMl = ZSTD_count(ip, match, iLimit);
            }
            if (currentMl > ml) {
                ml = currentMl;
                *offsetPtr = OFFSET_TO_OFFBASE(curr - matchIndex);
                if (ip + currentMl == iLimit) {
                    upstream_row_probe_record_visit(
                        trace,
                        matchPositions[currMatch],
                        matchIndex,
                        currentMl,
                        gatePassed,
                        ml,
                        ml >= 4 ? *offsetPtr : 0
                    );
                    break;
                }
            }
            upstream_row_probe_record_visit(
                trace,
                matchPositions[currMatch],
                matchIndex,
                currentMl,
                gatePassed,
                ml,
                ml >= 4 ? *offsetPtr : 0
            );
        }
    }

    return ml;
}

static void write_no_dict_row_lazy_probe(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t target_pos
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_CCtx* configured = ZSTD_createCCtx();
    ZSTD_MatchState_t* ms;
    const ZSTD_compressionParameters* cParams;
    ZSTD_parameters params;
    ZSTD_ParamSwitch_e useRowMatchFinder = ZSTD_ps_auto;
    ZSTD_Sequence* sequences = NULL;
    size_t sequence_capacity = ZSTD_sequenceBound(src_size);
    size_t sequence_count;
    const BYTE* const istart = (const BYTE*)src;
    const BYTE* ip = istart;
    const BYTE* anchor = istart;
    const BYTE* const iend = istart + src_size;
    const BYTE* const iLimit = iend;
    const BYTE* ilimit;
    const BYTE* base;
    const BYTE* prefixLowest;
    U32 mls;
    U32 rowLog;
    U32 depth = 0;
    U32 offset_1 = repStartValue[0];
    U32 offset_2 = repStartValue[1];
    size_t begin_result;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (configured == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    sequences = (ZSTD_Sequence*)malloc(sequence_capacity * sizeof(ZSTD_Sequence));
    if (sequences == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (src_size < 8 + ZSTD_ROW_HASH_CACHE_SIZE + 1) {
        fprintf(stderr, "input too short for no-dict row probe\n");
        exit(2);
    }
    if (target_pos + MINMATCH > src_size) {
        fprintf(stderr, "probe position out of range\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(configured, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(configured, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(configured, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    sequence_count = ZSTD_generateSequences(configured, sequences, sequence_capacity, src, src_size);
    if (ZSTD_isError(sequence_count)) {
        fprintf(stderr, "generate-sequences failed: %s\n", ZSTD_getErrorName(sequence_count));
        exit(2);
    }
    params.cParams = configured->appliedParams.cParams;
    params.fParams.contentSizeFlag = 1;
    params.fParams.checksumFlag = (unsigned)checksum;
    params.fParams.noDictIDFlag = 0;
    useRowMatchFinder = configured->appliedParams.useRowMatchFinder;
    ZSTD_freeCCtx(configured);
    configured = NULL;
    free(sequences);
    sequences = NULL;

    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_useRowMatchFinder, useRowMatchFinder))) {
        fprintf(stderr, "row match finder setup failed\n");
        exit(2);
    }
    begin_result = ZSTD_compressBegin_advanced(cctx, NULL, 0, params, src_size);
    if (ZSTD_isError(begin_result)) {
        fprintf(stderr, "compress begin failed: %s\n", ZSTD_getErrorName(begin_result));
        exit(2);
    }

    ms = &cctx->blockState.matchState;
    if (!ZSTD_window_update(&ms->window, src, src_size, ms->forceNonContiguous)) {
        ms->forceNonContiguous = 0;
        ms->nextToUpdate = ms->window.dictLimit;
    }
    {
        size_t const maxDistance = 1U << cctx->appliedParams.cParams.windowLog;
        ZSTD_window_enforceMaxDist(
            &ms->window,
            src,
            (U32)maxDistance,
            &ms->loadedDictEnd,
            &ms->dictMatchState
        );
        if (ms->nextToUpdate < ms->window.lowLimit) {
            ms->nextToUpdate = ms->window.lowLimit;
        }
    }

    if (ZSTD_matchState_dictMode(ms) != ZSTD_noDict ||
        cctx->appliedParams.useRowMatchFinder != ZSTD_ps_enable) {
        fprintf(stderr, "probe requires no-dict row backend\n");
        exit(2);
    }

    cParams = &ms->cParams;
    switch (cParams->strategy) {
        case ZSTD_greedy: depth = 0; break;
        case ZSTD_lazy: depth = 1; break;
        case ZSTD_lazy2:
        case ZSTD_btlazy2: depth = 2; break;
        default:
            fprintf(stderr, "probe requires greedy/lazy/lazy2 strategy\n");
            exit(2);
    }

    base = ms->window.base;
    prefixLowest = base + ms->window.dictLimit;
    mls = BOUNDED(4, cParams->minMatch, 6);
    rowLog = BOUNDED(4, cParams->searchLog, 6);
    ilimit = iend - 8 - ZSTD_ROW_HASH_CACHE_SIZE;

    printf("probe %zu\n", target_pos);
    printf("backend nodict\n");
    printf("depth %u\n", depth);
    printf(
        "applied %u %u %u %u %u %u %u %u %llu\n",
        cParams->windowLog,
        cParams->hashLog,
        cParams->searchLog,
        cParams->minMatch,
        cParams->strategy,
        ms->rowHashLog,
        rowLog,
        ms->window.dictLimit,
        (unsigned long long)ms->hashSalt
    );

    ip += (ip == prefixLowest);
    {
        U32 const curr = (U32)(ip - base);
        U32 const windowLow = ZSTD_getLowestPrefixIndex(ms, curr, cParams->windowLog);
        U32 const maxRep = curr - windowLow;
        if (offset_2 > maxRep) offset_2 = 0;
        if (offset_1 > maxRep) offset_1 = 0;
    }

    ms->lazySkipping = 0;
    ZSTD_row_fillHashCache(ms, base, rowLog, mls, ms->nextToUpdate, ilimit);

    while (ip < ilimit) {
        size_t matchLength = 0;
        size_t offBase = REPCODE1_TO_OFFBASE;
        const BYTE* start = ip + 1;
        size_t baselineRepLength = 0;
        UpstreamNoDictRowSearchTrace baselineRegularTrace = {0};
        size_t baselineRegularLength = 0;
        size_t baselineRegularOffBase = 0;
        size_t depth1RepLength = 0;
        size_t depth1RegularLength = 0;
        size_t depth1RegularOffBase = 0;
        size_t depth2RepLength = 0;
        size_t depth2RegularLength = 0;
        size_t depth2RegularOffBase = 0;
        size_t firstImmediateRep2Length = 0;

        if ((offset_1 > 0) & (MEM_read32(ip + 1 - offset_1) == MEM_read32(ip + 1))) {
            baselineRepLength = ZSTD_count(ip + 5, ip + 5 - offset_1, iend) + 4;
            matchLength = baselineRepLength;
        }

        {
            size_t ofbCandidate = 0;
            size_t const ml2 = upstream_no_dict_row_search_probe(
                ms,
                ip,
                iend,
                mls,
                rowLog,
                &ofbCandidate,
                &baselineRegularTrace
            );
            baselineRegularLength = ml2;
            baselineRegularOffBase = ml2 >= 4 ? ofbCandidate : 0;
            if (!((depth == 0) && (baselineRepLength >= 4)) && (ml2 > matchLength)) {
                matchLength = ml2;
                start = ip;
                offBase = ofbCandidate;
            }
        }

        if ((size_t)(ip - istart) == target_pos) {
            const BYTE* probePos = ip;
            size_t probeMatchLength = matchLength;
            size_t probeOffBase = offBase;
            const BYTE* probeStart = start;
            size_t continueStepCount = 0;
            size_t continuePositions[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            size_t continueRepLengths[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            unsigned continueRepImproved[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            size_t continueRegularLengths[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            size_t continueRegularOffBases[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            unsigned continueRegularImproved[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            const char* continueCurrentKinds[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {"none"};
            size_t continueCurrentStarts[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            size_t continueCurrentLengths[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            size_t continueCurrentOffBases[UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS] = {0};
            const char* stopReason = "none";

            if (probeMatchLength < 4) {
                stopReason = "no_baseline";
            } else if (depth == 0) {
                stopReason = "depth0";
            } else if (depth >= 1) {
                while (probePos < ilimit) {
                    const BYTE* depth1Ip = probePos + 1;
                    size_t repLength = 0;
                    unsigned repImproved = 0;
                    size_t regularLength = 0;
                    size_t regularOffBase = 0;
                    unsigned regularImproved = 0;

                    if (depth1Ip > ilimit) {
                        stopReason = "limit";
                        break;
                    }
                    if (probeOffBase && ((offset_1 > 0) & (MEM_read32(depth1Ip) == MEM_read32(depth1Ip - offset_1)))) {
                        repLength = ZSTD_count(depth1Ip + 4, depth1Ip + 4 - offset_1, iend) + 4;
                        if (probePos == ip) {
                            depth1RepLength = repLength;
                        }
                        if ((repLength >= 4) &&
                            ((int)(repLength * 3) >
                             (int)(probeMatchLength * 3 - ZSTD_highbit32((U32)probeOffBase) + 1))) {
                            probeMatchLength = repLength;
                            probeOffBase = REPCODE1_TO_OFFBASE;
                            probeStart = depth1Ip;
                            repImproved = 1;
                        }
                    }

                    {
                        size_t ofbCandidate = 0;
                        size_t const ml2 =
                            ZSTD_searchMax(ms, depth1Ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
                        regularLength = ml2;
                        regularOffBase = ml2 >= 4 ? ofbCandidate : 0;
                        if (probePos == ip) {
                            depth1RegularLength = ml2;
                            depth1RegularOffBase = ml2 >= 4 ? ofbCandidate : 0;
                        }
                        if ((ml2 >= 4) &&
                            ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                             (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase) + 4))) {
                            probeMatchLength = ml2;
                            probeOffBase = ofbCandidate;
                            probeStart = depth1Ip;
                            regularImproved = 1;
                        }
                    }
                    if (continueStepCount < UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS) {
                        size_t const stepIndex = continueStepCount++;
                        continuePositions[stepIndex] = (size_t)(depth1Ip - istart);
                        continueRepLengths[stepIndex] = repLength;
                        continueRepImproved[stepIndex] = repImproved;
                        continueRegularLengths[stepIndex] = regularLength;
                        continueRegularOffBases[stepIndex] = regularOffBase;
                        continueRegularImproved[stepIndex] = regularImproved;
                        continueCurrentKinds[stepIndex] =
                            OFFBASE_IS_OFFSET(probeOffBase) ? "regular" : "rep";
                        continueCurrentStarts[stepIndex] = (size_t)(probeStart - istart);
                        continueCurrentLengths[stepIndex] = probeMatchLength;
                        continueCurrentOffBases[stepIndex] = probeOffBase;
                    }
                    if (regularImproved) {
                        probePos = depth1Ip;
                        continue;
                    }

                    if (depth == 2) {
                        const BYTE* depth2Ip = depth1Ip + 1;
                        repLength = 0;
                        repImproved = 0;
                        regularLength = 0;
                        regularOffBase = 0;
                        regularImproved = 0;

                        if (depth2Ip > ilimit) {
                            stopReason = "limit";
                            break;
                        }
                        if (probeOffBase && ((offset_1 > 0) & (MEM_read32(depth2Ip) == MEM_read32(depth2Ip - offset_1)))) {
                            repLength = ZSTD_count(depth2Ip + 4, depth2Ip + 4 - offset_1, iend) + 4;
                            if (probePos == ip) {
                                depth2RepLength = repLength;
                            }
                            if ((repLength >= 4) &&
                                ((int)(repLength * 4) >
                                 (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase) + 1))) {
                                probeMatchLength = repLength;
                                probeOffBase = REPCODE1_TO_OFFBASE;
                                probeStart = depth2Ip;
                                repImproved = 1;
                            }
                        }

                        {
                            size_t ofbCandidate = 0;
                            size_t const ml2 =
                                ZSTD_searchMax(ms, depth2Ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
                            regularLength = ml2;
                            regularOffBase = ml2 >= 4 ? ofbCandidate : 0;
                            if (probePos == ip) {
                                depth2RegularLength = ml2;
                                depth2RegularOffBase = ml2 >= 4 ? ofbCandidate : 0;
                            }
                            if ((ml2 >= 4) &&
                                ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                                 (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase) + 7))) {
                                probeMatchLength = ml2;
                                probeOffBase = ofbCandidate;
                                probeStart = depth2Ip;
                                regularImproved = 1;
                            }
                        }
                        if (continueStepCount < UPSTREAM_ROW_LAZY_TRACE_MAX_STEPS) {
                            size_t const stepIndex = continueStepCount++;
                            continuePositions[stepIndex] = (size_t)(depth2Ip - istart);
                            continueRepLengths[stepIndex] = repLength;
                            continueRepImproved[stepIndex] = repImproved;
                            continueRegularLengths[stepIndex] = regularLength;
                            continueRegularOffBases[stepIndex] = regularOffBase;
                            continueRegularImproved[stepIndex] = regularImproved;
                            continueCurrentKinds[stepIndex] =
                                OFFBASE_IS_OFFSET(probeOffBase) ? "regular" : "rep";
                            continueCurrentStarts[stepIndex] = (size_t)(probeStart - istart);
                            continueCurrentLengths[stepIndex] = probeMatchLength;
                            continueCurrentOffBases[stepIndex] = probeOffBase;
                        }
                        if (regularImproved) {
                            probePos = depth2Ip;
                            continue;
                        }
                    }
                    stopReason = "no_regular_improve";
                    break;
                }
                if (strcmp(stopReason, "none") == 0) {
                    stopReason = "limit";
                }
            }

            if (OFFBASE_IS_OFFSET(probeOffBase)) {
                while (((probeStart > anchor) & (probeStart - OFFBASE_TO_OFFSET(probeOffBase) > prefixLowest)) &&
                       (probeStart[-1] == (probeStart - OFFBASE_TO_OFFSET(probeOffBase))[-1])) {
                    probeStart--;
                    probeMatchLength++;
                }
            }

            {
                U32 nextOffset1 = offset_1;
                U32 nextOffset2 = offset_2;
                const BYTE* nextIp;

                if (OFFBASE_IS_OFFSET(probeOffBase)) {
                    nextOffset2 = nextOffset1;
                    nextOffset1 = (U32)OFFBASE_TO_OFFSET(probeOffBase);
                }
                nextIp = probeStart + probeMatchLength;
                (void)nextOffset1;
                if (nextIp <= ilimit) {
                    if (((nextOffset2 > 0) & (MEM_read32(nextIp) == MEM_read32(nextIp - nextOffset2)))) {
                        firstImmediateRep2Length =
                            ZSTD_count(nextIp + 4, nextIp + 4 - nextOffset2, iend) + 4;
                    }
                }
            }

            printf("visited 1\n");
            printf("anchor %zu\n", (size_t)(anchor - istart));
            printf("offsets %u %u\n", offset_1, offset_2);
            printf("baseline_rep %zu\n", baselineRepLength);
            printf(
                "baseline_regular_state %u %u %u %u %u %u %u %u %u\n",
                baselineRegularTrace.nextToUpdateBeforeSearch,
                baselineRegularTrace.hash,
                baselineRegularTrace.relRow,
                baselineRegularTrace.tag,
                baselineRegularTrace.lowLimit,
                baselineRegularTrace.attemptBudget,
                baselineRegularTrace.headIndex,
                baselineRegularTrace.insertIndex,
                baselineRegularTrace.groupWidth
            );
            upstream_row_probe_write_matches(
                "baseline_regular_matches",
                baselineRegularTrace.matchPositions,
                baselineRegularTrace.matchIndices,
                baselineRegularTrace.matchCount
            );
            upstream_row_probe_write_visits(
                "baseline_regular_visits",
                baselineRegularTrace.visitPositions,
                baselineRegularTrace.visitIndices,
                baselineRegularTrace.visitLengths,
                baselineRegularTrace.visitCount
            );
            printf("baseline_regular %zu %zu\n", baselineRegularLength, baselineRegularOffBase);
            printf("depth1_rep %zu\n", depth1RepLength);
            printf("depth1_regular %zu %zu\n", depth1RegularLength, depth1RegularOffBase);
            printf("depth2_rep %zu\n", depth2RepLength);
            printf("depth2_regular %zu %zu\n", depth2RegularLength, depth2RegularOffBase);
            {
                size_t stepIndex;
                for (stepIndex = 0; stepIndex < continueStepCount; stepIndex++) {
                    printf(
                        "continue_step %zu %zu %zu %u %zu %zu %u %s %zu %zu %zu\n",
                        stepIndex,
                        continuePositions[stepIndex],
                        continueRepLengths[stepIndex],
                        continueRepImproved[stepIndex],
                        continueRegularLengths[stepIndex],
                        continueRegularOffBases[stepIndex],
                        continueRegularImproved[stepIndex],
                        continueCurrentKinds[stepIndex],
                        continueCurrentStarts[stepIndex],
                        continueCurrentLengths[stepIndex],
                        continueCurrentOffBases[stepIndex]
                    );
                }
            }
            printf("stop_reason %s\n", stopReason);
            if (probeMatchLength >= 4) {
                printf(
                    "chosen %s %zu %zu %zu\n",
                    OFFBASE_IS_OFFSET(probeOffBase) ? "regular" : "rep",
                    (size_t)(probeStart - istart),
                    probeMatchLength,
                    probeOffBase
                );
            } else {
                printf("chosen none 0 0 0\n");
            }
            printf("literal_length %zu\n", (size_t)(probeStart - anchor));
            printf("immediate_rep2 %zu\n", firstImmediateRep2Length);

            ZSTD_freeCCtx(cctx);
            return;
        }

        if (matchLength < 4) {
            size_t const step = ((size_t)(ip - anchor) >> kSearchStrength) + 1;
            ip += step;
            ms->lazySkipping = step > UPSTREAM_LAZY_SKIPPING_STEP;
            continue;
        }

        if (depth >= 1) {
            while (ip < ilimit) {
                ip++;
                if (offBase && ((offset_1 > 0) & (MEM_read32(ip) == MEM_read32(ip - offset_1)))) {
                    size_t const mlRep = ZSTD_count(ip + 4, ip + 4 - offset_1, iend) + 4;
                    int const gain2 = (int)(mlRep * 3);
                    int const gain1 = (int)(matchLength * 3 - ZSTD_highbit32((U32)offBase) + 1);
                    if ((mlRep >= 4) && (gain2 > gain1)) {
                        matchLength = mlRep;
                        offBase = REPCODE1_TO_OFFBASE;
                        start = ip;
                    }
                }

                {
                    size_t ofbCandidate = 0;
                    size_t const ml2 =
                        ZSTD_searchMax(ms, ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
                    int const gain2 = (int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate));
                    int const gain1 = (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 4);
                    if ((ml2 >= 4) && (gain2 > gain1)) {
                        matchLength = ml2;
                        offBase = ofbCandidate;
                        start = ip;
                        continue;
                    }
                }

                if ((depth == 2) && (ip < ilimit)) {
                    ip++;
                    if (offBase && ((offset_1 > 0) & (MEM_read32(ip) == MEM_read32(ip - offset_1)))) {
                        size_t const mlRep = ZSTD_count(ip + 4, ip + 4 - offset_1, iend) + 4;
                        int const gain2 = (int)(mlRep * 4);
                        int const gain1 = (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 1);
                        if ((mlRep >= 4) && (gain2 > gain1)) {
                            matchLength = mlRep;
                            offBase = REPCODE1_TO_OFFBASE;
                            start = ip;
                        }
                    }

                    {
                        size_t ofbCandidate = 0;
                        size_t const ml2 =
                            ZSTD_searchMax(ms, ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
                        int const gain2 = (int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate));
                        int const gain1 = (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 7);
                        if ((ml2 >= 4) && (gain2 > gain1)) {
                            matchLength = ml2;
                            offBase = ofbCandidate;
                            start = ip;
                            continue;
                        }
                    }
                }
                break;
            }
        }

        if (OFFBASE_IS_OFFSET(offBase)) {
            while (((start > anchor) & (start - OFFBASE_TO_OFFSET(offBase) > prefixLowest)) &&
                   (start[-1] == (start - OFFBASE_TO_OFFSET(offBase))[-1])) {
                start--;
                matchLength++;
            }
            offset_2 = offset_1;
            offset_1 = (U32)OFFBASE_TO_OFFSET(offBase);
        }

        anchor = ip = start + matchLength;
        if (ms->lazySkipping) {
            ZSTD_row_fillHashCache(ms, base, rowLog, mls, ms->nextToUpdate, ilimit);
            ms->lazySkipping = 0;
        }

        while (ip <= ilimit) {
            if (((offset_2 > 0) & (MEM_read32(ip) == MEM_read32(ip - offset_2)))) {
                size_t const matchLength2 = ZSTD_count(ip + 4, ip + 4 - offset_2, iend) + 4;
                size_t const offBase2 = offset_2;
                offset_2 = offset_1;
                offset_1 = (U32)offBase2;
                ip += matchLength2;
                anchor = ip;
                continue;
            }
            break;
        }
    }

    printf("visited 0\n");
    printf("anchor 0\n");
    printf("offsets 0 0\n");
    printf("baseline_rep 0\n");
    printf("applied 0 0 0 0 0 0 0 0 0\n");
    printf("baseline_regular_state 0 0 0 0 0 0 0 0 0\n");
    printf("baseline_regular_matches\n");
    printf("baseline_regular_visits\n");
    printf("baseline_regular 0 0\n");
    printf("depth1_rep 0\n");
    printf("depth1_regular 0 0\n");
    printf("depth2_rep 0\n");
    printf("depth2_regular 0 0\n");
    printf("stop_reason none\n");
    printf("chosen none 0 0 0\n");
    printf("literal_length 0\n");
    printf("immediate_rep2 0\n");

    ZSTD_freeCCtx(cctx);
}

static void write_no_dict_row_search_probe(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t state_pos,
    size_t probe_pos
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_CCtx* configured = ZSTD_createCCtx();
    ZSTD_MatchState_t* ms;
    const ZSTD_compressionParameters* cParams;
    ZSTD_parameters params;
    ZSTD_ParamSwitch_e useRowMatchFinder = ZSTD_ps_auto;
    ZSTD_Sequence* sequences = NULL;
    size_t sequence_capacity = ZSTD_sequenceBound(src_size);
    size_t sequence_count;
    const BYTE* const istart = (const BYTE*)src;
    const BYTE* ip = istart;
    const BYTE* anchor = istart;
    const BYTE* const iend = istart + src_size;
    const BYTE* ilimit;
    const BYTE* base;
    const BYTE* prefixLowest;
    U32 mls;
    U32 rowLog;
    U32 depth = 0;
    U32 offset_1 = repStartValue[0];
    U32 offset_2 = repStartValue[1];
    size_t begin_result;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (configured == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    sequences = (ZSTD_Sequence*)malloc(sequence_capacity * sizeof(ZSTD_Sequence));
    if (sequences == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (src_size < 8 + ZSTD_ROW_HASH_CACHE_SIZE + 1) {
        fprintf(stderr, "input too short for no-dict row search probe\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(configured, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(configured, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(configured, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    sequence_count = ZSTD_generateSequences(configured, sequences, sequence_capacity, src, src_size);
    if (ZSTD_isError(sequence_count)) {
        fprintf(stderr, "generate-sequences failed: %s\n", ZSTD_getErrorName(sequence_count));
        exit(2);
    }
    params.cParams = configured->appliedParams.cParams;
    params.fParams.contentSizeFlag = 1;
    params.fParams.checksumFlag = (unsigned)checksum;
    params.fParams.noDictIDFlag = 0;
    useRowMatchFinder = configured->appliedParams.useRowMatchFinder;
    ZSTD_freeCCtx(configured);
    configured = NULL;
    free(sequences);
    sequences = NULL;

    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_useRowMatchFinder, useRowMatchFinder))) {
        fprintf(stderr, "row match finder setup failed\n");
        exit(2);
    }
    begin_result = ZSTD_compressBegin_advanced(cctx, NULL, 0, params, src_size);
    if (ZSTD_isError(begin_result)) {
        fprintf(stderr, "compress begin failed: %s\n", ZSTD_getErrorName(begin_result));
        exit(2);
    }

    ms = &cctx->blockState.matchState;
    if (!ZSTD_window_update(&ms->window, src, src_size, ms->forceNonContiguous)) {
        ms->forceNonContiguous = 0;
        ms->nextToUpdate = ms->window.dictLimit;
    }
    {
        size_t const maxDistance = 1U << cctx->appliedParams.cParams.windowLog;
        ZSTD_window_enforceMaxDist(
            &ms->window,
            src,
            (U32)maxDistance,
            &ms->loadedDictEnd,
            &ms->dictMatchState
        );
        if (ms->nextToUpdate < ms->window.lowLimit) {
            ms->nextToUpdate = ms->window.lowLimit;
        }
    }

    if (ZSTD_matchState_dictMode(ms) != ZSTD_noDict ||
        cctx->appliedParams.useRowMatchFinder != ZSTD_ps_enable) {
        fprintf(stderr, "probe requires no-dict row backend\n");
        exit(2);
    }

    cParams = &ms->cParams;
    switch (cParams->strategy) {
        case ZSTD_greedy: depth = 0; break;
        case ZSTD_lazy: depth = 1; break;
        case ZSTD_lazy2:
        case ZSTD_btlazy2: depth = 2; break;
        default:
            fprintf(stderr, "probe requires greedy/lazy/lazy2 strategy\n");
            exit(2);
    }

    base = ms->window.base;
    prefixLowest = base + ms->window.dictLimit;
    mls = BOUNDED(4, cParams->minMatch, 6);
    rowLog = BOUNDED(4, cParams->searchLog, 6);
    ilimit = iend - 8 - ZSTD_ROW_HASH_CACHE_SIZE;
    if (state_pos + MINMATCH > src_size || probe_pos > (size_t)(ilimit - istart)) {
        fprintf(stderr, "probe position out of range\n");
        exit(2);
    }

    printf("state_pos %zu\n", state_pos);
    printf("probe_pos %zu\n", probe_pos);

    ip += (ip == prefixLowest);
    {
        U32 const curr = (U32)(ip - base);
        U32 const windowLow = ZSTD_getLowestPrefixIndex(ms, curr, cParams->windowLog);
        U32 const maxRep = curr - windowLow;
        if (offset_2 > maxRep) offset_2 = 0;
        if (offset_1 > maxRep) offset_1 = 0;
    }

    ms->lazySkipping = 0;
    ZSTD_row_fillHashCache(ms, base, rowLog, mls, ms->nextToUpdate, ilimit);

    while (ip < ilimit) {
        size_t matchLength = 0;
        size_t offBase = REPCODE1_TO_OFFBASE;
        const BYTE* start = ip + 1;

        if ((offset_1 > 0) & (MEM_read32(ip + 1 - offset_1) == MEM_read32(ip + 1))) {
            matchLength = ZSTD_count(ip + 5, ip + 5 - offset_1, iend) + 4;
        }

        {
            size_t ofbCandidate = 0;
            size_t const ml2 = ZSTD_searchMax(ms, ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
            if (!((depth == 0) && (matchLength >= 4)) && (ml2 > matchLength)) {
                matchLength = ml2;
                start = ip;
                offBase = ofbCandidate;
            }
        }

        if ((size_t)(ip - istart) == state_pos) {
            UpstreamNoDictRowSearchTrace searchTrace = {0};
            size_t searchOffBase = 0;
            size_t const searchMl = upstream_no_dict_row_search_probe(
                ms,
                istart + probe_pos,
                iend,
                mls,
                rowLog,
                &searchOffBase,
                &searchTrace
            );

            printf("visited 1\n");
            printf("anchor %zu\n", (size_t)(anchor - istart));
            printf("offsets %u %u\n", offset_1, offset_2);
            printf(
                "search_state %u %u %u %u %u %u %u %u %u\n",
                searchTrace.nextToUpdateBeforeSearch,
                searchTrace.hash,
                searchTrace.relRow,
                searchTrace.tag,
                searchTrace.lowLimit,
                searchTrace.attemptBudget,
                searchTrace.headIndex,
                searchTrace.insertIndex,
                searchTrace.groupWidth
            );
            upstream_row_probe_write_matches(
                "search_matches",
                searchTrace.matchPositions,
                searchTrace.matchIndices,
                searchTrace.matchCount
            );
            upstream_row_probe_write_visits(
                "search_visits",
                searchTrace.visitPositions,
                searchTrace.visitIndices,
                searchTrace.visitLengths,
                searchTrace.visitCount
            );
            upstream_row_probe_write_visit_states(
                "search_visit_states",
                searchTrace.visitGatePasses,
                searchTrace.visitWinnerLengths,
                searchTrace.visitWinnerOffBases,
                searchTrace.visitCount
            );
            printf("search_result %zu %zu\n", searchMl, searchMl >= 4 ? searchOffBase : 0);

            ZSTD_freeCCtx(cctx);
            return;
        }

        if (matchLength < 4) {
            size_t const step = ((size_t)(ip - anchor) >> kSearchStrength) + 1;
            ip += step;
            ms->lazySkipping = step > UPSTREAM_LAZY_SKIPPING_STEP;
            continue;
        }

        if (depth >= 1) {
            while (ip < ilimit) {
                ip++;
                if (offBase && ((offset_1 > 0) & (MEM_read32(ip) == MEM_read32(ip - offset_1)))) {
                    size_t const mlRep = ZSTD_count(ip + 4, ip + 4 - offset_1, iend) + 4;
                    int const gain2 = (int)(mlRep * 3);
                    int const gain1 = (int)(matchLength * 3 - ZSTD_highbit32((U32)offBase) + 1);
                    if ((mlRep >= 4) && (gain2 > gain1)) {
                        matchLength = mlRep;
                        offBase = REPCODE1_TO_OFFBASE;
                        start = ip;
                    }
                }

                {
                    size_t ofbCandidate = 0;
                    size_t const ml2 =
                        ZSTD_searchMax(ms, ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
                    int const gain2 = (int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate));
                    int const gain1 = (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 4);
                    if ((ml2 >= 4) && (gain2 > gain1)) {
                        matchLength = ml2;
                        offBase = ofbCandidate;
                        start = ip;
                        continue;
                    }
                }

                if ((depth == 2) && (ip < ilimit)) {
                    ip++;
                    if (offBase && ((offset_1 > 0) & (MEM_read32(ip) == MEM_read32(ip - offset_1)))) {
                        size_t const mlRep = ZSTD_count(ip + 4, ip + 4 - offset_1, iend) + 4;
                        int const gain2 = (int)(mlRep * 4);
                        int const gain1 = (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 1);
                        if ((mlRep >= 4) && (gain2 > gain1)) {
                            matchLength = mlRep;
                            offBase = REPCODE1_TO_OFFBASE;
                            start = ip;
                        }
                    }

                    {
                        size_t ofbCandidate = 0;
                        size_t const ml2 =
                            ZSTD_searchMax(ms, ip, iend, &ofbCandidate, mls, rowLog, search_rowHash, ZSTD_noDict);
                        int const gain2 = (int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate));
                        int const gain1 = (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 7);
                        if ((ml2 >= 4) && (gain2 > gain1)) {
                            matchLength = ml2;
                            offBase = ofbCandidate;
                            start = ip;
                            continue;
                        }
                    }
                }
                break;
            }
        }

        if (OFFBASE_IS_OFFSET(offBase)) {
            while (((start > anchor) & (start - OFFBASE_TO_OFFSET(offBase) > prefixLowest)) &&
                   (start[-1] == (start - OFFBASE_TO_OFFSET(offBase))[-1])) {
                start--;
                matchLength++;
            }
            offset_2 = offset_1;
            offset_1 = (U32)OFFBASE_TO_OFFSET(offBase);
        }

        anchor = ip = start + matchLength;
        if (ms->lazySkipping) {
            ZSTD_row_fillHashCache(ms, base, rowLog, mls, ms->nextToUpdate, ilimit);
            ms->lazySkipping = 0;
        }

        while (ip <= ilimit) {
            if (((offset_2 > 0) & (MEM_read32(ip) == MEM_read32(ip - offset_2)))) {
                size_t const matchLength2 = ZSTD_count(ip + 4, ip + 4 - offset_2, iend) + 4;
                size_t const offBase2 = offset_2;
                offset_2 = offset_1;
                offset_1 = (U32)offBase2;
                ip += matchLength2;
                anchor = ip;
                continue;
            }
            break;
        }
    }

    printf("visited 0\n");
    printf("anchor 0\n");
    printf("offsets 0 0\n");
    printf("search_state 0 0 0 0 0 0 0 0 0\n");
    printf("search_matches\n");
    printf("search_visits\n");
    printf("search_visit_states\n");
    printf("search_result 0 0\n");

    ZSTD_freeCCtx(cctx);
}

static size_t upstream_extdict_hc_search(
    ZSTD_MatchState_t* ms,
    const BYTE* const ip,
    const BYTE* const iLimit,
    size_t* offBasePtr,
    U32 const mls
) {
    const ZSTD_compressionParameters* const cParams = &ms->cParams;
    const BYTE* const base = ms->window.base;
    const BYTE* const dictBase = ms->window.dictBase;
    const U32 dictLimit = ms->window.dictLimit;
    const BYTE* const prefixStart = base + dictLimit;
    const BYTE* const dictEnd = dictBase + dictLimit;
    const U32 curr = (U32)(ip - base);
    const U32 maxDistance = 1U << cParams->windowLog;
    const U32 lowestValid = ms->window.lowLimit;
    const U32 withinMaxDistance = (curr - lowestValid > maxDistance) ? curr - maxDistance : lowestValid;
    const U32 lowLimit = ms->loadedDictEnd != 0 ? lowestValid : withinMaxDistance;
    const U32 chainMask = (1U << cParams->chainLog) - 1U;
    const U32 minChain = curr > (1U << cParams->chainLog) ? curr - (1U << cParams->chainLog) : 0;
    U32 nbAttempts = 1U << cParams->searchLog;
    U32 matchIndex;
    size_t ml = 4 - 1;

    *offBasePtr = 0;
    matchIndex = upstream_insert_and_find_first_index(ms, cParams, ip, mls, ms->lazySkipping);

    for (; (matchIndex >= lowLimit) && (nbAttempts > 0); nbAttempts--) {
        size_t currentMl = 0;
        if (matchIndex >= dictLimit) {
            const BYTE* const match = base + matchIndex;
            if (MEM_read32(match + ml - 3) == MEM_read32(ip + ml - 3)) {
                currentMl = ZSTD_count(ip, match, iLimit);
            }
        } else {
            const BYTE* const match = dictBase + matchIndex;
            if (match + 4 <= dictEnd && MEM_read32(match) == MEM_read32(ip)) {
                currentMl = ZSTD_count_2segments(ip + 4, match + 4, iLimit, dictEnd, prefixStart) + 4;
            }
        }

        if (currentMl > ml) {
            ml = currentMl;
            *offBasePtr = (size_t)(curr - matchIndex) + 3;
            if (ip + currentMl == iLimit) {
                break;
            }
        }

        if (matchIndex <= minChain) {
            break;
        }
        matchIndex = ms->chainTable[matchIndex & chainMask];
    }

    return ml;
}

static void write_trained_dict_extdict_lazy_probe(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t target_pos
) {
    unsigned char* dict;
    size_t dict_size;
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_CDict* cdict;
    ZSTD_MatchState_t* ms;
    const ZSTD_compressionParameters* cParams;
    const BYTE* const istart = (const BYTE*)src;
    const BYTE* ip = istart;
    const BYTE* anchor = istart;
    const BYTE* const iend = istart + src_size;
    const BYTE* const iLimit = iend;
    const BYTE* ilimit;
    const BYTE* base;
    const BYTE* dictBase;
    const BYTE* prefixStart;
    const BYTE* dictEnd;
    U32 dictLimit;
    U32 windowLog;
    U32 mls;
    U32 rowLog;
    U32 offset_1 = repStartValue[0];
    U32 offset_2 = repStartValue[1];
    U32 curr;
    U32 depth = 0;
    ZSTD_frameParameters fParams;
    ZSTD_dictMode_e dictMode;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (target_pos + MINMATCH > src_size) {
        fprintf(stderr, "probe position out of range\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }

    build_trained_dict(&dict, &dict_size);
    cdict = ZSTD_createCDict_advanced2(
        dict,
        dict_size,
        ZSTD_dlm_byRef,
        ZSTD_dct_auto,
        &cctx->requestedParams,
        ZSTD_defaultCMem
    );
    if (cdict == NULL) {
        fprintf(stderr, "create cdict failed\n");
        exit(2);
    }
    fParams.contentSizeFlag = 1;
    fParams.checksumFlag = (unsigned)checksum;
    fParams.noDictIDFlag = 0;
    {
        size_t const begin_result =
            ZSTD_compressBegin_usingCDict_advanced(cctx, cdict, fParams, src_size);
        if (ZSTD_isError(begin_result)) {
            fprintf(stderr, "compress begin failed: %s\n", ZSTD_getErrorName(begin_result));
            exit(2);
        }
    }

    ms = &cctx->blockState.matchState;
    if (!ZSTD_window_update(&ms->window, src, src_size, ms->forceNonContiguous)) {
        ms->forceNonContiguous = 0;
        ms->nextToUpdate = ms->window.dictLimit;
    }
    {
        size_t const maxDistance = 1U << cctx->appliedParams.cParams.windowLog;
        ZSTD_checkDictValidity(&ms->window, src + src_size, (U32)maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
        ZSTD_window_enforceMaxDist(&ms->window, src, (U32)maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
        if (ms->nextToUpdate < ms->window.lowLimit) {
            ms->nextToUpdate = ms->window.lowLimit;
        }
    }

    dictMode = ZSTD_matchState_dictMode(ms);
    printf("probe %zu\n", target_pos);
    printf(
        "backend %s\n",
        dictMode == ZSTD_noDict ? "nodict" :
        dictMode == ZSTD_extDict ? "extdict" :
        dictMode == ZSTD_dictMatchState ? "dictmatch" : "dds"
    );
    if (dictMode != ZSTD_extDict) {
            printf("unsupported 1\n");
            ZSTD_freeCDict(cdict);
            ZSTD_freeCCtx(cctx);
            free(dict);
            return;
    }

    cParams = &ms->cParams;
    switch (cParams->strategy) {
        case ZSTD_greedy: depth = 0; break;
        case ZSTD_lazy: depth = 1; break;
        case ZSTD_lazy2:
        case ZSTD_btlazy2: depth = 2; break;
        default:
            printf("unsupported 2\n");
            ZSTD_freeCDict(cdict);
            ZSTD_freeCCtx(cctx);
            free(dict);
            return;
    }

    base = ms->window.base;
    dictBase = ms->window.dictBase;
    dictLimit = ms->window.dictLimit;
    prefixStart = base + dictLimit;
    dictEnd = dictBase + dictLimit;
    windowLog = cParams->windowLog;
    mls = BOUNDED(4, cParams->minMatch, 6);
    rowLog = BOUNDED(4, cParams->searchLog, 6);
    ilimit = iend - 8;

    ms->lazySkipping = 0;
    ip += (ip == prefixStart);

    while (ip < ilimit) {
        size_t matchLength = 0;
        size_t offBase = REPCODE1_TO_OFFBASE;
        const BYTE* start = ip + 1;
        size_t baselineRepLength = 0;
        size_t baselineRegularLength = 0;
        size_t baselineRegularOffBase = 0;
        const char* baselineRegularSource = "none";
        size_t depth1RepLength = 0;
        size_t depth1RegularLength = 0;
        size_t depth1RegularOffBase = 0;
        const char* depth1RegularSource = "none";
        size_t depth2RepLength = 0;
        size_t depth2RegularLength = 0;
        size_t depth2RegularOffBase = 0;
        const char* depth2RegularSource = "none";
        size_t firstImmediateRep2Length = 0;

        curr = (U32)(ip - base);

        {
            const U32 windowLow = ZSTD_getLowestMatchIndex(ms, curr + 1, windowLog);
            const U32 repIndex = (U32)(curr + 1 - offset_1);
            const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
            const BYTE* const repMatch = repBase + repIndex;
            if ((ZSTD_index_overlap_check(dictLimit, repIndex))
             & (offset_1 <= curr + 1 - windowLow)) {
                if (MEM_read32(ip + 1) == MEM_read32(repMatch)) {
                    const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                    baselineRepLength =
                        ZSTD_count_2segments(ip + 1 + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                    matchLength = baselineRepLength;
                }
            }
        }

        {
                size_t ofbCandidate = 999999999;
            size_t const ml2 = upstream_extdict_hc_search(ms, ip, iend, &ofbCandidate, mls);
            baselineRegularLength = ml2;
            baselineRegularOffBase = ofbCandidate;
            baselineRegularSource = ml2 >= 4
                ? upstream_probe_source_from_offbase(ofbCandidate, ip, prefixStart)
                : "none";
            if (ml2 > matchLength) {
                matchLength = ml2;
                start = ip;
                offBase = ofbCandidate;
            }
        }

        if ((size_t)(ip - istart) == target_pos) {
            const BYTE* probeIp = ip;
            U32 probeCurr = curr;
            size_t probeMatchLength = matchLength;
            size_t probeOffBase = offBase;
            const BYTE* probeStart = start;
            if (probeMatchLength >= 4 && depth >= 1) {
                while (probeIp < ilimit) {
                    size_t probeStep;

                    probeIp++;
                    probeCurr++;
                    probeStep = (size_t)(probeIp - ip);
                    if (probeOffBase) {
                        const U32 windowLow = ZSTD_getLowestMatchIndex(ms, probeCurr, windowLog);
                        const U32 repIndex = (U32)(probeCurr - offset_1);
                        const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                        const BYTE* const repMatch = repBase + repIndex;
                        size_t repLength = 0;
                        if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                         & (offset_1 <= probeCurr - windowLow)) {
                            if (MEM_read32(probeIp) == MEM_read32(repMatch)) {
                                const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                                repLength =
                                    ZSTD_count_2segments(probeIp + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                            }
                        }
                        if (probeStep == 1) {
                            depth1RepLength = repLength;
                        } else if (probeStep == 2) {
                            depth2RepLength = repLength;
                        }
                        if ((repLength >= 4)
                         && ((int)(repLength * (probeStep == 1 ? 3 : 4)) >
                             (int)(probeMatchLength * (probeStep == 1 ? 3 : 4)
                                 - ZSTD_highbit32((U32)probeOffBase) + 1))) {
                            probeMatchLength = repLength;
                            probeOffBase = REPCODE1_TO_OFFBASE;
                            probeStart = probeIp;
                        }
                    }

                    {
                        size_t ofbCandidate = 999999999;
                        size_t const ml2 =
                            upstream_extdict_hc_search(ms, probeIp, iend, &ofbCandidate, mls);
                        if (probeStep == 1) {
                            depth1RegularLength = ml2;
                            depth1RegularOffBase = ofbCandidate;
                            depth1RegularSource = ml2 >= 4
                                ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                : "none";
                        } else if (probeStep == 2) {
                            depth2RegularLength = ml2;
                            depth2RegularOffBase = ofbCandidate;
                            depth2RegularSource = ml2 >= 4
                                ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                : "none";
                        }
                        if ((ml2 >= 4)
                         && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                             (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase)
                                 + (probeStep == 1 ? 4 : 7)))) {
                            probeMatchLength = ml2;
                            probeOffBase = ofbCandidate;
                            probeStart = probeIp;
                            continue;
                        }
                    }

                    if ((depth == 2) && (probeIp < ilimit)) {
                        probeIp++;
                        probeCurr++;
                        probeStep = (size_t)(probeIp - ip);
                        if (probeOffBase) {
                            const U32 windowLow = ZSTD_getLowestMatchIndex(ms, probeCurr, windowLog);
                            const U32 repIndex = (U32)(probeCurr - offset_1);
                            const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                            const BYTE* const repMatch = repBase + repIndex;
                            size_t repLength = 0;
                            if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                             & (offset_1 <= probeCurr - windowLow)) {
                                if (MEM_read32(probeIp) == MEM_read32(repMatch)) {
                                    const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                                    repLength =
                                        ZSTD_count_2segments(probeIp + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                                }
                            }
                            if (probeStep == 1) {
                                depth1RepLength = repLength;
                            } else if (probeStep == 2) {
                                depth2RepLength = repLength;
                            }
                            if ((repLength >= 4)
                             && ((int)(repLength * (probeStep == 1 ? 3 : 4)) >
                                 (int)(probeMatchLength * (probeStep == 1 ? 3 : 4)
                                     - ZSTD_highbit32((U32)probeOffBase) + 1))) {
                                probeMatchLength = repLength;
                                probeOffBase = REPCODE1_TO_OFFBASE;
                                probeStart = probeIp;
                            }
                        }

                        {
                            size_t ofbCandidate = 999999999;
                            size_t const ml2 =
                                upstream_extdict_hc_search(ms, probeIp, iend, &ofbCandidate, mls);
                            if (probeStep == 1) {
                                depth1RegularLength = ml2;
                                depth1RegularOffBase = ofbCandidate;
                                depth1RegularSource = ml2 >= 4
                                    ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                    : "none";
                            } else if (probeStep == 2) {
                                depth2RegularLength = ml2;
                                depth2RegularOffBase = ofbCandidate;
                                depth2RegularSource = ml2 >= 4
                                    ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                    : "none";
                            }
                            if ((ml2 >= 4)
                             && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                                 (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase)
                                     + (probeStep == 1 ? 4 : 7)))) {
                                probeMatchLength = ml2;
                                probeOffBase = ofbCandidate;
                                probeStart = probeIp;
                                continue;
                            }
                        }
                    }
                    break;
                }
            }

            if (OFFBASE_IS_OFFSET(probeOffBase)) {
                U32 const matchIndex = (U32)((size_t)(probeStart - base) - OFFBASE_TO_OFFSET(probeOffBase));
                const BYTE* match = (matchIndex < dictLimit) ? dictBase + matchIndex : base + matchIndex;
                const BYTE* const mStart = (matchIndex < dictLimit) ? dictBase + ms->window.lowLimit : prefixStart;
                while ((probeStart > anchor) && (match > mStart) && (probeStart[-1] == match[-1])) {
                    probeStart--;
                    match--;
                    probeMatchLength++;
                }
            }

            {
                U32 nextOffset1 = offset_1;
                U32 nextOffset2 = offset_2;
                const BYTE* nextIp;
                if (OFFBASE_IS_OFFSET(probeOffBase)) {
                    nextOffset2 = nextOffset1;
                    nextOffset1 = (U32)OFFBASE_TO_OFFSET(probeOffBase);
                }
                nextIp = probeStart + probeMatchLength;
                if (nextIp <= ilimit) {
                    const U32 repCurrent = (U32)(nextIp - base);
                    const U32 windowLow = ZSTD_getLowestMatchIndex(ms, repCurrent, windowLog);
                    const U32 repIndex = repCurrent - nextOffset2;
                    const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                    const BYTE* const repMatch = repBase + repIndex;
                    if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                     & (nextOffset2 <= repCurrent - windowLow)) {
                        if (MEM_read32(nextIp) == MEM_read32(repMatch)) {
                            const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                            firstImmediateRep2Length =
                                ZSTD_count_2segments(nextIp + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                        }
                    }
                }
            }

            printf("anchor %zu\n", (size_t)(anchor - istart));
            printf("offsets %u %u\n", offset_1, offset_2);
            printf("baseline_rep %zu\n", baselineRepLength);
            printf(
                "baseline_regular %s %zu %zu\n",
                baselineRegularSource,
                baselineRegularLength,
                baselineRegularOffBase
            );
            printf("depth1_rep %zu\n", depth1RepLength);
            printf(
                "depth1_regular %s %zu %zu\n",
                depth1RegularSource,
                depth1RegularLength,
                depth1RegularOffBase
            );
            printf("depth2_rep %zu\n", depth2RepLength);
            printf(
                "depth2_regular %s %zu %zu\n",
                depth2RegularSource,
                depth2RegularLength,
                depth2RegularOffBase
            );
            if (probeMatchLength >= 4) {
                printf(
                    "chosen %s %s %zu %zu %zu\n",
                    OFFBASE_IS_OFFSET(probeOffBase) ? "regular" : "rep",
                    upstream_probe_source_from_offbase(probeOffBase, probeStart, prefixStart),
                    (size_t)(probeStart - istart),
                    probeMatchLength,
                    probeOffBase
                );
            } else {
                printf("chosen none none 0 0 0\n");
            }
            printf("literal_length %zu\n", (size_t)(probeStart - anchor));
            printf("immediate_rep2 %zu\n", firstImmediateRep2Length);

            ZSTD_freeCDict(cdict);
            ZSTD_freeCCtx(cctx);
            free(dict);
            return;
        }

        if (matchLength < 4) {
            size_t const step = ((size_t)(ip - anchor) >> kSearchStrength);
            ip += step + 1;
            ms->lazySkipping = step > 8;
            continue;
        }

        if (depth >= 1) {
            while (ip < ilimit) {
                ip++;
                curr++;
                if (offBase) {
                    const U32 windowLow = ZSTD_getLowestMatchIndex(ms, curr, windowLog);
                    const U32 repIndex = (U32)(curr - offset_1);
                    const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                    const BYTE* const repMatch = repBase + repIndex;
                    if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                     & (offset_1 <= curr - windowLow)) {
                        if (MEM_read32(ip) == MEM_read32(repMatch)) {
                            const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                            size_t const repLength =
                                ZSTD_count_2segments(ip + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                            if ((repLength >= 4)
                             && ((int)(repLength * 3) >
                                 (int)(matchLength * 3 - ZSTD_highbit32((U32)offBase) + 1))) {
                                matchLength = repLength;
                                offBase = REPCODE1_TO_OFFBASE;
                                start = ip;
                            }
                        }
                    }
                }

                {
                    size_t ofbCandidate = 999999999;
                    size_t const ml2 =
                        upstream_extdict_hc_search(ms, ip, iend, &ofbCandidate, mls);
                    if ((ml2 >= 4)
                     && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                         (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 4))) {
                        matchLength = ml2;
                        offBase = ofbCandidate;
                        start = ip;
                        continue;
                    }
                }

                if ((depth == 2) && (ip < ilimit)) {
                    ip++;
                    curr++;
                    if (offBase) {
                        const U32 windowLow = ZSTD_getLowestMatchIndex(ms, curr, windowLog);
                        const U32 repIndex = (U32)(curr - offset_1);
                        const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                        const BYTE* const repMatch = repBase + repIndex;
                        if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                         & (offset_1 <= curr - windowLow)) {
                            if (MEM_read32(ip) == MEM_read32(repMatch)) {
                                const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                                size_t const repLength =
                                    ZSTD_count_2segments(ip + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                                if ((repLength >= 4)
                                 && ((int)(repLength * 4) >
                                     (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 1))) {
                                    matchLength = repLength;
                                    offBase = REPCODE1_TO_OFFBASE;
                                    start = ip;
                                }
                            }
                        }
                    }

                    {
                        size_t ofbCandidate = 999999999;
                        size_t const ml2 =
                            upstream_extdict_hc_search(ms, ip, iend, &ofbCandidate, mls);
                        if ((ml2 >= 4)
                         && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                             (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 7))) {
                            matchLength = ml2;
                            offBase = ofbCandidate;
                            start = ip;
                            continue;
                        }
                    }
                }
                break;
            }
        }

        if (OFFBASE_IS_OFFSET(offBase)) {
            U32 const matchIndex = (U32)((size_t)(start - base) - OFFBASE_TO_OFFSET(offBase));
            const BYTE* match = (matchIndex < dictLimit) ? dictBase + matchIndex : base + matchIndex;
            const BYTE* const mStart = (matchIndex < dictLimit) ? dictBase + ms->window.lowLimit : prefixStart;
            while ((start > anchor) && (match > mStart) && (start[-1] == match[-1])) {
                start--;
                match--;
                matchLength++;
            }
            offset_2 = offset_1;
            offset_1 = (U32)OFFBASE_TO_OFFSET(offBase);
        }

        anchor = ip = start + matchLength;
        if (ms->lazySkipping) {
            ms->lazySkipping = 0;
        }

        while (ip <= ilimit) {
            const U32 repCurrent = (U32)(ip - base);
            const U32 windowLow = ZSTD_getLowestMatchIndex(ms, repCurrent, windowLog);
            const U32 repIndex = repCurrent - offset_2;
            const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
            const BYTE* const repMatch = repBase + repIndex;
            if ((ZSTD_index_overlap_check(dictLimit, repIndex))
             & (offset_2 <= repCurrent - windowLow)) {
                if (MEM_read32(ip) == MEM_read32(repMatch)) {
                    size_t const repLength = ZSTD_count_2segments(
                        ip + 4,
                        repMatch + 4,
                        iend,
                        repIndex < dictLimit ? dictEnd : iend,
                        prefixStart
                    ) + 4;
                    size_t const offBase2 = offset_2;
                    offset_2 = offset_1;
                    offset_1 = (U32)offBase2;
                    ip += repLength;
                    anchor = ip;
                    continue;
                }
            }
            break;
        }
    }

    printf("anchor %zu\n", target_pos);
    printf("offsets %u %u\n", offset_1, offset_2);
    printf("baseline_rep 0\n");
    printf("baseline_regular none 0 0\n");
    printf("depth1_rep 0\n");
    printf("depth1_regular none 0 0\n");
    printf("depth2_rep 0\n");
    printf("depth2_regular none 0 0\n");
    printf("chosen none none 0 0 0\n");
    printf("literal_length 0\n");
    printf("immediate_rep2 0\n");

    ZSTD_freeCDict(cdict);
    ZSTD_freeCCtx(cctx);
    free(dict);
}

static void upstream_prepare_frame_chunk_block(
    ZSTD_CCtx* cctx,
    const BYTE* ip,
    size_t block_size
) {
    ZSTD_MatchState_t* const ms = &cctx->blockState.matchState;
    U32 const maxDistance = (U32)1 << cctx->appliedParams.cParams.windowLog;
    ZSTD_overflowCorrectIfNeeded(ms, &cctx->workspace, &cctx->appliedParams, ip, ip + block_size);
    ZSTD_checkDictValidity(&ms->window, ip + block_size, maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
    ZSTD_window_enforceMaxDist(&ms->window, ip, maxDistance, &ms->loadedDictEnd, &ms->dictMatchState);
    if (ms->nextToUpdate < ms->window.lowLimit) {
        ms->nextToUpdate = ms->window.lowLimit;
    }
}

static int upstream_replay_trained_dict_blocks_until(
    ZSTD_CCtx* cctx,
    const BYTE* src,
    size_t src_size,
    size_t target_block_index,
    const BYTE** block_start_out,
    size_t* block_size_out
) {
    size_t remaining = src_size;
    const BYTE* ip = src;
    size_t current_block = 0;
    S64 savings = (S64)cctx->consumedSrcSize - (S64)cctx->producedCSize;
    size_t replay_capacity = ZSTD_compressBound(cctx->blockSizeMax);
    BYTE* replay_dst = (BYTE*)malloc(replay_capacity ? replay_capacity : 1);

    if (replay_dst == NULL) {
        fprintf(stderr, "replay allocation failed\n");
        exit(2);
    }

    while (remaining) {
        size_t const block_size = ZSTD_optimalBlockSize(
            cctx,
            ip,
            remaining,
            cctx->blockSizeMax,
            cctx->appliedParams.preBlockSplitter_level,
            cctx->appliedParams.cParams.strategy,
            savings
        );
        if (current_block == target_block_index) {
            *block_start_out = ip;
            *block_size_out = block_size;
            free(replay_dst);
            return 1;
        }

        upstream_prepare_frame_chunk_block(cctx, ip, block_size);
        {
            size_t emitted_size;
            size_t const cSize = ZSTD_compressBlock_internal(
                cctx,
                replay_dst,
                replay_capacity,
                ip,
                block_size,
                1 /* frame */
            );
            if (ZSTD_isError(cSize)) {
                fprintf(stderr, "replay compressBlock_internal failed: %s\n", ZSTD_getErrorName(cSize));
                free(replay_dst);
                exit(2);
            }
            if (cSize == 0) {
                emitted_size = ZSTD_noCompressBlock(
                    replay_dst,
                    replay_capacity,
                    ip,
                    block_size,
                    0 /* lastBlock */
                );
                if (ZSTD_isError(emitted_size)) {
                    fprintf(stderr, "replay noCompressBlock failed: %s\n", ZSTD_getErrorName(emitted_size));
                    free(replay_dst);
                    exit(2);
                }
            } else {
                emitted_size = cSize + ZSTD_blockHeaderSize;
            }
            savings += (S64)block_size - (S64)emitted_size;
        }

        ip += block_size;
        remaining -= block_size;
        current_block += 1;
        cctx->isFirstBlock = 0;
    }

    free(replay_dst);
    return 0;
}

static void write_trained_dict_extdict_block_lazy_probe(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t block_index,
    size_t target_pos
) {
    unsigned char* dict;
    size_t dict_size;
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_CDict* cdict;
    ZSTD_MatchState_t* ms;
    const ZSTD_compressionParameters* cParams;
    const BYTE* const full_start = (const BYTE*)src;
    const BYTE* block_start;
    size_t block_size;
    const BYTE* ip;
    const BYTE* anchor;
    const BYTE* iend;
    const BYTE* const iLimit = full_start + src_size;
    const BYTE* ilimit;
    const BYTE* base;
    const BYTE* dictBase;
    const BYTE* prefixStart;
    const BYTE* dictEnd;
    U32 dictLimit;
    U32 windowLog;
    U32 mls;
    U32 rowLog;
    U32 offset_1;
    U32 offset_2;
    U32 curr;
    U32 depth = 0;
    ZSTD_frameParameters fParams;
    ZSTD_dictMode_e dictMode;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (target_pos + MINMATCH > src_size) {
        fprintf(stderr, "probe position out of range\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }

    build_trained_dict(&dict, &dict_size);
    cdict = ZSTD_createCDict_advanced2(
        dict,
        dict_size,
        ZSTD_dlm_byRef,
        ZSTD_dct_auto,
        &cctx->requestedParams,
        ZSTD_defaultCMem
    );
    if (cdict == NULL) {
        fprintf(stderr, "create cdict failed\n");
        exit(2);
    }
    fParams.contentSizeFlag = 1;
    fParams.checksumFlag = (unsigned)checksum;
    fParams.noDictIDFlag = 0;
    {
        size_t const begin_result =
            ZSTD_compressBegin_usingCDict_advanced(cctx, cdict, fParams, src_size);
        if (ZSTD_isError(begin_result)) {
            fprintf(stderr, "compress begin failed: %s\n", ZSTD_getErrorName(begin_result));
            exit(2);
        }
    }

    ms = &cctx->blockState.matchState;
    if (!ZSTD_window_update(&ms->window, src, src_size, ms->forceNonContiguous)) {
        ms->forceNonContiguous = 0;
        ms->nextToUpdate = ms->window.dictLimit;
    }

    if (!upstream_replay_trained_dict_blocks_until(
            cctx,
            full_start,
            src_size,
            block_index,
            &block_start,
            &block_size)) {
        printf("probe %zu\n", target_pos);
        printf("block %zu 0 0\n", block_index);
        printf("visited false\n");
        printf("backend none\n");
        ZSTD_freeCDict(cdict);
        ZSTD_freeCCtx(cctx);
        free(dict);
        return;
    }

    if (target_pos < (size_t)(block_start - full_start)
     || target_pos + MINMATCH > (size_t)(block_start - full_start) + block_size) {
        fprintf(stderr, "probe position is outside block %zu\n", block_index);
        exit(2);
    }

    upstream_prepare_frame_chunk_block(cctx, block_start, block_size);

    dictMode = ZSTD_matchState_dictMode(ms);
    printf("probe %zu\n", target_pos);
    printf("block %zu %zu %zu\n", block_index, (size_t)(block_start - full_start), block_size);
    printf("visited true\n");
    printf(
        "backend %s\n",
        dictMode == ZSTD_noDict ? "nodict" :
        dictMode == ZSTD_extDict ? "extdict" :
        dictMode == ZSTD_dictMatchState ? "dictmatch" : "dds"
    );
    if (dictMode != ZSTD_extDict) {
        printf("unsupported 1\n");
        ZSTD_freeCDict(cdict);
        ZSTD_freeCCtx(cctx);
        free(dict);
        return;
    }

    cParams = &ms->cParams;
    switch (cParams->strategy) {
        case ZSTD_greedy: depth = 0; break;
        case ZSTD_lazy: depth = 1; break;
        case ZSTD_lazy2:
        case ZSTD_btlazy2: depth = 2; break;
        default:
            printf("unsupported 2\n");
            ZSTD_freeCDict(cdict);
            ZSTD_freeCCtx(cctx);
            free(dict);
            return;
    }

    base = ms->window.base;
    dictBase = ms->window.dictBase;
    dictLimit = ms->window.dictLimit;
    prefixStart = base + dictLimit;
    dictEnd = dictBase + dictLimit;
    windowLog = cParams->windowLog;
    mls = BOUNDED(4, cParams->minMatch, 6);
    rowLog = BOUNDED(4, cParams->searchLog, 6);
    (void)rowLog;
    offset_1 = cctx->blockState.prevCBlock->rep[0];
    offset_2 = cctx->blockState.prevCBlock->rep[1];
    ip = block_start;
    anchor = block_start;
    iend = block_start + block_size;
    ilimit = iend - 8;

    ms->lazySkipping = 0;
    ip += (ip == prefixStart);

    while (ip < ilimit) {
        size_t matchLength = 0;
        size_t offBase = REPCODE1_TO_OFFBASE;
        const BYTE* start = ip + 1;
        size_t baselineRepLength = 0;
        size_t baselineRegularLength = 0;
        size_t baselineRegularOffBase = 0;
        const char* baselineRegularSource = "none";
        size_t depth1RepLength = 0;
        size_t depth1RegularLength = 0;
        size_t depth1RegularOffBase = 0;
        const char* depth1RegularSource = "none";
        size_t depth2RepLength = 0;
        size_t depth2RegularLength = 0;
        size_t depth2RegularOffBase = 0;
        const char* depth2RegularSource = "none";
        size_t firstImmediateRep2Length = 0;

        curr = (U32)(ip - base);

        {
            const U32 windowLow = ZSTD_getLowestMatchIndex(ms, curr + 1, windowLog);
            const U32 repIndex = (U32)(curr + 1 - offset_1);
            const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
            const BYTE* const repMatch = repBase + repIndex;
            if ((ZSTD_index_overlap_check(dictLimit, repIndex))
             & (offset_1 <= curr + 1 - windowLow)) {
                if (MEM_read32(ip + 1) == MEM_read32(repMatch)) {
                    const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                    baselineRepLength =
                        ZSTD_count_2segments(ip + 1 + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                    matchLength = baselineRepLength;
                }
            }
        }

        {
            size_t ofbCandidate = 999999999;
            size_t const ml2 = upstream_extdict_hc_search(ms, ip, iend, &ofbCandidate, mls);
            baselineRegularLength = ml2;
            baselineRegularOffBase = ofbCandidate;
            baselineRegularSource = ml2 >= 4
                ? upstream_probe_source_from_offbase(ofbCandidate, ip, prefixStart)
                : "none";
            if (ml2 > matchLength) {
                matchLength = ml2;
                start = ip;
                offBase = ofbCandidate;
            }
        }

        if ((size_t)(ip - full_start) == target_pos) {
            const BYTE* probeIp = ip;
            U32 probeCurr = curr;
            size_t probeMatchLength = matchLength;
            size_t probeOffBase = offBase;
            const BYTE* probeStart = start;
            if (probeMatchLength >= 4 && depth >= 1) {
                while (probeIp < ilimit) {
                    size_t probeStep;

                    probeIp++;
                    probeCurr++;
                    probeStep = (size_t)(probeIp - ip);
                    if (probeOffBase) {
                        const U32 windowLow = ZSTD_getLowestMatchIndex(ms, probeCurr, windowLog);
                        const U32 repIndex = (U32)(probeCurr - offset_1);
                        const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                        const BYTE* const repMatch = repBase + repIndex;
                        size_t repLength = 0;
                        if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                         & (offset_1 <= probeCurr - windowLow)) {
                            if (MEM_read32(probeIp) == MEM_read32(repMatch)) {
                                const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                                repLength =
                                    ZSTD_count_2segments(probeIp + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                            }
                        }
                        if (probeStep == 1) {
                            depth1RepLength = repLength;
                        } else if (probeStep == 2) {
                            depth2RepLength = repLength;
                        }
                        if ((repLength >= 4)
                         && ((int)(repLength * (probeStep == 1 ? 3 : 4)) >
                             (int)(probeMatchLength * (probeStep == 1 ? 3 : 4)
                                 - ZSTD_highbit32((U32)probeOffBase) + 1))) {
                            probeMatchLength = repLength;
                            probeOffBase = REPCODE1_TO_OFFBASE;
                            probeStart = probeIp;
                        }
                    }

                    {
                        size_t ofbCandidate = 999999999;
                        size_t const ml2 =
                            upstream_extdict_hc_search(ms, probeIp, iend, &ofbCandidate, mls);
                        if (probeStep == 1) {
                            depth1RegularLength = ml2;
                            depth1RegularOffBase = ofbCandidate;
                            depth1RegularSource = ml2 >= 4
                                ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                : "none";
                        } else if (probeStep == 2) {
                            depth2RegularLength = ml2;
                            depth2RegularOffBase = ofbCandidate;
                            depth2RegularSource = ml2 >= 4
                                ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                : "none";
                        }
                        if ((ml2 >= 4)
                         && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                             (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase)
                                 + (probeStep == 1 ? 4 : 7)))) {
                            probeMatchLength = ml2;
                            probeOffBase = ofbCandidate;
                            probeStart = probeIp;
                            continue;
                        }
                    }

                    if ((depth == 2) && (probeIp < ilimit)) {
                        probeIp++;
                        probeCurr++;
                        probeStep = (size_t)(probeIp - ip);
                        if (probeOffBase) {
                            const U32 windowLow = ZSTD_getLowestMatchIndex(ms, probeCurr, windowLog);
                            const U32 repIndex = (U32)(probeCurr - offset_1);
                            const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                            const BYTE* const repMatch = repBase + repIndex;
                            size_t repLength = 0;
                            if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                             & (offset_1 <= probeCurr - windowLow)) {
                                if (MEM_read32(probeIp) == MEM_read32(repMatch)) {
                                    const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                                    repLength =
                                        ZSTD_count_2segments(probeIp + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                                }
                            }
                            if (probeStep == 1) {
                                depth1RepLength = repLength;
                            } else if (probeStep == 2) {
                                depth2RepLength = repLength;
                            }
                            if ((repLength >= 4)
                             && ((int)(repLength * (probeStep == 1 ? 3 : 4)) >
                                 (int)(probeMatchLength * (probeStep == 1 ? 3 : 4)
                                     - ZSTD_highbit32((U32)probeOffBase) + 1))) {
                                probeMatchLength = repLength;
                                probeOffBase = REPCODE1_TO_OFFBASE;
                                probeStart = probeIp;
                            }
                        }

                        {
                            size_t ofbCandidate = 999999999;
                            size_t const ml2 =
                                upstream_extdict_hc_search(ms, probeIp, iend, &ofbCandidate, mls);
                            if (probeStep == 1) {
                                depth1RegularLength = ml2;
                                depth1RegularOffBase = ofbCandidate;
                                depth1RegularSource = ml2 >= 4
                                    ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                    : "none";
                            } else if (probeStep == 2) {
                                depth2RegularLength = ml2;
                                depth2RegularOffBase = ofbCandidate;
                                depth2RegularSource = ml2 >= 4
                                    ? upstream_probe_source_from_offbase(ofbCandidate, probeIp, prefixStart)
                                    : "none";
                            }
                            if ((ml2 >= 4)
                             && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                                 (int)(probeMatchLength * 4 - ZSTD_highbit32((U32)probeOffBase)
                                     + (probeStep == 1 ? 4 : 7)))) {
                                probeMatchLength = ml2;
                                probeOffBase = ofbCandidate;
                                probeStart = probeIp;
                                continue;
                            }
                        }
                    }
                    break;
                }
            }

            if (OFFBASE_IS_OFFSET(probeOffBase)) {
                U32 const matchIndex = (U32)((size_t)(probeStart - base) - OFFBASE_TO_OFFSET(probeOffBase));
                const BYTE* match = (matchIndex < dictLimit) ? dictBase + matchIndex : base + matchIndex;
                const BYTE* const mStart = (matchIndex < dictLimit) ? dictBase + ms->window.lowLimit : prefixStart;
                while ((probeStart > anchor) && (match > mStart) && (probeStart[-1] == match[-1])) {
                    probeStart--;
                    match--;
                    probeMatchLength++;
                }
            }

            {
                U32 nextOffset1 = offset_1;
                U32 nextOffset2 = offset_2;
                const BYTE* nextIp;
                if (OFFBASE_IS_OFFSET(probeOffBase)) {
                    nextOffset2 = nextOffset1;
                    nextOffset1 = (U32)OFFBASE_TO_OFFSET(probeOffBase);
                }
                nextIp = probeStart + probeMatchLength;
                if (nextIp <= ilimit) {
                    const U32 repCurrent = (U32)(nextIp - base);
                    const U32 windowLow = ZSTD_getLowestMatchIndex(ms, repCurrent, windowLog);
                    const U32 repIndex = repCurrent - nextOffset2;
                    const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                    const BYTE* const repMatch = repBase + repIndex;
                    if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                     & (nextOffset2 <= repCurrent - windowLow)) {
                        if (MEM_read32(nextIp) == MEM_read32(repMatch)) {
                            const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                            firstImmediateRep2Length =
                                ZSTD_count_2segments(nextIp + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                        }
                    }
                }
            }

            printf("anchor %zu\n", (size_t)(anchor - full_start));
            printf("offsets %u %u\n", offset_1, offset_2);
            printf("baseline_rep %zu\n", baselineRepLength);
            printf(
                "baseline_regular %s %zu %zu\n",
                baselineRegularSource,
                baselineRegularLength,
                baselineRegularOffBase
            );
            printf("depth1_rep %zu\n", depth1RepLength);
            printf(
                "depth1_regular %s %zu %zu\n",
                depth1RegularSource,
                depth1RegularLength,
                depth1RegularOffBase
            );
            printf("depth2_rep %zu\n", depth2RepLength);
            printf(
                "depth2_regular %s %zu %zu\n",
                depth2RegularSource,
                depth2RegularLength,
                depth2RegularOffBase
            );
            if (probeMatchLength >= 4) {
                printf(
                    "chosen %s %s %zu %zu %zu\n",
                    OFFBASE_IS_OFFSET(probeOffBase) ? "regular" : "rep",
                    upstream_probe_source_from_offbase(probeOffBase, probeStart, prefixStart),
                    (size_t)(probeStart - full_start),
                    probeMatchLength,
                    probeOffBase
                );
            } else {
                printf("chosen none none 0 0 0\n");
            }
            printf("literal_length %zu\n", (size_t)(probeStart - anchor));
            printf("immediate_rep2 %zu\n", firstImmediateRep2Length);

            ZSTD_freeCDict(cdict);
            ZSTD_freeCCtx(cctx);
            free(dict);
            return;
        }

        if (matchLength < 4) {
            size_t const step = ((size_t)(ip - anchor) >> kSearchStrength);
            ip += step + 1;
            ms->lazySkipping = step > 8;
            continue;
        }

        if (depth >= 1) {
            while (ip < ilimit) {
                ip++;
                curr++;
                if (offBase) {
                    const U32 windowLow = ZSTD_getLowestMatchIndex(ms, curr, windowLog);
                    const U32 repIndex = (U32)(curr - offset_1);
                    const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                    const BYTE* const repMatch = repBase + repIndex;
                    if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                     & (offset_1 <= curr - windowLow)) {
                        if (MEM_read32(ip) == MEM_read32(repMatch)) {
                            const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                            size_t const repLength =
                                ZSTD_count_2segments(ip + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                            if ((repLength >= 4)
                             && ((int)(repLength * 3) >
                                 (int)(matchLength * 3 - ZSTD_highbit32((U32)offBase) + 1))) {
                                matchLength = repLength;
                                offBase = REPCODE1_TO_OFFBASE;
                                start = ip;
                            }
                        }
                    }
                }

                {
                    size_t ofbCandidate = 999999999;
                    size_t const ml2 =
                        upstream_extdict_hc_search(ms, ip, iend, &ofbCandidate, mls);
                    if ((ml2 >= 4)
                     && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                         (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 4))) {
                        matchLength = ml2;
                        offBase = ofbCandidate;
                        start = ip;
                        continue;
                    }
                }

                if ((depth == 2) && (ip < ilimit)) {
                    ip++;
                    curr++;
                    if (offBase) {
                        const U32 windowLow = ZSTD_getLowestMatchIndex(ms, curr, windowLog);
                        const U32 repIndex = (U32)(curr - offset_1);
                        const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
                        const BYTE* const repMatch = repBase + repIndex;
                        if ((ZSTD_index_overlap_check(dictLimit, repIndex))
                         & (offset_1 <= curr - windowLow)) {
                            if (MEM_read32(ip) == MEM_read32(repMatch)) {
                                const BYTE* const repEnd = repIndex < dictLimit ? dictEnd : iend;
                                size_t const repLength =
                                    ZSTD_count_2segments(ip + 4, repMatch + 4, iend, repEnd, prefixStart) + 4;
                                if ((repLength >= 4)
                                 && ((int)(repLength * 4) >
                                     (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 1))) {
                                    matchLength = repLength;
                                    offBase = REPCODE1_TO_OFFBASE;
                                    start = ip;
                                }
                            }
                        }
                    }

                    {
                        size_t ofbCandidate = 999999999;
                        size_t const ml2 =
                            upstream_extdict_hc_search(ms, ip, iend, &ofbCandidate, mls);
                        if ((ml2 >= 4)
                         && ((int)(ml2 * 4 - ZSTD_highbit32((U32)ofbCandidate)) >
                             (int)(matchLength * 4 - ZSTD_highbit32((U32)offBase) + 7))) {
                            matchLength = ml2;
                            offBase = ofbCandidate;
                            start = ip;
                            continue;
                        }
                    }
                }
                break;
            }
        }

        if (OFFBASE_IS_OFFSET(offBase)) {
            U32 const matchIndex = (U32)((size_t)(start - base) - OFFBASE_TO_OFFSET(offBase));
            const BYTE* match = (matchIndex < dictLimit) ? dictBase + matchIndex : base + matchIndex;
            const BYTE* const mStart = (matchIndex < dictLimit) ? dictBase + ms->window.lowLimit : prefixStart;
            while ((start > anchor) && (match > mStart) && (start[-1] == match[-1])) {
                start--;
                match--;
                matchLength++;
            }
            offset_2 = offset_1;
            offset_1 = (U32)OFFBASE_TO_OFFSET(offBase);
        }

        anchor = ip = start + matchLength;
        if (ms->lazySkipping) {
            ms->lazySkipping = 0;
        }

        while (ip <= ilimit) {
            const U32 repCurrent = (U32)(ip - base);
            const U32 windowLow = ZSTD_getLowestMatchIndex(ms, repCurrent, windowLog);
            const U32 repIndex = repCurrent - offset_2;
            const BYTE* const repBase = repIndex < dictLimit ? dictBase : base;
            const BYTE* const repMatch = repBase + repIndex;
            if ((ZSTD_index_overlap_check(dictLimit, repIndex))
             & (offset_2 <= repCurrent - windowLow)) {
                if (MEM_read32(ip) == MEM_read32(repMatch)) {
                    size_t const repLength = ZSTD_count_2segments(
                        ip + 4,
                        repMatch + 4,
                        iend,
                        repIndex < dictLimit ? dictEnd : iend,
                        prefixStart
                    ) + 4;
                    size_t const offBase2 = offset_2;
                    offset_2 = offset_1;
                    offset_1 = (U32)offBase2;
                    ip += repLength;
                    anchor = ip;
                    continue;
                }
            }
            break;
        }
    }

    printf("anchor %zu\n", target_pos);
    printf("offsets %u %u\n", offset_1, offset_2);
    printf("baseline_rep 0\n");
    printf("baseline_regular none 0 0\n");
    printf("depth1_rep 0\n");
    printf("depth1_regular none 0 0\n");
    printf("depth2_rep 0\n");
    printf("depth2_regular none 0 0\n");
    printf("chosen none none 0 0 0\n");
    printf("literal_length 0\n");
    printf("immediate_rep2 0\n");

    ZSTD_freeCDict(cdict);
    ZSTD_freeCCtx(cctx);
    free(dict);
}

/* Build the context the compression benchmarks reuse across their iterations.
 *
 * This belongs outside the timing loop. The Rust side of the row reuses one
 * `Encoder` across iterations, and upstream's own `zstd -b` reuses its context,
 * so creating one per iteration charges upstream a workspace allocation and
 * match-table init that nothing it is being compared against pays. Measured on
 * this corpus it was worth up to 18% of the reported time at level 17 and
 * above, and under 5% elsewhere.
 *
 * Reuse is safe for repeated whole-frame compression: `ZSTD_compress2` opens
 * with `ZSTD_CCtx_reset(cctx, ZSTD_reset_session_only)`, which starts a fresh
 * frame while keeping the parameters, the workspace, and any loaded dictionary.
 * The emitted frame is identical to what a fresh context produces, which
 * matters because the report feeds that frame to the decode row and counts its
 * bytes in the ratio table. */
static ZSTD_CCtx* create_bench_cctx(int level, int checksum, const void* dict, size_t dict_size) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    if (dict != NULL && dict_size != 0) {
        if (ZSTD_isError(ZSTD_CCtx_loadDictionary(cctx, dict, dict_size))) {
            fprintf(stderr, "dictionary setup failed\n");
            exit(2);
        }
    }
    return cctx;
}

static size_t compress_with_cctx(
    ZSTD_CCtx* cctx,
    const unsigned char* src,
    size_t src_size,
    void* dst,
    size_t dst_capacity
) {
    size_t compressed_size = ZSTD_compress2(cctx, dst, dst_capacity, src, src_size);

    if (ZSTD_isError(compressed_size)) {
        fprintf(stderr, "compress failed: %s\n", ZSTD_getErrorName(compressed_size));
        exit(2);
    }
    return compressed_size;
}

/* One frame from a context of its own, for the subcommands that emit a single
 * frame and exit. The benchmark loops deliberately do not use this. */
static size_t compress_once(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    const void* dict,
    size_t dict_size,
    void* dst,
    size_t dst_capacity
) {
    ZSTD_CCtx* cctx = create_bench_cctx(level, checksum, dict, dict_size);
    size_t compressed_size = compress_with_cctx(cctx, src, src_size, dst, dst_capacity);

    ZSTD_freeCCtx(cctx);
    return compressed_size;
}

/* Apply one `name=value` setting from the advanced-parameter modes.
 *
 * An unrecognised name aborts rather than being ignored. Silently dropping a
 * misspelled override would leave both sides compressing at the level's own
 * parameters, and the resulting test would pass without having compared
 * anything -- which is the exact failure the override tests exist to catch. */
static void apply_advanced_setting(ZSTD_CCtx* cctx, const char* spec) {
    const char* eq = strchr(spec, '=');
    char name[32];
    size_t name_len;
    long long value;
    ZSTD_cParameter param;

    if (eq == NULL) {
        fprintf(stderr, "advanced setting '%s' is not name=value\n", spec);
        exit(2);
    }
    name_len = (size_t)(eq - spec);
    if (name_len == 0 || name_len >= sizeof(name)) {
        fprintf(stderr, "advanced setting '%s' has an unusable name\n", spec);
        exit(2);
    }
    memcpy(name, spec, name_len);
    name[name_len] = '\0';
    value = strtoll(eq + 1, NULL, 10);

    /* Not a ZSTD_c_* parameter: it lives on the context, not in the params. */
    if (strcmp(name, "pledgedSrcSize") == 0) {
        if (ZSTD_isError(ZSTD_CCtx_setPledgedSrcSize(cctx, (unsigned long long)value))) {
            fprintf(stderr, "pledgedSrcSize=%lld was rejected\n", value);
            exit(2);
        }
        return;
    }

    if (strcmp(name, "compressionLevel") == 0)   param = ZSTD_c_compressionLevel;
    else if (strcmp(name, "windowLog") == 0)     param = ZSTD_c_windowLog;
    else if (strcmp(name, "hashLog") == 0)       param = ZSTD_c_hashLog;
    else if (strcmp(name, "chainLog") == 0)      param = ZSTD_c_chainLog;
    else if (strcmp(name, "searchLog") == 0)     param = ZSTD_c_searchLog;
    else if (strcmp(name, "minMatch") == 0)      param = ZSTD_c_minMatch;
    else if (strcmp(name, "targetLength") == 0)  param = ZSTD_c_targetLength;
    else if (strcmp(name, "strategy") == 0)      param = ZSTD_c_strategy;
    else if (strcmp(name, "contentSizeFlag") == 0) param = ZSTD_c_contentSizeFlag;
    else if (strcmp(name, "checksumFlag") == 0)  param = ZSTD_c_checksumFlag;
    else if (strcmp(name, "dictIDFlag") == 0)    param = ZSTD_c_dictIDFlag;
    else if (strcmp(name, "srcSizeHint") == 0)   param = ZSTD_c_srcSizeHint;
    else if (strcmp(name, "format") == 0)        param = ZSTD_c_format;
    else if (strcmp(name, "enableLongDistanceMatching") == 0) param = ZSTD_c_enableLongDistanceMatching;
    else if (strcmp(name, "ldmHashLog") == 0)    param = ZSTD_c_ldmHashLog;
    else if (strcmp(name, "ldmMinMatch") == 0)   param = ZSTD_c_ldmMinMatch;
    else if (strcmp(name, "ldmBucketSizeLog") == 0) param = ZSTD_c_ldmBucketSizeLog;
    else if (strcmp(name, "ldmHashRateLog") == 0) param = ZSTD_c_ldmHashRateLog;
    else if (strcmp(name, "useRowMatchFinder") == 0) param = ZSTD_c_useRowMatchFinder;
    else if (strcmp(name, "literalCompressionMode") == 0) param = ZSTD_c_literalCompressionMode;
    else if (strcmp(name, "splitAfterSequences") == 0) param = ZSTD_c_splitAfterSequences;
    else {
        fprintf(stderr, "unknown advanced setting '%s'\n", name);
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, param, (int)value))) {
        fprintf(stderr, "setting %s=%lld was rejected\n", name, value);
        exit(2);
    }
}

/* Resolve `none` / `raw` / `trained` into dictionary bytes.
 *
 * `*owned_out` is set only for `trained`, whose bytes are built on the heap.
 * The other two modes point at static storage, so the caller frees `*owned_out`
 * and never the returned pointer. */
static void resolve_dict_mode(
    const char* mode,
    const void** dict_out,
    size_t* dict_size_out,
    unsigned char** owned_out
) {
    *owned_out = NULL;
    if (strcmp(mode, "none") == 0) {
        *dict_out = NULL;
        *dict_size_out = 0;
        return;
    }
    if (strcmp(mode, "raw") == 0) {
        *dict_out = RAW_DICT_BYTES;
        *dict_size_out = sizeof(RAW_DICT_BYTES) - 1;
        return;
    }
    if (strcmp(mode, "trained") == 0) {
        build_trained_dict(owned_out, dict_size_out);
        *dict_out = *owned_out;
        return;
    }
    fprintf(stderr, "unknown dictionary mode '%s'\n", mode);
    exit(2);
}

/* A context configured entirely by the caller's `name=value` list.
 *
 * Deliberately unlike create_bench_cctx, which forces contentSizeFlag on:
 * whether the frame declares a content size is one of the things these modes
 * exist to vary, so nothing is set that the caller did not ask for. */
static ZSTD_CCtx* create_advanced_cctx(
    const void* dict,
    size_t dict_size,
    char** settings,
    int setting_count
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    int i;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    for (i = 0; i < setting_count; ++i) {
        apply_advanced_setting(cctx, settings[i]);
    }
    if (dict != NULL && dict_size != 0) {
        if (ZSTD_isError(ZSTD_CCtx_loadDictionary(cctx, dict, dict_size))) {
            fprintf(stderr, "dictionary setup failed\n");
            exit(2);
        }
    }
    return cctx;
}

/* One frame driven through the streaming API, with the input handed over in
 * fixed-size pieces.
 *
 * Deliberately no ZSTD_CCtx_setPledgedSrcSize: a streaming caller that knew
 * the total length up front would be measuring one-shot with extra steps.
 * Without the pledge the frame header carries a window descriptor and no
 * content size, which is the shape our own StreamingEncoder emits, so the two
 * frames are comparable byte for byte.
 *
 * `piece_size` controls how often upstream is called, not how it splits the
 * frame: ZSTD_e_continue buffers internally and blocks fall on upstream's own
 * boundaries. That is the point of the mode -- those boundaries are what a
 * streaming parity check has to be measured against, and no one-shot mode can
 * report them.
 *
 * The destination grows rather than being sized once: ZSTD_compressBound
 * covers ZSTD_compress, and a streaming frame may carry more block headers
 * than the one-shot frame over the same bytes. Keeping ZSTD_CStreamOutSize
 * free before every call is the documented sufficient-room condition, so the
 * ZSTD_e_end loop is guaranteed to make progress and terminate. */
/* Drive an already-configured context through `ZSTD_compressStream2`, one
 * `piece_size` chunk at a time. Frees the context. Split out from
 * `compress_streaming` so the advanced mode can drive the same loop over a
 * context built from `name=value` settings rather than from a level. */
static size_t drive_streaming(
    ZSTD_CCtx* cctx,
    const unsigned char* src,
    size_t src_size,
    size_t piece_size,
    unsigned char** dst_out
) {
    const size_t headroom = ZSTD_CStreamOutSize();
    size_t capacity = ZSTD_compressBound(src_size) + headroom;
    unsigned char* dst = (unsigned char*)malloc(capacity);
    ZSTD_outBuffer out;
    ZSTD_inBuffer in;
    size_t consumed = 0;
    size_t remaining;

    if (dst == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    out.dst = dst;
    out.size = capacity;
    out.pos = 0;

    while (consumed < src_size) {
        const size_t available = src_size - consumed;
        const size_t piece = available < piece_size ? available : piece_size;

        in.src = src + consumed;
        in.size = piece;
        in.pos = 0;
        while (in.pos < in.size) {
            size_t rc;
            if (out.size - out.pos < headroom) {
                capacity += capacity / 2 + headroom;
                dst = (unsigned char*)realloc(dst, capacity);
                if (dst == NULL) {
                    fprintf(stderr, "allocation failed\n");
                    exit(2);
                }
                out.dst = dst;
                out.size = capacity;
            }
            rc = ZSTD_compressStream2(cctx, &out, &in, ZSTD_e_continue);
            if (ZSTD_isError(rc)) {
                fprintf(stderr, "compress-stream failed: %s\n", ZSTD_getErrorName(rc));
                exit(2);
            }
        }
        consumed += piece;
    }

    in.src = src;
    in.size = 0;
    in.pos = 0;
    do {
        if (out.size - out.pos < headroom) {
            capacity += capacity / 2 + headroom;
            dst = (unsigned char*)realloc(dst, capacity);
            if (dst == NULL) {
                fprintf(stderr, "allocation failed\n");
                exit(2);
            }
            out.dst = dst;
            out.size = capacity;
        }
        remaining = ZSTD_compressStream2(cctx, &out, &in, ZSTD_e_end);
        if (ZSTD_isError(remaining)) {
            fprintf(stderr, "compress-stream end failed: %s\n", ZSTD_getErrorName(remaining));
            exit(2);
        }
    } while (remaining != 0);

    /* The context is the caller's: `trace-advanced-streaming-cparams` reads
     * its applied parameters once the stream has resolved them. */
    *dst_out = dst;
    return out.pos;
}

static size_t compress_streaming(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    size_t piece_size,
    unsigned char** dst_out
) {
    ZSTD_CCtx* const cctx = create_bench_cctx(level, checksum, NULL, 0);
    size_t const written = drive_streaming(cctx, src, src_size, piece_size, dst_out);
    ZSTD_freeCCtx(cctx);
    return written;
}

static void write_sequence_trace(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    const void* dict,
    size_t dict_size,
    size_t max_sequences
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_Sequence* sequences;
    size_t sequence_capacity = ZSTD_sequenceBound(src_size);
    size_t sequence_count;
    size_t emitted = 0;
    size_t pos = 0;
    size_t i;
    int has_dictionary = dict != NULL && dict_size != 0;

    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    sequences = (ZSTD_Sequence*)malloc(sequence_capacity * sizeof(ZSTD_Sequence));
    if (sequences == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    if (has_dictionary) {
        if (ZSTD_isError(ZSTD_CCtx_loadDictionary(cctx, dict, dict_size))) {
            fprintf(stderr, "dictionary setup failed\n");
            exit(2);
        }
    }

    sequence_count = ZSTD_generateSequences(cctx, sequences, sequence_capacity, src, src_size);
    if (ZSTD_isError(sequence_count)) {
        fprintf(stderr, "generate-sequences failed: %s\n", ZSTD_getErrorName(sequence_count));
        exit(2);
    }

    for (i = 0; i < sequence_count && emitted < max_sequences; ++i) {
        const ZSTD_Sequence seq = sequences[i];
        size_t start;
        unsigned off_base;
        const char* kind;
        const char* source;

        if (seq.offset == 0 && seq.matchLength == 0) {
            pos += seq.litLength;
            continue;
        }

        start = pos + seq.litLength;
        off_base = seq.rep != 0 ? seq.rep : seq.offset + 3U;
        kind = seq.rep != 0 ? "rep" : "regular";
        if (has_dictionary && seq.offset > start) {
            source = cctx->blockState.matchState.dictMatchState != NULL ? "dict" : "prefix";
        } else {
            source = "source";
        }
        printf(
            "%s %s %zu %u %u %u %u\n",
            kind,
            source,
            start,
            seq.litLength,
            seq.matchLength,
            off_base,
            seq.offset
        );
        pos += seq.litLength + seq.matchLength;
        emitted += 1;
    }

    free(sequences);
    ZSTD_freeCCtx(cctx);
}

static void write_cdict_bt_state(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_CDict* cdict;
    unsigned char* dict;
    size_t dict_size;
    ZSTD_frameParameters fParams = {0, 0, 0};
    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    build_trained_dict(&dict, &dict_size);
    cdict = ZSTD_createCDict_advanced2(
        dict,
        dict_size,
        ZSTD_dlm_byRef,
        ZSTD_dct_auto,
        &cctx->requestedParams,
        ZSTD_defaultCMem
    );
    if (cdict == NULL) {
        fprintf(stderr, "create cdict failed\n");
        exit(2);
    }
    /* Access the CDict's internal match state */
    {
        const ZSTD_MatchState_t* dms = &cdict->matchState;
        const ZSTD_compressionParameters* cp = &cdict->matchState.cParams;
        U32 hashLog = cp->hashLog;
        U32 chainLog = cp->chainLog;
        U32 minMatch = cp->minMatch;
        U32 searchLog = cp->searchLog;
        U32 windowLog = cp->windowLog;
        U32 strategy = (U32)cp->strategy;
        U32 targetLength = cp->targetLength;
        U32 hashSize = 1u << hashLog;
        U32 btLog = chainLog - 1;
        U32 btSize = 1u << btLog;
        U32 btMask = btSize - 1;
        const U32* hashTable = dms->hashTable;
        const U32* chainTable = dms->chainTable;
        U32 i;
        uint64_t heads_hash = 0;
        uint64_t children_hash = 0;
        printf("cdict_windowLog %u\n", windowLog);
        printf("cdict_hashLog %u\n", hashLog);
        printf("cdict_chainLog %u\n", chainLog);
        printf("cdict_minMatch %u\n", minMatch);
        printf("cdict_searchLog %u\n", searchLog);
        printf("cdict_strategy %u\n", strategy);
        printf("cdict_targetLength %u\n", targetLength);
        printf("cdict_hashSize %u\n", hashSize);
        printf("cdict_btSize %u\n", btSize);
        printf("cdict_btMask %u\n", btMask);
        printf("cdict_dictSize %zu\n", dict_size);
        printf("cdict_lowLimit %u\n", dms->window.lowLimit);
        printf("cdict_dictLimit %u\n", dms->window.dictLimit);
        printf("cdict_nextToUpdate %u\n", dms->nextToUpdate);
        printf("cdict_window_base_offset %td\n", (ptrdiff_t)(dms->window.base - (const BYTE*)dict));
        /* Hash the heads array for quick comparison */
        for (i = 0; i < hashSize; i++) {
            heads_hash ^= (uint64_t)hashTable[i] * (uint64_t)(i + 1) * 2654435761ULL;
        }
        printf("cdict_heads_hash %llu\n", (unsigned long long)heads_hash);
        /* Hash the children (chainTable) array */
        for (i = 0; i < btSize * 2; i++) {
            children_hash ^= (uint64_t)chainTable[i] * (uint64_t)(i + 1) * 2654435761ULL;
        }
        printf("cdict_children_hash %llu\n", (unsigned long long)children_hash);
        /* Show first few hash slots and content bytes for verification */
        {
            const BYTE* dictContent = (const BYTE*)dms->window.base + dms->window.lowLimit;
            size_t contentSize = dms->nextToUpdate - dms->window.lowLimit;
            printf("cdict_contentSize %zu\n", contentSize);
            printf("cdict_content_first8");
            { size_t j; for (j = 0; j < 8 && j < contentSize; j++) printf(" %u", (unsigned)dictContent[j]); }
            printf("\n");
            /* Hash of first few positions using min_match=3 */
            printf("cdict_hash_pos0 %u\n", ZSTD_hashPtr(dictContent, hashLog, minMatch));
            if (contentSize > 1) printf("cdict_hash_pos1 %u\n", ZSTD_hashPtr(dictContent+1, hashLog, minMatch));
            if (contentSize > 2) printf("cdict_hash_pos2 %u\n", ZSTD_hashPtr(dictContent+2, hashLog, minMatch));
        }
        /* Dump first 32 heads entries and specific slots */
        printf("cdict_heads_sample");
        for (i = 0; i < 32 && i < hashSize; i++) {
            printf(" %u", hashTable[i]);
        }
        printf("\n");
        /* Dump specific head slots of interest */
        if (hashSize > 688) {
            printf("cdict_heads_688 %u\n", hashTable[688]);
        }
        /* Dump first 512 children entries */
        printf("cdict_children_sample");
        for (i = 0; i < 512 && i < btSize * 2; i++) {
            printf(" %u", chainTable[i]);
        }
        printf("\n");
    }
    ZSTD_freeCDict(cdict);
    free(dict);
    ZSTD_freeCCtx(cctx);
}

static void write_applied_cparams(
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    const void* dict,
    size_t dict_size
) {
    ZSTD_CCtx* cctx = ZSTD_createCCtx();
    ZSTD_Sequence* sequences;
    size_t sequence_capacity = ZSTD_sequenceBound(src_size);
    size_t sequence_count;
    if (cctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    sequences = (ZSTD_Sequence*)malloc(sequence_capacity * sizeof(ZSTD_Sequence));
    if (sequences == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_compressionLevel, level)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_contentSizeFlag, 1)) ||
        ZSTD_isError(ZSTD_CCtx_setParameter(cctx, ZSTD_c_checksumFlag, checksum))) {
        fprintf(stderr, "parameter setup failed\n");
        exit(2);
    }
    if (dict != NULL && dict_size != 0) {
        if (ZSTD_isError(ZSTD_CCtx_loadDictionary(cctx, dict, dict_size))) {
            fprintf(stderr, "dictionary setup failed\n");
            exit(2);
        }
    }
    sequence_count = ZSTD_generateSequences(cctx, sequences, sequence_capacity, src, src_size);
    if (ZSTD_isError(sequence_count)) {
        fprintf(stderr, "generate-sequences failed: %s\n", ZSTD_getErrorName(sequence_count));
        exit(2);
    }
    {
        const ZSTD_compressionParameters cp = cctx->appliedParams.cParams;
        printf(
            "%u %u %u %u %u %u %u\n",
            cp.windowLog,
            cp.chainLog,
            cp.hashLog,
            cp.searchLog,
            cp.minMatch,
            cp.targetLength,
            (unsigned)cp.strategy
        );
    }
    free(sequences);
    ZSTD_freeCCtx(cctx);
}

static size_t decompress_once(
    const unsigned char* src,
    size_t src_size,
    const void* dict,
    size_t dict_size,
    void* dst,
    size_t dst_capacity
) {
    ZSTD_DCtx* dctx = ZSTD_createDCtx();
    size_t decompressed_size;

    if (dctx == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }
    if (dict != NULL && dict_size != 0) {
        if (ZSTD_isError(ZSTD_DCtx_loadDictionary(dctx, dict, dict_size))) {
            fprintf(stderr, "dictionary setup failed\n");
            exit(2);
        }
    }

    decompressed_size = ZSTD_decompressDCtx(dctx, dst, dst_capacity, src, src_size);
    if (ZSTD_isError(decompressed_size)) {
        fprintf(stderr, "decompress failed: %s\n", ZSTD_getErrorName(decompressed_size));
        exit(2);
    }

    ZSTD_freeDCtx(dctx);
    return decompressed_size;
}

static void write_bench_result(uint64_t elapsed_ns, size_t last_output_size, uint64_t total_output_size) {
    printf("%llu %zu %llu\n",
        (unsigned long long)elapsed_ns,
        last_output_size,
        (unsigned long long)total_output_size
    );
}

static void write_bench_result_stderr(uint64_t elapsed_ns, size_t last_output_size, uint64_t total_output_size) {
    fprintf(stderr, "%llu %zu %llu\n",
        (unsigned long long)elapsed_ns,
        last_output_size,
        (unsigned long long)total_output_size
    );
}

static void bench_compress(
    size_t iterations,
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    const void* dict,
    size_t dict_size
) {
    size_t dst_capacity = ZSTD_compressBound(src_size);
    void* dst = malloc(dst_capacity ? dst_capacity : 1);
    ZSTD_CCtx* cctx = create_bench_cctx(level, checksum, dict, dict_size);
    size_t last_size = 0;
    uint64_t total_size = 0;
    uint64_t start_ns;
    size_t i;

    if (dst == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }

    start_ns = now_ns();
    for (i = 0; i < iterations; ++i) {
        last_size = compress_with_cctx(cctx, src, src_size, dst, dst_capacity);
        total_size += (uint64_t)last_size;
    }
    write_bench_result(now_ns() - start_ns, last_size, total_size);
    free(dst);
    ZSTD_freeCCtx(cctx);
}

static void bench_compress_with_output(
    size_t iterations,
    const unsigned char* src,
    size_t src_size,
    int level,
    int checksum,
    const void* dict,
    size_t dict_size
) {
    size_t dst_capacity = ZSTD_compressBound(src_size);
    void* dst = malloc(dst_capacity ? dst_capacity : 1);
    ZSTD_CCtx* cctx = create_bench_cctx(level, checksum, dict, dict_size);
    size_t last_size = 0;
    uint64_t total_size = 0;
    uint64_t start_ns;
    size_t i;

    if (dst == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }

    start_ns = now_ns();
    for (i = 0; i < iterations; ++i) {
        last_size = compress_with_cctx(cctx, src, src_size, dst, dst_capacity);
        total_size += (uint64_t)last_size;
    }
    write_bench_result_stderr(now_ns() - start_ns, last_size, total_size);
    write_all_stdout(dst, last_size);
    free(dst);
    ZSTD_freeCCtx(cctx);
}

static void bench_decompress(
    size_t iterations,
    const unsigned char* src,
    size_t src_size,
    const void* dict,
    size_t dict_size
) {
    unsigned long long dst_size_ull = ZSTD_findDecompressedSize(src, src_size);
    size_t dst_size;
    void* dst;
    size_t last_size = 0;
    uint64_t total_size = 0;
    uint64_t start_ns;
    size_t i;

    if (dst_size_ull == ZSTD_CONTENTSIZE_ERROR || dst_size_ull == ZSTD_CONTENTSIZE_UNKNOWN) {
        fprintf(stderr, "decompressed size unavailable\n");
        exit(2);
    }

    dst_size = (size_t)dst_size_ull;
    dst = malloc(dst_size ? dst_size : 1);
    if (dst == NULL) {
        fprintf(stderr, "allocation failed\n");
        exit(2);
    }

    start_ns = now_ns();
    for (i = 0; i < iterations; ++i) {
        last_size = decompress_once(src, src_size, dict, dict_size, dst, dst_size);
        total_size += (uint64_t)last_size;
    }
    write_bench_result(now_ns() - start_ns, last_size, total_size);
    free(dst);
}

int main(int argc, char** argv) {
    size_t src_size = 0;
    unsigned char* src = read_all_stdin(&src_size);

    if (argc < 2) {
        fprintf(stderr, "usage: helper <mode> [args]\n");
        return 2;
    }

    if (strcmp(argv[1], "emit-raw-dict") == 0) {
        write_all_stdout(RAW_DICT_BYTES, sizeof(RAW_DICT_BYTES) - 1);
        return 0;
    }

    if (strcmp(argv[1], "emit-trained-dict") == 0) {
        unsigned char* dict;
        size_t dict_size;
        build_trained_dict(&dict, &dict_size);
        write_all_stdout(dict, dict_size);
        free(dict);
        return 0;
    }

    /* Where the entropy header ends and the content begins, for either side's
     * dictionary: the content is a prefix of the selected material and its
     * length depends on the header size, so the boundary is needed to compare
     * two dictionaries' content at all. */
    if (strcmp(argv[1], "dict-header-size") == 0) {
        size_t header = ZDICT_getDictHeaderSize(src, src_size);
        if (ZDICT_isError(header)) {
            fprintf(stderr, "dict-header-size failed: %s\n", ZDICT_getErrorName(header));
            return 1;
        }
        printf("%u\n", (unsigned)header);
        fflush(stdout);
        return 0;
    }

    /* The histograms ZDICT_analyzeEntropy accumulates before it builds any
     * table, so a divergence in the trained header can be attributed to the
     * statistics or to the table construction rather than guessed at.
     * stdin is a u32 content length, the content, then the train-dict framing. */
    if (strcmp(argv[1], "dict-entropy-stats") == 0) {
        unsigned countLit[256];
        unsigned offcodeCount[31];
        unsigned matchLengthCount[53];
        unsigned litLengthCount[36];
        size_t content_len;
        const unsigned char* content;
        const unsigned char* rest;
        size_t rest_size;
        unsigned nb_samples;
        size_t header_size;
        size_t pos;
        unsigned u;
        int level;
        size_t total_src = 0;
        size_t average;
        ZSTD_parameters params;
        ZSTD_CDict* cdict;
        ZSTD_CCtx* zc;
        void* workspace;
        size_t blockSizeMax;

        if (argc != 3) {
            fprintf(stderr, "usage: helper dict-entropy-stats <level>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        if (src_size < 4) { fprintf(stderr, "dict-entropy-stats: truncated\n"); return 2; }
        content_len = (size_t)((unsigned)src[0] | ((unsigned)src[1] << 8)
                    | ((unsigned)src[2] << 16) | ((unsigned)src[3] << 24));
        content = src + 4;
        rest = content + content_len;
        rest_size = src_size - 4 - content_len;
        if (rest_size < 4) { fprintf(stderr, "dict-entropy-stats: truncated\n"); return 2; }
        nb_samples = (unsigned)rest[0] | ((unsigned)rest[1] << 8) | ((unsigned)rest[2] << 16)
                   | ((unsigned)rest[3] << 24);
        header_size = 4 + (size_t)nb_samples * 4;

        for (u = 0; u < 256; u++) countLit[u] = 1;
        for (u = 0; u < 31; u++) offcodeCount[u] = 1;
        for (u = 0; u < 53; u++) matchLengthCount[u] = 1;
        for (u = 0; u < 36; u++) litLengthCount[u] = 1;

        for (u = 0; u < nb_samples; u++) {
            const unsigned char* p = rest + 4 + (size_t)u * 4;
            total_src += (size_t)((unsigned)p[0] | ((unsigned)p[1] << 8)
                       | ((unsigned)p[2] << 16) | ((unsigned)p[3] << 24));
        }
        average = total_src / (nb_samples + !nb_samples);
        params = ZSTD_getParams(level ? level : ZSTD_CLEVEL_DEFAULT, average, content_len);
        blockSizeMax = ZSTD_BLOCKSIZE_MAX;
        if (blockSizeMax > ((size_t)1 << params.cParams.windowLog))
            blockSizeMax = (size_t)1 << params.cParams.windowLog;

        printf("params wlog=%u clog=%u hlog=%u slog=%u mml=%u tlen=%u strat=%u\n",
               params.cParams.windowLog, params.cParams.chainLog, params.cParams.hashLog,
               params.cParams.searchLog, params.cParams.minMatch, params.cParams.targetLength,
               (unsigned)params.cParams.strategy);

        cdict = ZSTD_createCDict_advanced(content, content_len, ZSTD_dlm_byRef,
                                          ZSTD_dct_rawContent, params.cParams, ZSTD_defaultCMem);
        zc = ZSTD_createCCtx();
        workspace = malloc(ZSTD_BLOCKSIZE_MAX);
        if (!cdict || !zc || !workspace) { fprintf(stderr, "allocation failed\n"); return 1; }

        pos = 0;
        for (u = 0; u < nb_samples; u++) {
            const unsigned char* p = rest + 4 + (size_t)u * 4;
            size_t sample_size = (size_t)((unsigned)p[0] | ((unsigned)p[1] << 8)
                               | ((unsigned)p[2] << 16) | ((unsigned)p[3] << 24));
            const unsigned char* sample = rest + header_size + pos;
            size_t use = sample_size > blockSizeMax ? blockSizeMax : sample_size;
            size_t cSize;
            pos += sample_size;
            if (ZSTD_isError(ZSTD_compressBegin_usingCDict_deprecated(zc, cdict))) continue;
            cSize = ZSTD_compressBlock_deprecated(zc, workspace, ZSTD_BLOCKSIZE_MAX, sample, use);
            if (ZSTD_isError(cSize) || cSize == 0) continue;
            {
                const SeqStore_t* const seqStore = ZSTD_getSeqStore(zc);
                const BYTE* bytePtr;
                U32 nbSeq = (U32)(seqStore->sequences - seqStore->sequencesStart);
                U32 i;
                for (bytePtr = seqStore->litStart; bytePtr < seqStore->lit; bytePtr++)
                    countLit[*bytePtr]++;
                ZSTD_seqToCodes(seqStore);
                for (i = 0; i < nbSeq; i++) offcodeCount[seqStore->ofCode[i]]++;
                for (i = 0; i < nbSeq; i++) matchLengthCount[seqStore->mlCode[i]]++;
                for (i = 0; i < nbSeq; i++) litLengthCount[seqStore->llCode[i]]++;
            }
        }

        printf("lit");   for (u = 0; u < 256; u++) printf(" %u", countLit[u]);          printf("\n");
        printf("off");   for (u = 0; u < 31; u++)  printf(" %u", offcodeCount[u]);      printf("\n");
        printf("ml");    for (u = 0; u < 53; u++)  printf(" %u", matchLengthCount[u]);  printf("\n");
        printf("ll");    for (u = 0; u < 36; u++)  printf(" %u", litLengthCount[u]);    printf("\n");
        fflush(stdout);
        ZSTD_freeCDict(cdict);
        ZSTD_freeCCtx(zc);
        free(workspace);
        return 0;
    }

    /* Train on samples supplied by the caller, rather than on the fixed corpus
     * `emit-trained-dict` carries. stdin is a little-endian u32 sample count,
     * that many u32 sample sizes, then the samples concatenated. */
    if (strcmp(argv[1], "train-dict") == 0) {
        unsigned char* dict;
        size_t dict_size;
        size_t capacity;
        unsigned nb_samples;
        size_t* sizes;
        size_t header_size;
        size_t total = 0;
        unsigned i;
        if (argc != 3 && argc != 6) {
            fprintf(stderr, "usage: helper train-dict <capacity> [k d steps]\n");
            return 2;
        }
        capacity = (size_t)parse_int_arg(argv[2], "capacity");
        if (src_size < 4) {
            fprintf(stderr, "train-dict: input is missing its sample count\n");
            return 2;
        }
        nb_samples = (unsigned)src[0] | ((unsigned)src[1] << 8) | ((unsigned)src[2] << 16)
                   | ((unsigned)src[3] << 24);
        header_size = 4 + (size_t)nb_samples * 4;
        if (src_size < header_size) {
            fprintf(stderr, "train-dict: input is missing its sample sizes\n");
            return 2;
        }
        sizes = (size_t*)malloc((nb_samples ? nb_samples : 1) * sizeof(size_t));
        dict = (unsigned char*)malloc(capacity ? capacity : 1);
        if (sizes == NULL || dict == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        for (i = 0; i < nb_samples; ++i) {
            const unsigned char* p = src + 4 + (size_t)i * 4;
            sizes[i] = (size_t)((unsigned)p[0] | ((unsigned)p[1] << 8) | ((unsigned)p[2] << 16)
                     | ((unsigned)p[3] << 24));
            total += sizes[i];
        }
        if (src_size != header_size + total) {
            fprintf(stderr, "train-dict: sample sizes do not add up to the input\n");
            return 2;
        }
        if (argc == 6) {
            /* Pin the search to one (k, d) so the selected content does not
             * depend on which candidate measured best. */
            ZDICT_fastCover_params_t params;
            memset(&params, 0, sizeof(params));
            params.k = (unsigned)parse_int_arg(argv[3], "k");
            params.d = (unsigned)parse_int_arg(argv[4], "d");
            params.steps = (unsigned)parse_int_arg(argv[5], "steps");
            params.zParams.compressionLevel = ZSTD_CLEVEL_DEFAULT;
            dict_size = ZDICT_optimizeTrainFromBuffer_fastCover(
                dict, capacity, src + header_size, sizes, nb_samples, &params);
        } else {
            dict_size = ZDICT_trainFromBuffer(dict, capacity, src + header_size, sizes, nb_samples);
        }
        free(sizes);
        if (ZDICT_isError(dict_size)) {
            fprintf(stderr, "train-dict failed: %s\n", ZDICT_getErrorName(dict_size));
            free(dict);
            return 1;
        }
        write_all_stdout(dict, dict_size);
        free(dict);
        return 0;
    }

    if (strcmp(argv[1], "compress-regular-configured") == 0) {
        int level;
        int checksum;
        size_t dst_capacity = ZSTD_compressBound(src_size);
        void* dst = malloc(dst_capacity ? dst_capacity : 1);
        size_t compressed_size;
        if (argc != 4) {
            fprintf(stderr, "usage: helper compress-regular-configured <level> <checksum>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        compressed_size = compress_once(src, src_size, level, checksum, NULL, 0, dst, dst_capacity);
        write_all_stdout(dst, compressed_size);
        free(dst);
        return 0;
    }

    if (strcmp(argv[1], "compress-regular-streaming-configured") == 0) {
        int level;
        int checksum;
        int piece_size;
        unsigned char* dst = NULL;
        size_t compressed_size;
        if (argc != 5) {
            fprintf(
                stderr,
                "usage: helper compress-regular-streaming-configured <level> <checksum> <piece>\n"
            );
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        piece_size = parse_int_arg(argv[4], "piece");
        if (piece_size <= 0) {
            fprintf(stderr, "piece must be positive\n");
            return 2;
        }
        compressed_size =
            compress_streaming(src, src_size, level, checksum, (size_t)piece_size, &dst);
        write_all_stdout(dst, compressed_size);
        free(dst);
        return 0;
    }

    if (strcmp(argv[1], "compress-raw-dict-configured") == 0) {
        int level;
        int checksum;
        size_t dst_capacity = ZSTD_compressBound(src_size);
        void* dst = malloc(dst_capacity ? dst_capacity : 1);
        size_t compressed_size;
        if (argc != 4) {
            fprintf(stderr, "usage: helper compress-raw-dict-configured <level> <checksum>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        compressed_size = compress_once(
            src,
            src_size,
            level,
            checksum,
            RAW_DICT_BYTES,
            sizeof(RAW_DICT_BYTES) - 1,
            dst,
            dst_capacity
        );
        write_all_stdout(dst, compressed_size);
        free(dst);
        return 0;
    }

    if (strcmp(argv[1], "compress-trained-dict-configured") == 0) {
        unsigned char* dict;
        size_t dict_size;
        int level;
        int checksum;
        size_t dst_capacity = ZSTD_compressBound(src_size);
        void* dst = malloc(dst_capacity ? dst_capacity : 1);
        size_t compressed_size;
        if (argc != 4) {
            fprintf(stderr, "usage: helper compress-trained-dict-configured <level> <checksum>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        build_trained_dict(&dict, &dict_size);
        compressed_size = compress_once(src, src_size, level, checksum, dict, dict_size, dst, dst_capacity);
        write_all_stdout(dst, compressed_size);
        free(dst);
        free(dict);
        return 0;
    }

    /* Compress with an arbitrary set of ZSTD_c_* settings.
     *
     * The narrow `compress-*-configured` modes above take a level and a
     * checksum flag and pin everything else. This one pins nothing: it is what
     * the parameter-override tests compare against, and the settings it does
     * not receive keep upstream's own defaults rather than this harness's. */
    if (strcmp(argv[1], "compress-advanced") == 0) {
        const void* dict;
        size_t dict_size;
        unsigned char* owned_dict;
        ZSTD_CCtx* cctx;
        size_t dst_capacity = ZSTD_compressBound(src_size);
        void* dst;
        size_t compressed_size;
        if (argc < 3) {
            fprintf(stderr, "usage: helper compress-advanced <dict-mode> [name=value ...]\n");
            return 2;
        }
        resolve_dict_mode(argv[2], &dict, &dict_size, &owned_dict);
        cctx = create_advanced_cctx(dict, dict_size, argv + 3, argc - 3);
        dst = malloc(dst_capacity ? dst_capacity : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        compressed_size = ZSTD_compress2(cctx, dst, dst_capacity, src, src_size);
        if (ZSTD_isError(compressed_size)) {
            fprintf(stderr, "compress-advanced failed: %s\n", ZSTD_getErrorName(compressed_size));
            return 1;
        }
        write_all_stdout(dst, compressed_size);
        ZSTD_freeCCtx(cctx);
        free(dst);
        free(owned_dict);
        return 0;
    }

    /* `compress-advanced` against a dictionary the caller supplies, rather than
     * one of `resolve_dict_mode`'s three built-ins.
     *
     * `tests/baseline.rs` builds its own dictionaries -- 112 KiB of raw content
     * and a 16 KiB trained one -- deliberately, so that grid needs no helper to
     * run. The size difference is not incidental: a CDict resolves its own
     * cparams, so a 112 KiB dictionary and a 512-byte one put the same level on
     * different strategies and different windows. Without this mode there is no
     * way to ask what upstream would have produced for a baseline row, and a
     * movement there can only be read as "ours changed", never as "ours moved
     * towards upstream" or away from it.
     *
     * stdin is a u32 dictionary length, the dictionary, then the body. */
    if (strcmp(argv[1], "compress-advanced-with-dict") == 0) {
        ZSTD_CCtx* cctx;
        size_t dict_size;
        const unsigned char* dict;
        const unsigned char* body;
        size_t body_size;
        size_t dst_capacity;
        void* dst;
        size_t compressed_size;
        if (src_size < 4) {
            fprintf(stderr, "compress-advanced-with-dict: input is shorter than its header\n");
            return 2;
        }
        dict_size = (size_t)((unsigned)src[0] | ((unsigned)src[1] << 8)
                  | ((unsigned)src[2] << 16) | ((unsigned)src[3] << 24));
        if (src_size - 4 < dict_size) {
            fprintf(stderr, "compress-advanced-with-dict: dictionary length overruns input\n");
            return 2;
        }
        dict = src + 4;
        body = dict + dict_size;
        body_size = src_size - 4 - dict_size;
        cctx = create_advanced_cctx(dict, dict_size, argv + 2, argc - 2);
        dst_capacity = ZSTD_compressBound(body_size);
        dst = malloc(dst_capacity ? dst_capacity : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        compressed_size = ZSTD_compress2(cctx, dst, dst_capacity, body, body_size);
        if (ZSTD_isError(compressed_size)) {
            fprintf(
                stderr,
                "compress-advanced-with-dict failed: %s\n",
                ZSTD_getErrorName(compressed_size)
            );
            return 1;
        }
        write_all_stdout(dst, compressed_size);
        ZSTD_freeCCtx(cctx);
        free(dst);
        return 0;
    }

    /* `trace-advanced-sequences` against a caller-supplied dictionary.
     *
     * The `<dict-mode>` form can only name the three dictionaries built into
     * this helper, so a parse driven by one of the crate's own dictionaries had
     * no upstream sequence list to be read against. Same stdin framing as
     * `compress-advanced-with-dict`: a u32 dictionary length, the dictionary,
     * then the body. */
    if (strcmp(argv[1], "trace-advanced-sequences-with-dict") == 0) {
        ZSTD_CCtx* cctx;
        size_t dict_size;
        const unsigned char* dict;
        const unsigned char* body;
        size_t body_size;
        ZSTD_Sequence* sequences;
        size_t sequence_capacity;
        size_t sequence_count;
        size_t pos = 0;
        size_t i;
        if (src_size < 4) {
            fprintf(stderr, "trace-advanced-sequences-with-dict: input is shorter than its header\n");
            return 2;
        }
        dict_size = (size_t)((unsigned)src[0] | ((unsigned)src[1] << 8)
                  | ((unsigned)src[2] << 16) | ((unsigned)src[3] << 24));
        if (src_size - 4 < dict_size) {
            fprintf(stderr, "trace-advanced-sequences-with-dict: dictionary length overruns input\n");
            return 2;
        }
        dict = src + 4;
        body = dict + dict_size;
        body_size = src_size - 4 - dict_size;
        cctx = create_advanced_cctx(dict, dict_size, argv + 2, argc - 2);
        sequence_capacity = ZSTD_sequenceBound(body_size);
        sequences = (ZSTD_Sequence*)malloc(sequence_capacity * sizeof(ZSTD_Sequence));
        if (sequences == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        sequence_count = ZSTD_generateSequences(cctx, sequences, sequence_capacity, body, body_size);
        if (ZSTD_isError(sequence_count)) {
            fprintf(
                stderr,
                "trace-advanced-sequences-with-dict failed: %s\n",
                ZSTD_getErrorName(sequence_count)
            );
            return 1;
        }
        for (i = 0; i < sequence_count; ++i) {
            const ZSTD_Sequence seq = sequences[i];
            if (seq.offset == 0 && seq.matchLength == 0) {
                printf("block-end %zu %u\n", pos + seq.litLength, seq.litLength);
                pos += seq.litLength;
                continue;
            }
            printf(
                "%zu %u %u %u %u\n",
                pos + seq.litLength,
                seq.litLength,
                seq.matchLength,
                seq.rep != 0 ? seq.rep : seq.offset + 3U,
                seq.offset
            );
            pos += seq.litLength + seq.matchLength;
        }
        ZSTD_freeCCtx(cctx);
        free(sequences);
        return 0;
    }

    /* `compress-advanced-with-dict` through the streaming API, for the same
     * reason `compress-advanced-streaming` exists next to `compress-advanced`:
     * a frame upstream builds in pieces is laid out differently from one it
     * builds in a single call, so a streamed baseline row has no one-shot
     * equivalent to be compared against.
     *
     * stdin is a u32 dictionary length, the dictionary, then the body. */
    if (strcmp(argv[1], "compress-advanced-streaming-with-dict") == 0) {
        ZSTD_CCtx* cctx;
        size_t dict_size;
        const unsigned char* dict;
        const unsigned char* body;
        size_t body_size;
        int piece_size;
        unsigned char* dst = NULL;
        size_t compressed_size;
        if (argc < 3) {
            fprintf(
                stderr,
                "usage: helper compress-advanced-streaming-with-dict <piece> [name=value ...]\n"
            );
            return 2;
        }
        piece_size = parse_int_arg(argv[2], "piece");
        if (piece_size <= 0) {
            fprintf(stderr, "piece must be positive\n");
            return 2;
        }
        if (src_size < 4) {
            fprintf(stderr, "compress-advanced-streaming-with-dict: input is shorter than its header\n");
            return 2;
        }
        dict_size = (size_t)((unsigned)src[0] | ((unsigned)src[1] << 8)
                  | ((unsigned)src[2] << 16) | ((unsigned)src[3] << 24));
        if (src_size - 4 < dict_size) {
            fprintf(stderr, "compress-advanced-streaming-with-dict: dictionary length overruns input\n");
            return 2;
        }
        dict = src + 4;
        body = dict + dict_size;
        body_size = src_size - 4 - dict_size;
        cctx = create_advanced_cctx(dict, dict_size, argv + 3, argc - 3);
        compressed_size = drive_streaming(cctx, body, body_size, (size_t)piece_size, &dst);
        write_all_stdout(dst, compressed_size);
        ZSTD_freeCCtx(cctx);
        free(dst);
        return 0;
    }

    /* `compress-advanced`, driven through the streaming API a piece at a time.
     *
     * Neither of the two modes it sits between can stand in for it. A frame
     * upstream compresses in one call is laid out differently from one it
     * compresses in pieces -- `ZSTD_compress_frameChunk` declines to pre-split
     * a chunk below 128 KiB, so the buffered path yields at most two blocks per
     * chunk -- and `compress-regular-streaming-configured` takes a level and a
     * checksum flag and pins everything else, so it cannot ask for long-distance
     * matching or a window. */
    if (strcmp(argv[1], "compress-advanced-streaming") == 0) {
        const void* dict;
        size_t dict_size;
        unsigned char* owned_dict;
        ZSTD_CCtx* cctx;
        int piece_size;
        unsigned char* dst = NULL;
        size_t compressed_size;
        if (argc < 4) {
            fprintf(
                stderr,
                "usage: helper compress-advanced-streaming <dict-mode> <piece> [name=value ...]\n"
            );
            return 2;
        }
        resolve_dict_mode(argv[2], &dict, &dict_size, &owned_dict);
        piece_size = parse_int_arg(argv[3], "piece");
        if (piece_size <= 0) {
            fprintf(stderr, "piece must be positive\n");
            return 2;
        }
        cctx = create_advanced_cctx(dict, dict_size, argv + 4, argc - 4);
        compressed_size = drive_streaming(cctx, src, src_size, (size_t)piece_size, &dst);
        write_all_stdout(dst, compressed_size);
        ZSTD_freeCCtx(cctx);
        free(dst);
        free(owned_dict);
        return 0;
    }

    /* `trace-advanced-cparams` for the streaming path. The two differ: the
     * one-shot API knows the source size and `ZSTD_adjustCParams` uses it,
     * where a stream with no pledge does not. */
    if (strcmp(argv[1], "trace-advanced-streaming-cparams") == 0) {
        const void* dict;
        size_t dict_size;
        unsigned char* owned_dict;
        ZSTD_CCtx* cctx;
        int piece_size;
        unsigned char* dst = NULL;
        size_t compressed_size;
        if (argc < 4) {
            fprintf(
                stderr,
                "usage: helper trace-advanced-streaming-cparams <dict-mode> <piece> [name=value ...]\n"
            );
            return 2;
        }
        resolve_dict_mode(argv[2], &dict, &dict_size, &owned_dict);
        piece_size = parse_int_arg(argv[3], "piece");
        if (piece_size <= 0) {
            fprintf(stderr, "piece must be positive\n");
            return 2;
        }
        cctx = create_advanced_cctx(dict, dict_size, argv + 4, argc - 4);
        compressed_size = drive_streaming(cctx, src, src_size, (size_t)piece_size, &dst);
        (void)compressed_size;
        {
            const ZSTD_compressionParameters cp = cctx->appliedParams.cParams;
            printf(
                "%u %u %u %u %u %u %u\n",
                cp.windowLog,
                cp.chainLog,
                cp.hashLog,
                cp.searchLog,
                cp.minMatch,
                cp.targetLength,
                (unsigned)cp.strategy
            );
        }
        ZSTD_freeCCtx(cctx);
        free(dst);
        free(owned_dict);
        return 0;
    }

    /* The sequences `compress-advanced` would parse, one per line, with the
     * block boundaries `ZSTD_generateSequences` marks kept rather than
     * skipped: a divergence is usually easier to place once you know which
     * block it is in. */
    if (strcmp(argv[1], "trace-advanced-sequences") == 0) {
        const void* dict;
        size_t dict_size;
        unsigned char* owned_dict;
        ZSTD_CCtx* cctx;
        ZSTD_Sequence* sequences;
        size_t sequence_capacity = ZSTD_sequenceBound(src_size);
        size_t sequence_count;
        size_t pos = 0;
        size_t i;
        if (argc < 3) {
            fprintf(stderr, "usage: helper trace-advanced-sequences <dict-mode> [name=value ...]\n");
            return 2;
        }
        resolve_dict_mode(argv[2], &dict, &dict_size, &owned_dict);
        cctx = create_advanced_cctx(dict, dict_size, argv + 3, argc - 3);
        sequences = (ZSTD_Sequence*)malloc(sequence_capacity * sizeof(ZSTD_Sequence));
        if (sequences == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        sequence_count = ZSTD_generateSequences(cctx, sequences, sequence_capacity, src, src_size);
        if (ZSTD_isError(sequence_count)) {
            fprintf(stderr, "trace-advanced-sequences failed: %s\n", ZSTD_getErrorName(sequence_count));
            return 1;
        }
        for (i = 0; i < sequence_count; ++i) {
            const ZSTD_Sequence seq = sequences[i];
            if (seq.offset == 0 && seq.matchLength == 0) {
                printf("block-end %zu %u\n", pos + seq.litLength, seq.litLength);
                pos += seq.litLength;
                continue;
            }
            printf(
                "%zu %u %u %u %u\n",
                pos + seq.litLength,
                seq.litLength,
                seq.matchLength,
                seq.rep != 0 ? seq.rep : seq.offset + 3U,
                seq.offset
            );
            pos += seq.litLength + seq.matchLength;
        }
        ZSTD_freeCCtx(cctx);
        free(sequences);
        free(owned_dict);
        return 0;
    }

    /* The compression parameters `compress-advanced` would actually apply.
     *
     * Reads them back off the context after a real compression rather than
     * recomputing them, so the answer is what drove the parse and not a second
     * opinion about what should have. */
    if (strcmp(argv[1], "trace-advanced-cparams") == 0) {
        const void* dict;
        size_t dict_size;
        unsigned char* owned_dict;
        ZSTD_CCtx* cctx;
        size_t dst_capacity = ZSTD_compressBound(src_size);
        void* dst;
        size_t compressed_size;
        if (argc < 3) {
            fprintf(stderr, "usage: helper trace-advanced-cparams <dict-mode> [name=value ...]\n");
            return 2;
        }
        resolve_dict_mode(argv[2], &dict, &dict_size, &owned_dict);
        cctx = create_advanced_cctx(dict, dict_size, argv + 3, argc - 3);
        dst = malloc(dst_capacity ? dst_capacity : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        compressed_size = ZSTD_compress2(cctx, dst, dst_capacity, src, src_size);
        if (ZSTD_isError(compressed_size)) {
            fprintf(stderr, "trace-advanced-cparams failed: %s\n", ZSTD_getErrorName(compressed_size));
            return 1;
        }
        {
            const ZSTD_compressionParameters cp = cctx->appliedParams.cParams;
            printf(
                "%u %u %u %u %u %u %u\n",
                cp.windowLog,
                cp.chainLog,
                cp.hashLog,
                cp.searchLog,
                cp.minMatch,
                cp.targetLength,
                (unsigned)cp.strategy
            );
        }
        ZSTD_freeCCtx(cctx);
        free(dst);
        free(owned_dict);
        return 0;
    }

    if (strcmp(argv[1], "trace-advanced-ldm-params") == 0) {
        const void* dict;
        size_t dict_size;
        unsigned char* owned_dict;
        ZSTD_CCtx* cctx;
        size_t dst_capacity = ZSTD_compressBound(src_size);
        void* dst;
        size_t compressed_size;
        if (argc < 3) {
            fprintf(stderr, "usage: helper trace-advanced-ldm-params <dict-mode> [name=value ...]\n");
            return 2;
        }
        resolve_dict_mode(argv[2], &dict, &dict_size, &owned_dict);
        cctx = create_advanced_cctx(dict, dict_size, argv + 3, argc - 3);
        dst = malloc(dst_capacity ? dst_capacity : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 1;
        }
        /* Compress rather than only reset: the derivation runs inside
         * ZSTD_resetCCtx_internal, so appliedParams is not filled in until a
         * frame has actually been started. */
        compressed_size = ZSTD_compress2(cctx, dst, dst_capacity, src, src_size);
        if (ZSTD_isError(compressed_size)) {
            fprintf(stderr, "trace-advanced-ldm-params failed: %s\n", ZSTD_getErrorName(compressed_size));
            return 1;
        }
        {
            const ldmParams_t lp = cctx->appliedParams.ldmParams;
            printf(
                "%u %u %u %u %u %u\n",
                (unsigned)lp.enableLdm,
                lp.hashLog,
                lp.minMatchLength,
                lp.bucketSizeLog,
                lp.hashRateLog,
                lp.windowLog
            );
        }
        ZSTD_freeCCtx(cctx);
        free(dst);
        free(owned_dict);
        return 0;
    }

    if (strcmp(argv[1], "trace-trained-dict-sequences-configured") == 0) {
        unsigned char* dict;
        size_t dict_size;
        int level;
        int checksum;
        size_t max_sequences;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-trained-dict-sequences-configured <level> <checksum> <max-sequences>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        max_sequences = (size_t)parse_int_arg(argv[4], "max-sequences");
        build_trained_dict(&dict, &dict_size);
        write_sequence_trace(src, src_size, level, checksum, dict, dict_size, max_sequences);
        free(dict);
        return 0;
    }

    if (strcmp(argv[1], "trace-raw-dict-sequences-configured") == 0) {
        int level;
        int checksum;
        size_t max_sequences;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-raw-dict-sequences-configured <level> <checksum> <max-sequences>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        max_sequences = (size_t)parse_int_arg(argv[4], "max-sequences");
        write_sequence_trace(src, src_size, level, checksum, RAW_DICT_BYTES, sizeof(RAW_DICT_BYTES) - 1, max_sequences);
        return 0;
    }

    if (strcmp(argv[1], "trace-regular-sequences-configured") == 0) {
        int level;
        int checksum;
        size_t max_sequences;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-regular-sequences-configured <level> <checksum> <max-sequences>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        max_sequences = (size_t)parse_int_arg(argv[4], "max-sequences");
        write_sequence_trace(src, src_size, level, checksum, NULL, 0, max_sequences);
        return 0;
    }

    if (strcmp(argv[1], "trace-trained-dict-cparams-configured") == 0) {
        unsigned char* dict;
        size_t dict_size;
        int level;
        int checksum;
        if (argc != 4) {
            fprintf(stderr, "usage: helper trace-trained-dict-cparams-configured <level> <checksum>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        build_trained_dict(&dict, &dict_size);
        write_applied_cparams(src, src_size, level, checksum, dict, dict_size);
        free(dict);
        return 0;
    }

    if (strcmp(argv[1], "dump-cdict-bt-state") == 0) {
        int level;
        int checksum;
        if (argc != 4) {
            fprintf(stderr, "usage: helper dump-cdict-bt-state <level> <checksum>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        write_cdict_bt_state(src, src_size, level, checksum);
        return 0;
    }

    if (strcmp(argv[1], "trace-regular-cparams-configured") == 0) {
        int level;
        int checksum;
        if (argc != 4) {
            fprintf(stderr, "usage: helper trace-regular-cparams-configured <level> <checksum>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        write_applied_cparams(src, src_size, level, checksum, NULL, 0);
        return 0;
    }

    if (strcmp(argv[1], "trace-trained-dict-hc-probe-configured") == 0) {
        int level;
        int checksum;
        size_t pos;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-trained-dict-hc-probe-configured <level> <checksum> <pos>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        pos = (size_t)parse_int_arg(argv[4], "pos");
        write_trained_dict_hc_probe(src, src_size, level, checksum, pos);
        return 0;
    }

    if (strcmp(argv[1], "trace-trained-dict-extdict-lazy-probe-configured") == 0) {
        int level;
        int checksum;
        size_t pos;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-trained-dict-extdict-lazy-probe-configured <level> <checksum> <pos>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        pos = (size_t)parse_int_arg(argv[4], "pos");
        write_trained_dict_extdict_lazy_probe(src, src_size, level, checksum, pos);
        return 0;
    }

    if (strcmp(argv[1], "trace-trained-dict-extdict-block-lazy-probe-configured") == 0) {
        int level;
        int checksum;
        size_t block_index;
        size_t pos;
        if (argc != 6) {
            fprintf(stderr, "usage: helper trace-trained-dict-extdict-block-lazy-probe-configured <level> <checksum> <block-index> <pos>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        block_index = (size_t)parse_int_arg(argv[4], "block-index");
        pos = (size_t)parse_int_arg(argv[5], "pos");
        write_trained_dict_extdict_block_lazy_probe(src, src_size, level, checksum, block_index, pos);
        return 0;
    }

    if (strcmp(argv[1], "trace-no-dict-row-lazy-probe-configured") == 0) {
        int level;
        int checksum;
        size_t pos;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-no-dict-row-lazy-probe-configured <level> <checksum> <pos>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        pos = (size_t)parse_int_arg(argv[4], "pos");
        write_no_dict_row_lazy_probe(src, src_size, level, checksum, pos);
        return 0;
    }

    if (strcmp(argv[1], "trace-no-dict-row-search-probe-configured") == 0) {
        int level;
        int checksum;
        size_t state_pos;
        size_t probe_pos;
        if (argc != 6) {
            fprintf(stderr, "usage: helper trace-no-dict-row-search-probe-configured <level> <checksum> <state-pos> <probe-pos>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        state_pos = (size_t)parse_int_arg(argv[4], "state-pos");
        probe_pos = (size_t)parse_int_arg(argv[5], "probe-pos");
        write_no_dict_row_search_probe(src, src_size, level, checksum, state_pos, probe_pos);
        return 0;
    }

    if (strcmp(argv[1], "trace-raw-dict-extdict-double-fast-probe-configured") == 0) {
        int level;
        int checksum;
        size_t pos;
        if (argc != 5) {
            fprintf(stderr, "usage: helper trace-raw-dict-extdict-double-fast-probe-configured <level> <checksum> <pos>\n");
            return 2;
        }
        level = parse_int_arg(argv[2], "level");
        checksum = parse_bool_arg(argv[3], "checksum");
        pos = (size_t)parse_int_arg(argv[4], "pos");
        write_raw_dict_extdict_double_fast_probe(src, src_size, level, checksum, pos);
        return 0;
    }

    if (strcmp(argv[1], "bench-compress-regular") == 0) {
        size_t iterations;
        int level;
        int checksum;
        if (argc != 5) {
            fprintf(stderr, "usage: helper bench-compress-regular <iters> <level> <checksum>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        level = parse_int_arg(argv[3], "level");
        checksum = parse_bool_arg(argv[4], "checksum");
        bench_compress(iterations, src, src_size, level, checksum, NULL, 0);
        return 0;
    }

    if (strcmp(argv[1], "bench-compress-regular-with-output") == 0) {
        size_t iterations;
        int level;
        int checksum;
        if (argc != 5) {
            fprintf(stderr, "usage: helper bench-compress-regular-with-output <iters> <level> <checksum>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        level = parse_int_arg(argv[3], "level");
        checksum = parse_bool_arg(argv[4], "checksum");
        bench_compress_with_output(iterations, src, src_size, level, checksum, NULL, 0);
        return 0;
    }

    if (strcmp(argv[1], "bench-compress-raw-dict") == 0) {
        size_t iterations;
        int level;
        int checksum;
        if (argc != 5) {
            fprintf(stderr, "usage: helper bench-compress-raw-dict <iters> <level> <checksum>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        level = parse_int_arg(argv[3], "level");
        checksum = parse_bool_arg(argv[4], "checksum");
        bench_compress(
            iterations,
            src,
            src_size,
            level,
            checksum,
            RAW_DICT_BYTES,
            sizeof(RAW_DICT_BYTES) - 1
        );
        return 0;
    }

    if (strcmp(argv[1], "bench-compress-raw-dict-with-output") == 0) {
        size_t iterations;
        int level;
        int checksum;
        if (argc != 5) {
            fprintf(stderr, "usage: helper bench-compress-raw-dict-with-output <iters> <level> <checksum>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        level = parse_int_arg(argv[3], "level");
        checksum = parse_bool_arg(argv[4], "checksum");
        bench_compress_with_output(
            iterations,
            src,
            src_size,
            level,
            checksum,
            RAW_DICT_BYTES,
            sizeof(RAW_DICT_BYTES) - 1
        );
        return 0;
    }

    if (strcmp(argv[1], "bench-compress-trained-dict") == 0) {
        unsigned char* dict;
        size_t dict_size;
        size_t iterations;
        int level;
        int checksum;
        if (argc != 5) {
            fprintf(stderr, "usage: helper bench-compress-trained-dict <iters> <level> <checksum>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        level = parse_int_arg(argv[3], "level");
        checksum = parse_bool_arg(argv[4], "checksum");
        build_trained_dict(&dict, &dict_size);
        bench_compress(iterations, src, src_size, level, checksum, dict, dict_size);
        free(dict);
        return 0;
    }

    if (strcmp(argv[1], "bench-compress-trained-dict-with-output") == 0) {
        unsigned char* dict;
        size_t dict_size;
        size_t iterations;
        int level;
        int checksum;
        if (argc != 5) {
            fprintf(stderr, "usage: helper bench-compress-trained-dict-with-output <iters> <level> <checksum>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        level = parse_int_arg(argv[3], "level");
        checksum = parse_bool_arg(argv[4], "checksum");
        build_trained_dict(&dict, &dict_size);
        bench_compress_with_output(iterations, src, src_size, level, checksum, dict, dict_size);
        free(dict);
        return 0;
    }

    if (strcmp(argv[1], "bench-decompress") == 0) {
        size_t iterations;
        if (argc != 3) {
            fprintf(stderr, "usage: helper bench-decompress <iters>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        bench_decompress(iterations, src, src_size, NULL, 0);
        return 0;
    }

    if (strcmp(argv[1], "bench-decompress-raw-dict") == 0) {
        size_t iterations;
        if (argc != 3) {
            fprintf(stderr, "usage: helper bench-decompress-raw-dict <iters>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        bench_decompress(iterations, src, src_size, RAW_DICT_BYTES, sizeof(RAW_DICT_BYTES) - 1);
        return 0;
    }

    if (strcmp(argv[1], "bench-decompress-trained-dict") == 0) {
        unsigned char* dict;
        size_t dict_size;
        size_t iterations;
        if (argc != 3) {
            fprintf(stderr, "usage: helper bench-decompress-trained-dict <iters>\n");
            return 2;
        }
        iterations = (size_t)parse_int_arg(argv[2], "iters");
        build_trained_dict(&dict, &dict_size);
        bench_decompress(iterations, src, src_size, dict, dict_size);
        free(dict);
        return 0;
    }

    if (strcmp(argv[1], "decompress") == 0) {
        unsigned long long dst_size_ull = ZSTD_findDecompressedSize(src, src_size);
        size_t dst_size;
        void* dst;
        size_t actual_size;
        if (argc != 2) {
            fprintf(stderr, "usage: helper decompress\n");
            return 2;
        }
        if (dst_size_ull == ZSTD_CONTENTSIZE_ERROR || dst_size_ull == ZSTD_CONTENTSIZE_UNKNOWN) {
            fprintf(stderr, "decompressed size unavailable\n");
            return 2;
        }
        dst_size = (size_t)dst_size_ull;
        dst = malloc(dst_size ? dst_size : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 2;
        }
        actual_size = decompress_once(src, src_size, NULL, 0, dst, dst_size);
        write_all_stdout(dst, actual_size);
        free(dst);
        return 0;
    }

    if (strcmp(argv[1], "decompress-raw-dict") == 0) {
        unsigned long long dst_size_ull = ZSTD_findDecompressedSize(src, src_size);
        size_t dst_size;
        void* dst;
        size_t actual_size;
        if (argc != 2) {
            fprintf(stderr, "usage: helper decompress-raw-dict\n");
            return 2;
        }
        if (dst_size_ull == ZSTD_CONTENTSIZE_ERROR || dst_size_ull == ZSTD_CONTENTSIZE_UNKNOWN) {
            fprintf(stderr, "decompressed size unavailable\n");
            return 2;
        }
        dst_size = (size_t)dst_size_ull;
        dst = malloc(dst_size ? dst_size : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 2;
        }
        actual_size = decompress_once(
            src,
            src_size,
            RAW_DICT_BYTES,
            sizeof(RAW_DICT_BYTES) - 1,
            dst,
            dst_size
        );
        write_all_stdout(dst, actual_size);
        free(dst);
        return 0;
    }

    if (strcmp(argv[1], "decompress-trained-dict") == 0) {
        unsigned char* dict;
        size_t dict_size;
        unsigned long long dst_size_ull = ZSTD_findDecompressedSize(src, src_size);
        size_t dst_size;
        void* dst;
        size_t actual_size;
        if (argc != 2) {
            fprintf(stderr, "usage: helper decompress-trained-dict\n");
            return 2;
        }
        if (dst_size_ull == ZSTD_CONTENTSIZE_ERROR || dst_size_ull == ZSTD_CONTENTSIZE_UNKNOWN) {
            fprintf(stderr, "decompressed size unavailable\n");
            return 2;
        }
        dst_size = (size_t)dst_size_ull;
        dst = malloc(dst_size ? dst_size : 1);
        if (dst == NULL) {
            fprintf(stderr, "allocation failed\n");
            return 2;
        }
        build_trained_dict(&dict, &dict_size);
        actual_size = decompress_once(src, src_size, dict, dict_size, dst, dst_size);
        write_all_stdout(dst, actual_size);
        free(dst);
        free(dict);
        return 0;
    }

    fprintf(stderr, "unknown mode: %s\n", argv[1]);
    return 2;
}
"#
}
