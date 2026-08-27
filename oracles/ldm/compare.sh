#!/bin/bash
# Diff this crate's LDM parameter derivation against C's own
# `ZSTD_ldm_adjustParameters`, over a grid that includes a supplied zero for
# every parameter.
#
# The derivation is ordered and interdependent, and reading it is not enough:
# the first transcription of it here was written straight from the C and still
# differed on 3261 of 5184 rows, because C spells "unset" as zero and an
# `Option` spells it as `None`.
#
# Usage: ./oracles/ldm/compare.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

UPSTREAM=${ZSTANDARD_UPSTREAM_DIR:-upstream-zstd}
if [ ! -f "$UPSTREAM/lib/libzstd.a" ]; then
    echo "no static libzstd at $UPSTREAM/lib/libzstd.a; build the pinned checkout first" >&2
    exit 1
fi

OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

echo ">>> C reference (ZSTD_ldm_adjustParameters)"
cc -o "$OUT/ldm_oracle" oracles/ldm/ldm_oracle.c "$UPSTREAM/lib/libzstd.a" \
    -I"$UPSTREAM/lib" -I"$UPSTREAM/lib/common" -I"$UPSTREAM/lib/compress"
"$OUT/ldm_oracle" > "$OUT/c.csv"

echo ">>> Rust (LdmParameters::resolve)"
cargo test --lib window::ldm::print_resolution_grid -- --ignored --nocapture 2>/dev/null \
    | grep -E '^[0-9]+,[0-9]' > "$OUT/rust.csv"

if diff -q "$OUT/c.csv" "$OUT/rust.csv" >/dev/null; then
    echo "IDENTICAL: $(wc -l < "$OUT/c.csv" | tr -d ' ') rows"
else
    echo "DIFFER on $(diff "$OUT/c.csv" "$OUT/rust.csv" | grep -c '^<') rows:"
    diff "$OUT/c.csv" "$OUT/rust.csv" | head -20
    exit 1
fi

# --- Sequences -------------------------------------------------------------
# The parameters above decide the table's shape; these decide what goes in it.
# A planted repeat far past any block, with unrepeating filler between the
# copies, so every reported match is one the corpus put there.
echo
echo ">>> Sequences"
cc -o "$OUT/seq_oracle" oracles/ldm/ldm_seq_oracle.c "$UPSTREAM/lib/libzstd.a" \
    -I"$UPSTREAM/lib" -I"$UPSTREAM/lib/common" -I"$UPSTREAM/lib/compress"

python3 - "$OUT" <<'PY'
import sys
def filler(n, seed):
    out = bytearray(); s = seed | 1
    while len(out) < n:
        s ^= (s << 13) & 0xffffffffffffffff; s ^= s >> 7; s ^= (s << 17) & 0xffffffffffffffff
        out += s.to_bytes(8, 'little')
    return bytes(out[:n])
planted = filler(1 << 16, 0x5EED)
corpus = planted + filler(3 << 20, 0xA11CE) + planted + filler(1 << 18, 0xBEEF) + planted
open(sys.argv[1] + "/corpus.bin", "wb").write(corpus)
PY

# `blockSize` of 0 is one call over the whole corpus; the others call the
# matcher once per block, which is how the encoder drives it. Only the blocked
# runs can see state that wrongly survives a block boundary -- C's
# `leftoverSize` is a local, so its trailing literals are dropped rather than
# folded into the next block's first sequence.
# The benchmark corpora as well as the planted one, because a corpus built to
# make matches easy to find agrees on cases a real one does not. These are the
# same bytes `long_distance_matching_is_byte_identical_to_upstream` encodes.
for name in json-records log-lines binary-structured; do
    ZSTANDARD_LDM_CORPUS_NAME="$name" ZSTANDARD_LDM_CORPUS_OUT="$OUT/$name.bin" \
        ZSTANDARD_LDM_CORPUS_SIZE=$((1 << 20)) \
        cargo test --lib window::ldm::write_benchmark_corpus -- --ignored >/dev/null 2>&1
