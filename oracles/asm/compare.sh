#!/bin/bash
# Compare Rust vs C assembly for each hot-loop piece.
# Usage: ./oracles/asm/compare.sh
set -euo pipefail

cd "$(dirname "$0")/../.."

ARCH=$(uname -m)
echo "=== Architecture: $ARCH ==="
echo

# --- Rust assembly ---
echo ">>> Compiling Rust assembly..."
cargo rustc --release --lib --features asm-inspect -- --emit asm 2>&1 | tail -3
RUST_ASM=$(ls target/release/deps/zstandard-*.s | head -1)
echo "    Rust asm: $RUST_ASM"
echo

# --- C assembly ---
echo ">>> Compiling C assembly..."
if [ "$ARCH" = "arm64" ] || [ "$ARCH" = "aarch64" ]; then
    TARGET="-target aarch64-apple-darwin"
else
    TARGET="-target x86_64-apple-darwin"
fi

clang -O2 $TARGET -S oracles/asm/c_pieces.c \
    -o oracles/asm/c_pieces.s 2>&1
echo "    C asm: oracles/asm/c_pieces.s"
echo

# --- Count instructions for a function ---
# Extracts from function label to `ret` instruction, counting only real instructions
# (not directives, comments, or local labels)
count_insns() {
    local file=$1
    local func=$2
    local pat1="^${func}:"
    local pat2="^_${func}:"
    awk -v pat1="$pat1" -v pat2="$pat2" '
        BEGIN { found=0; count=0 }
        $0 ~ pat1 || $0 ~ pat2 { found=1; next }
        # Stop at the NEXT global/external function label (not local labels like LBB*)
        found && /^_[a-zA-Z]/ { found=0 }
        found && /^[a-zA-Z]/ && !/^L/ && !/^Lfunc/ && !/^Lexception/ && !/^Lloh/ { found=0 }
        # Skip directives, comments, local labels
        found && /^\t\./ { next }
        found && /^;/ { next }
        found && /^\/\// { next }
        found && /^L/ { next }
        found && /^Lfunc/ { next }
        found && /^Lexception/ { next }
        found && /^Lloh/ { next }
        # Count actual instructions (indented with tab or spaces, starts with letter)
        found && /^\t[a-z]/ { count++ }
        found && /^  +[a-z]/ { count++ }
        END { print count }
    ' "$file"
}

# --- Extract assembly for a function ---
extract_func() {
    local file=$1
    local func=$2
    local pat1="^${func}:"
    local pat2="^_${func}:"
    awk -v pat1="$pat1" -v pat2="$pat2" '
        $0 ~ pat1 || $0 ~ pat2 { found=1 }
        found && /^_[a-zA-Z]/ && !($0 ~ pat1) && !($0 ~ pat2) { found=0 }
        found && /^[a-zA-Z]/ && !/^L/ && !/^Lfunc/ && !/^Lexception/ && !/^Lloh/ && !($0 ~ pat1) && !($0 ~ pat2) { found=0 }
        found { print }
    ' "$file" | head -100
}

echo "============================================================"
echo "INSTRUCTION COUNT COMPARISON: Rust vs C (aarch64)"
echo "============================================================"
printf "%-45s %6s %6s %6s\n" "Function" "Rust" "C" "Delta"
echo "-------------------------------------------------------------"

PAIRS=(
    "asm_read_bits_fast_zero_safe:c_read_bits:read_bits (zero-safe vs getMiddleBits)"
    "asm_read_bits_fast:c_read_bits_fast:read_bits_fast (both require nbBits>=1)"
    "asm_look_bits_fast:c_look_bits_fast:look_bits_fast (no skip)"
    "asm_fse_update_state:c_fse_update_state:FSE state update"
    "asm_reload:c_reload:reload (full)"
    "asm_reload_fast:c_reload_fast:reload_fast"
    "asm_resolve_offset:c_resolve_offset:offset resolution"
    "asm_copy_16:c_copy_16:copy_16 (SIMD)"
    "asm_copy_literals:c_copy_literals:literal copy (with guard)"
    "asm_copy_literals_no_guard:c_copy_literals:literal copy (no guard vs C)"
    "asm_copy_match_large_offset:c_copy_match_large_offset:match copy (large offset)"
    "asm_copy_match_small_offset:c_copy_match_small_offset:match copy (small offset)"
    "asm_bounds_check:c_bounds_check:bounds check"
    "asm_window_check:c_window_check:window check"
    "asm_full_guard:c_bounds_check:full guard (vs C bounds only)"
    "asm_prefetch_pair:c_prefetch_pair:prefetch pair"
    "asm_full_decode_step:c_full_decode_step:FULL decode step"
)

for pair in "${PAIRS[@]}"; do
    IFS=':' read -r rust_func c_func desc <<< "$pair"
    rust_count=$(count_insns "$RUST_ASM" "$rust_func")
    c_count=$(count_insns "oracles/asm/c_pieces.s" "$c_func")
    delta=$((rust_count - c_count))
    sign=""
    [ "$delta" -gt 0 ] && sign="+"
    printf "%-45s %6d %6d %s%d\n" "$desc" "$rust_count" "$c_count" "$sign" "$delta"
done

echo "============================================================"
echo
echo ">>> Detailed assembly for key functions:"
echo

for func in asm_read_bits_fast_zero_safe asm_read_bits_fast asm_fse_update_state asm_resolve_offset asm_reload asm_reload_fast; do
    echo "--- Rust: $func ---"
    extract_func "$RUST_ASM" "$func"
    echo
done

for func in c_read_bits c_read_bits_fast c_fse_update_state c_resolve_offset c_reload c_reload_fast; do
    echo "--- C: $func ---"
    extract_func "oracles/asm/c_pieces.s" "$func"
    echo
done
