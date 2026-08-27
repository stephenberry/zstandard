#!/usr/bin/env bash
#
# PGO (Profile-Guided Optimization) benchmark script for zstandard.
#
# Automates: baseline build → instrumented build → training → profile merge
#            → PGO build → interleaved measurement → before/after comparison.
#
# Both directions are trained and both are reported. That is worth stating
# because it was not always so: training ran only `profile_decode`, which left
# the encoder's match finders with no profile at all, so every encode figure
# this script printed came from a build LLVM had laid out on static heuristics.
# A parser whose match arms are taken a few hundred times in half a million
# iterations is exactly the shape that needs measured branch probabilities, so
# the omission mattered most where it was least visible.
#
# Measurement drives `profile_encode` and `profile_decode` rather than
# `benchmark_report`. Those two are the only binaries not gated on
# `internal-trace` (see the comments in Cargo.toml), which makes them the only
# ones that can be built identically in all three builds here. Reporting
# against upstream C is `benchmark_report`'s job; this script answers one
# question, which is whether the profile bought anything.
#
# Requirements:
#   - an upstream zstd checkout (only if a dictionary case is measured)
#   - rustup-managed toolchain with llvm-tools-preview
#
# Usage:
#   ./scripts/pgo_bench.sh [--iters N] [--measure-iters N] [--rounds N]
#
#   --iters N           Training iterations per case (default: 200)
#   --measure-iters N   Measurement iterations per sample (default: 100)
#   --rounds N          Interleaved measurement rounds, best taken (default: 5)

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(cd "$SCRIPT_DIR/.." && pwd)"

# --- Configuration -----------------------------------------------------------

PROFILE_DIR="$PROJECT_DIR/tmp/pgo"

# Decode training cases: decode-gap severity order (no dictionary cases)
DECODE_TRAINING_CASES=(
    "mixed-entropy:3"
    "mixed-entropy:7"
    "json-records:3"
    "wikipedia:7"
    "log-lines:5"
)

# Encode training cases. Levels 3 and 4 are the double-fast parser and 5 and 7
# the lazy ones; they are separate functions, and a profile that covers only one
# strategy leaves the other laid out blind.
ENCODE_TRAINING_CASES=(
    "binary-structured:4"
    "mixed-entropy:3"
    "json-records:3"
    "log-lines:5"
    "wikipedia:7"
)

# Rows to measure, as case:level. Includes rows outside the training set on
# purpose: a profile that only helps what it was trained on has not generalised.
MEASURE_ROWS=(
    "binary-structured:3"
    "binary-structured:4"
    "mixed-entropy:3"
    "mixed-entropy:4"
    "json-records:3"
    "json-records:4"
    "log-lines:3"
    "log-lines:5"
    "wikipedia:4"
    "wikipedia:7"
    "tabular-csv:4"
)

# `RUSTFLAGS` in the environment *replaces* `target.<triple>.rustflags` from
# .cargo/config.toml rather than adding to it. Every build below sets RUSTFLAGS
# to pass `-Cprofile-generate`/`-Cprofile-use`, so whatever the config would
# have contributed has to be repeated here or the instrumented and PGO builds
# quietly lose it and the comparison measures the missing flag instead.
BASE_RUSTFLAGS="-C target-cpu=native"

TRAINING_ITERS=200
MEASURE_ITERS=100
MEASURE_ROUNDS=5

# --- Argument parsing ---------------------------------------------------------

while [[ $# -gt 0 ]]; do
    case "$1" in
        --iters)
            TRAINING_ITERS="$2"
            shift 2
            ;;
        --measure-iters)
            MEASURE_ITERS="$2"
            shift 2
            ;;
        --rounds)
            MEASURE_ROUNDS="$2"
            shift 2
            ;;
        *)
            echo "Unknown argument: $1" >&2
            echo "Usage: $0 [--iters N] [--measure-iters N] [--rounds N]" >&2
            exit 1
            ;;
    esac
done

# --- Helper functions ---------------------------------------------------------

detect_target() {
    local arch
    arch="$(uname -m)"
    local os
    os="$(uname -s)"
    case "$os" in
        Darwin)
            case "$arch" in
                arm64|aarch64) echo "aarch64-apple-darwin" ;;
                x86_64)        echo "x86_64-apple-darwin" ;;
                *)             echo "unsupported arch: $arch" >&2; exit 1 ;;
            esac
            ;;
        Linux)
            case "$arch" in
                aarch64) echo "aarch64-unknown-linux-gnu" ;;
                x86_64)  echo "x86_64-unknown-linux-gnu" ;;
                *)       echo "unsupported arch: $arch" >&2; exit 1 ;;
            esac
            ;;
        *)
            echo "unsupported OS: $os" >&2; exit 1
            ;;
    esac
}