done

status=0
compared=0
empty=0
# Window logs below the corpus size are what move the matcher's floor through
# the data: at 27 the whole corpus is inside the window and the floor sits at
# zero for every chunk, so any bound measured against it is untested. 16 is
# what caught backward extension being bounded by the start of the buffer
# rather than by the floor -- C bounds it at `base + dictLimit`, which
# `ZSTD_window_enforceMaxDist` raises on every chunk. It differs on
# `json-records` at strategy 8 and nowhere else in this grid.

# One configuration through both matchers. `$5..$8` are the four LDM
# parameters, where zero means "unset" on both sides.
compare_one() {
    local corpus=$1 wlog=$2 strategy=$3 bsize=$4 hlog=$5 mml=$6 bslog=$7 hrlog=$8
    # The leading `dsize` bytes of the corpus stand in for a dictionary: hashed
    # in up front, then searchable but never searched over. Zero is the
    # no-dictionary case every row above this addition runs as.
    local dsize=${9:-0}
    local where="$corpus windowLog=$wlog strategy=$strategy block=$bsize params=[$hlog $mml $bslog $hrlog] dict=$dsize"
    "$OUT/seq_oracle" "$OUT/$corpus.bin" "$wlog" "$strategy" "$bsize" \
        "$hlog" "$mml" "$bslog" "$hrlog" "$dsize" > "$OUT/seq_c.csv"
    # The exit status matters. Without it a Rust side that panicked, failed to
    # build, or never ran writes an empty file, and against a configuration
    # whose matcher legitimately finds nothing that diffs clean -- a run that
    # compared nothing at all reported as identical.
    if ! ZSTANDARD_LDM_CORPUS="$OUT/$corpus.bin" ZSTANDARD_LDM_WINDOW_LOG="$wlog" \
        ZSTANDARD_LDM_STRATEGY="$strategy" ZSTANDARD_LDM_BLOCK_SIZE="$bsize" \
        ZSTANDARD_LDM_HASH_LOG="$hlog" ZSTANDARD_LDM_MIN_MATCH="$mml" \
        ZSTANDARD_LDM_BUCKET_SIZE_LOG="$bslog" ZSTANDARD_LDM_HASH_RATE_LOG="$hrlog" \
        ZSTANDARD_LDM_DICT_SIZE="$dsize" \
        cargo test --lib window::ldm::print_sequences_for_corpus -- --ignored --nocapture \
        > "$OUT/seq_rust.raw" 2>&1
    then
        echo "    $where: the Rust side failed to run"
        tail -20 "$OUT/seq_rust.raw"
        status=1
        return
    fi
    grep -E '^[0-9]+,[0-9]+,[0-9]+,[0-9]+$' "$OUT/seq_rust.raw" > "$OUT/seq_rust.csv" || true
    compared=$((compared + 1))
    # Counted, not failed. Some shapes legitimately find nothing on some corpora
    # -- the bucket-cap shape needs a hashLog under 8, and a 64-entry table
    # finds no long-range match in a megabyte -- so an empty pair is a fact
    # about the configuration. It is only the *silence* that is wrong: without
    # this the summary would call those rows identical alongside rows that
    # actually matched thousands of sequences.
    if [ ! -s "$OUT/seq_c.csv" ]; then
        empty=$((empty + 1))
    fi
    if ! diff -q "$OUT/seq_c.csv" "$OUT/seq_rust.csv" >/dev/null; then
        echo "    $where: DIFFER"
        diff "$OUT/seq_c.csv" "$OUT/seq_rust.csv" | head -10
        status=1
    fi
}

for corpus in corpus json-records log-lines binary-structured; do
    for wlog in 27 20 18 16; do
        for strategy in 1 2 3 4 5 6 7 8 9; do
            for bsize in 0 131072 16384; do
                compare_one "$corpus" "$wlog" "$strategy" "$bsize" 0 0 0 0
            done
        done
    done
