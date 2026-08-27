#!/bin/bash
# Diff this crate's *applied* compression parameters against C's own, over a
# grid of (level, dictionary size, source size hint).
#
# Reading the derivation is not enough, and neither is checking one row. The
# resolution runs in four ordered stages and threads a `ZSTD_CParamMode_e`
# through them that changes what `dictSize` means; `ZSTD_cpm_attachDict` zeroes
# it in two separate places. Before that mode was modelled, 175 of these 576
# rows disagreed -- every one of them a row where the dictionary attaches -- and
# the visible symptom was a streamed frame declaring a 128 KiB window at every
# level, worth 3.4x-4.4x against upstream on a 2 MiB body.
#
# Each row carries two parameter sets, because C keeps two and reads a
# different one at each stage. `appliedParams.cParams` sizes the source match
# state; the `dms_*` columns are `cdict->matchState.cParams`, which attaching
# points `dictMatchState` at and never rewrites, and which size the
# dictionary's own tables and bound every search over them. They are not
# interchangeable: applied is the CDict's parameters re-fitted to the source,
# so a 16 KiB dictionary against a 256-byte source resolves chain_log 16 for
# the dictionary and 9 for applied. Sizing the dictionary's tree from applied
# left 255 positions of that dictionary reachable and the rest not, which cost
# 3.5x at levels 11 and 12.
#
# The 144 no-dictionary rows are the control: they must always agree, because
# nothing in the dictionary path should be able to move them. Their `dms_*`
# columns are zero on both sides, which is also the check that the two agree on
# *when* a dictionary attaches rather than only on what it resolves to.
#
# The grid drives C through `ZSTD_CCtx_loadDictionary`, which is the shape this
# crate's dictionary API takes. It also records the `ZSTD_createCDict` +
# `ZSTD_CCtx_refCDict` rows, which resolve *differently* -- see the header of
# `cparams_oracle.c`. Diffing ours against those invents a defect that is not
# there; an earlier session lost an hour to exactly that.
#
# Usage: ./oracles/cparams/compare.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

UPSTREAM=${ZSTANDARD_UPSTREAM_DIR:-upstream-zstd}
if [ ! -f "$UPSTREAM/lib/libzstd.a" ]; then
    echo "no static libzstd at $UPSTREAM/lib/libzstd.a; build the pinned checkout first" >&2
    exit 1
fi

OUT=$(mktemp -d)
trap 'rm -rf "$OUT"' EXIT

echo ">>> C reference (ZSTD_CCtx_loadDictionary, appliedParams.cParams)"
cc -O2 -o "$OUT/cparams_oracle" oracles/cparams/cparams_oracle.c "$UPSTREAM/lib/libzstd.a" \
    -I"$UPSTREAM/lib" -I"$UPSTREAM/lib/common" -I"$UPSTREAM/lib/compress"
"$OUT/cparams_oracle" > "$OUT/all.csv"
grep '^loadDict,' "$OUT/all.csv" | sed 's/^loadDict,//' > "$OUT/c.csv"

echo ">>> Rust (compression_parameters_with_overrides)"
cargo test --release --lib encode::tests::print_cparams_grid -- --ignored --nocapture 2>/dev/null \
    | grep -E '^-?[0-9]+,' > "$OUT/rust.csv"

C_ROWS=$(wc -l < "$OUT/c.csv" | tr -d ' ')
R_ROWS=$(wc -l < "$OUT/rust.csv" | tr -d ' ')
if [ "$C_ROWS" != "$R_ROWS" ]; then
    echo "GRID MISMATCH: C produced $C_ROWS rows, Rust $R_ROWS — the two sweeps are not the same grid" >&2
    exit 1
fi

# Anti-vacuity: a grid whose dictionary rows never differ from the
# no-dictionary ones would agree trivially.
VARIED=$(awk -F, '$2 > 0' "$OUT/c.csv" | wc -l | tr -d ' ')
if [ "$VARIED" -eq 0 ]; then
    echo "GRID IS INERT: no dictionary rows" >&2
    exit 1
fi

if diff -q "$OUT/c.csv" "$OUT/rust.csv" >/dev/null; then
    echo "IDENTICAL: $C_ROWS rows ($VARIED of them carrying a dictionary)"
else
    echo "DIFFER on $(diff "$OUT/c.csv" "$OUT/rust.csv" | grep -c '^<') of $C_ROWS rows:"
    diff "$OUT/c.csv" "$OUT/rust.csv" | head -30
    exit 1
fi