find_llvm_profdata() {
    local toolchain_root
    toolchain_root="$(rustc --print sysroot)"
    local profdata
    profdata="$(find "$toolchain_root" -name llvm-profdata -type f 2>/dev/null | head -1)"
    if [[ -z "$profdata" ]]; then
        echo "ERROR: llvm-profdata not found in toolchain at $toolchain_root" >&2
        echo "Try: rustup component add llvm-tools-preview" >&2
        exit 1
    fi
    echo "$profdata"
}

timestamp() {
    date "+%H:%M:%S"
}

# Build the two profiling binaries with the given extra rustflags and copy them
# into $PROFILE_DIR/$1, so all three builds survive to the measurement step and
# baseline can be interleaved against PGO rather than run before it.
build_into() {
    local dest="$1" extra="$2"
    rm -rf "${PROFILE_DIR:?}/$dest"
    mkdir -p "$PROFILE_DIR/$dest"
    RUSTFLAGS="$BASE_RUSTFLAGS $extra" \
        cargo build --release --target "$TARGET" \
        --bin profile_encode --bin profile_decode 2>&1 | tail -1
    cp "$TARGET_DIR/profile_encode" "$TARGET_DIR/profile_decode" "$PROFILE_DIR/$dest/"
}

# --- Main script --------------------------------------------------------------

echo "=== zstandard PGO Benchmark ==="
echo ""

TARGET="$(detect_target)"
TARGET_DIR="$PROJECT_DIR/target/$TARGET/release"
echo "[$(timestamp)] Host target: $TARGET"

# Same candidate order as src/support/upstream_zstd.rs: an explicit
# ZSTANDARD_UPSTREAM_DIR is the only answer if given, then the gitignored in-crate
# checkout, then a sibling. Only dictionary cases need it, and none are measured
# by default, so a miss is a note rather than a failure.
if [[ -n "${ZSTANDARD_UPSTREAM_DIR:-}" ]]; then
    UPSTREAM_CANDIDATES=("$ZSTANDARD_UPSTREAM_DIR")
else
    UPSTREAM_CANDIDATES=("$PROJECT_DIR/upstream-zstd" "$PROJECT_DIR/../zstd")
fi
UPSTREAM_DIR=""
for candidate in "${UPSTREAM_CANDIDATES[@]}"; do
    if [[ -d "$candidate" ]]; then
        UPSTREAM_DIR="$candidate"
        break
    fi
done
if [[ -z "$UPSTREAM_DIR" ]]; then
    echo "[$(timestamp)] NOTE: no upstream zstd checkout found; dictionary cases would fail."
else
    echo "[$(timestamp)] Upstream checkout: $UPSTREAM_DIR"
fi

echo "[$(timestamp)] Ensuring llvm-tools-preview is installed..."
rustup component add llvm-tools-preview 2>/dev/null || true

LLVM_PROFDATA="$(find_llvm_profdata)"
echo "[$(timestamp)] Using llvm-profdata: $LLVM_PROFDATA"

cd "$PROJECT_DIR"
mkdir -p "$PROFILE_DIR"

# --- Step 1: Baseline build ---------------------------------------------------

echo ""
echo "=== Step 1: Baseline (non-PGO) build ==="
echo "[$(timestamp)] Building baseline binaries..."
build_into baseline ""

# --- Step 2: Instrumented build -----------------------------------------------

echo ""
echo "=== Step 2: Instrumented build ==="
RAW_DIR="$PROFILE_DIR/raw"
rm -rf "$RAW_DIR"
mkdir -p "$RAW_DIR"

echo "[$(timestamp)] Building with profile instrumentation..."
build_into instrumented "-Cprofile-generate=$RAW_DIR"

# One raw profile per binary image (%m) per process (%p). Without %p the two
# training binaries' many runs contend for the same paths, and a merge that
# silently reads fewer files than it should looks exactly like one that worked.
export LLVM_PROFILE_FILE="$RAW_DIR/%m_%p.profraw"

# --- Step 3: Training workloads -----------------------------------------------

echo ""
echo "=== Step 3: Running training workloads ==="
for entry in "${DECODE_TRAINING_CASES[@]}"; do
    echo "[$(timestamp)] Training decode: ${entry} iters=$TRAINING_ITERS"
    "$PROFILE_DIR/instrumented/profile_decode" \
        --case "${entry%%:*}" \
        --level "${entry##*:}" \
        --iters "$TRAINING_ITERS" 2>&1 | sed 's/^/  /'
done
for entry in "${ENCODE_TRAINING_CASES[@]}"; do
    echo "[$(timestamp)] Training encode: ${entry} iters=$TRAINING_ITERS"
    "$PROFILE_DIR/instrumented/profile_encode" \
        --case "${entry%%:*}" \
        --level "${entry##*:}" \
        --iters "$TRAINING_ITERS" 2>&1 | sed 's/^/  /'
done
unset LLVM_PROFILE_FILE

# --- Step 4: Merge profiles ---------------------------------------------------

echo ""
echo "=== Step 4: Merging profiles ==="
PROF_COUNT=$(find "$RAW_DIR" -name '*.profraw' -type f 2>/dev/null | wc -l | tr -d ' ')
echo "[$(timestamp)] Found $PROF_COUNT profile files"
if [[ "$PROF_COUNT" -eq 0 ]]; then
    echo "ERROR: training produced no .profraw files; nothing to optimize against." >&2
    exit 1