done

# The four parameters, on a narrower grid: the sweep above varies the window,
# the strategy and the block size against the *derived* shape, and what is left
# is whether a supplied value derives the others the way C does.
#
# Shapes rather than one parameter at a time, because the derivation is
# interdependent -- `hashRateLog` decides `hashLog`, which caps `bucketSizeLog`
# -- so moving one alone leaves the rest deriving around it and never reaches
# the combinations a caller can ask for. Each line names the branch it reaches.
#
#            hashLog minMatch bucketSizeLog hashRateLog
PARAM_SHAPES=(
    "18 0 0 0"   # a supplied hashLog below the window derives the rate from it
    "0 0 0 9"    # and the other way round; 9 is a rate no strategy derives
                 # (the derivation is 7 - strategy/3, so 4 through 7), which a
                 # rate of 6 was not: it reproduced the default shape exactly on
                 # strategies 3, 4 and 5 and re-ran rows the first sweep covers
    "18 64 0 6"  # nothing left to derive but the bucket size
    "18 4 0 6"   # a rate wider than the minimum match: the split mask degenerates
    "6 0 8 4"    # a supplied bucket size capped by hashLog, outside the unset check
    "18 32 3 6"  # nothing derived at all
)
# 18 is the one window here that a supplied `hashLog` of 18 does not sit below,
# which is the shape that leaves the rate at zero and splits at every position.
for corpus in corpus json-records log-lines binary-structured; do
    for wlog in 20 18; do
        for strategy in 1 2 3 4 5 6 7 8 9; do
            for shape in "${PARAM_SHAPES[@]}"; do
                # shellcheck disable=SC2086
                compare_one "$corpus" "$wlog" "$strategy" 131072 $shape
            done
        done
    done
done

# >>> With a dictionary in front of the frame.
#
# The dictionary is hashed in by `ZSTD_ldm_fillHashTable`, which is a different
# walk from the one generation performs -- it feeds from the first byte with the
# rolling hash in its initial state, where generation primes over the first
# `minMatchLength` bytes and starts after them. So the dictionary's split points
# are not the ones prepending it to the searched range would produce, and this
# is the only thing that says so.
#
# The window logs matter twice over here. At 20 a megabyte corpus plus its
# dictionary still sits inside the window, so the dictionary keeps its credit
# for the whole frame; at 18 the frame outgrows it partway and C invalidates the
# dictionary outright. Both branches of `ZSTD_window_enforceMaxDist` are on this
# grid.
echo ""
echo ">>> Sequences, with a dictionary"
dict_rows=0
dict_engaged=0
for corpus in corpus json-records log-lines binary-structured; do
    for wlog in 20 18; do
        for strategy in 1 5 7; do
            "$OUT/seq_oracle" "$OUT/$corpus.bin" "$wlog" "$strategy" 131072 \
                0 0 0 0 0 > "$OUT/seq_nodict.csv"
            for dsize in 4096 65536 262144; do
                compare_one "$corpus" "$wlog" "$strategy" 131072 0 0 0 0 "$dsize"
                dict_rows=$((dict_rows + 1))
                # A dictionary that changed nothing is a row proving nothing:
                # the matcher would agree with C by never having consulted it.
                if ! diff -q "$OUT/seq_nodict.csv" "$OUT/seq_c.csv" >/dev/null; then
                    dict_engaged=$((dict_engaged + 1))
                fi
            done
        done
    done
done
echo "    $dict_engaged of $dict_rows dictionary rows changed the sequences C produces"
if [ "$dict_engaged" = 0 ]; then
    echo "    every dictionary row reproduced the no-dictionary frame: this sweep proves nothing"
    status=1
fi

if [ "$status" = 0 ]; then
    echo "    identical on all $compared configurations ($empty of them finding no match on either side)"
fi
exit $status