fi

MERGED="$PROFILE_DIR/merged.profdata"
"$LLVM_PROFDATA" merge -o "$MERGED" "$RAW_DIR"/*.profraw
echo "[$(timestamp)] Merged to $MERGED"

# --- Step 5: PGO build --------------------------------------------------------

echo ""
echo "=== Step 5: PGO-optimized build ==="
echo "[$(timestamp)] Building with profile data..."
build_into pgo "-Cprofile-use=$MERGED"

# --- Step 6: Interleaved measurement ------------------------------------------

echo ""
echo "=== Step 6: Measurement ==="
echo "[$(timestamp)] $MEASURE_ROUNDS rounds x $MEASURE_ITERS iters, baseline and PGO interleaved"

SAMPLES="$PROFILE_DIR/samples.txt"
: > "$SAMPLES"

# Interleaved rather than all-baseline-then-all-PGO: thermal drift over a run
# this long is larger than the effect being measured, and interleaving is what
# keeps it from landing entirely on one of the two builds.
for round in $(seq 1 "$MEASURE_ROUNDS"); do
    printf "[%s] round %d/%d" "$(timestamp)" "$round" "$MEASURE_ROUNDS"
    for row in "${MEASURE_ROWS[@]}"; do
        case_name="${row%%:*}"
        level="${row##*:}"
        for build in baseline pgo; do
            enc=$("$PROFILE_DIR/$build/profile_encode" \
                --case "$case_name" --level "$level" --iters "$MEASURE_ITERS" \
                | sed -n 's/.*throughput=\([0-9.]*\).*/\1/p')
            dec=$("$PROFILE_DIR/$build/profile_decode" \
                --case "$case_name" --level "$level" --iters "$MEASURE_ITERS" \
                | sed -n 's/.*mb_per_s=\([0-9.]*\).*/\1/p')
            echo "$build $case_name $level encode $enc" >> "$SAMPLES"
            echo "$build $case_name $level decode $dec" >> "$SAMPLES"
        done
    done
    printf " done\n"
done

# --- Step 7: Comparison table -------------------------------------------------

echo ""
echo "=== Results: Baseline vs PGO (best of $MEASURE_ROUNDS, MiB/s) ==="

# `trained` marks rows the profile actually saw. Trained and untrained rows
# answer different questions and should not be averaged together by eye.
TRAINED="$PROFILE_DIR/trained.txt"
: > "$TRAINED"
for entry in "${ENCODE_TRAINING_CASES[@]}"; do echo "encode ${entry%%:*} ${entry##*:}" >> "$TRAINED"; done
for entry in "${DECODE_TRAINING_CASES[@]}"; do echo "decode ${entry%%:*} ${entry##*:}" >> "$TRAINED"; done

for dir in encode decode; do
    echo ""
    echo "--- $dir ---"
    awk -v want="$dir" -v trained_file="$TRAINED" '
        BEGIN {
            while ((getline line < trained_file) > 0) {
                split(line, f, " ")
                trained[f[1] "/" f[2] "/" f[3]] = 1
            }
            printf "%-20s %5s %12s %12s %9s\n", "Case", "Level", "Baseline", "PGO", "Delta"
            printf "%-20s %5s %12s %12s %9s\n", "----", "-----", "--------", "---", "-----"
        }
        $4 == want {
            key = $2 "/" $3
            if (!(($1 "/" key) in best) || $5 + 0 > best[$1 "/" key]) best[$1 "/" key] = $5 + 0
            if (!(key in seen)) { seen[key] = 1; order[++n] = key }
        }
        END {
            logsum = 0; rows = 0
            for (i = 1; i <= n; i++) {
                key = order[i]
                b = best["baseline/" key]; p = best["pgo/" key]
                split(key, k, "/")
                mark = ((want "/" key) in trained) ? " *" : ""
                if (b > 0 && p > 0) {
                    d = (p / b - 1) * 100
                    logsum += log(p / b); rows++
                    printf "%-20s %5s %12.1f %12.1f %+8.1f%%%s\n", k[1], k[2], b, p, d, mark
                } else {
                    printf "%-20s %5s %12s %12s %9s%s\n", k[1], k[2], "n/a", "n/a", "n/a", mark
                }
            }
            if (rows > 0)
                printf "\n%-20s %5s %12s %12s %+8.1f%%\n", "geometric mean", "", "", "", (exp(logsum / rows) - 1) * 100
        }
    ' "$SAMPLES"
done

echo ""
echo "  * = case:level was in that direction's training set"
echo ""
echo "[$(timestamp)] Done. Profile data in $PROFILE_DIR/"
echo ""
echo "To rebuild with PGO manually:"
echo "  RUSTFLAGS=\"$BASE_RUSTFLAGS -Cprofile-use=$MERGED\" cargo build --release --target $TARGET"
