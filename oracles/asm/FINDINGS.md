# Assembly Comparison Findings: Rust vs C zstd Decode Hot Loop

**Architecture:** aarch64 (Apple Silicon)
**Compiler:** rustc (LLVM) release + clang -O2

## Instruction Count Summary

| Piece | Rust | C | Delta | Calls/iter | Total/iter |
|-------|------|---|-------|------------|------------|
| `read_bits_fast_zero_safe` vs `BIT_readBits` (getMiddleBits) | 11 | 11 | **0** | x6 | **0** |
| `read_bits_fast` vs `BIT_readBitsFast` | 9 | 9 | **0** | -- | -- |
| FSE state update | 15 | 13 | **+2** | x3 | **+6** |
| reload (full) | 59 | 35 | **+24** | x1 | **+24** |
| reload_fast | 36 | 16 | **+20** | x1 | **+20** |
| offset resolution | 31 | 42 | **-11** | x1 | **-11** |
| copy_16 (SIMD) | 3 | 3 | **0** | -- | -- |
| literal copy (with guard) | 39 | 14 | **+25** | x1 | **+25** |
| match copy (large offset) | 26 | 10 | **+16** | x1 | **+16** |
| match copy (small offset) | 38 | 39 | **-1** | x1 | **-1** |
| bounds check | 7 | 7 | **0** | x1 | **0** |
| window check | 6 | 4 | **+2** | x1 | **+2** |
| full guard (vs C bounds only) | 16 | 7 | **+9** | x1 | **+9** |
| prefetch pair | 5 | 4 | **+1** | x1 | **+1** |
| **FULL decode step** (composite) | **121** | **96** | **+25** | x1 | **+25** |

## Key Findings

### 1. Zero-Safe Mask is FREE (0 extra insns)

**Surprise:** `read_bits_fast_zero_safe` (11 insns) matches C's `BIT_readBits` via `getMiddleBits` (11 insns).

Rust's mask: `mov x10, #-1; lsl x10, x10, x1; bic x0, x8, x10` (3 insns)
C's table:   `adrp x10, BIT_mask@PAGE; add x10, ...; ldr w10, [x10, w1, uxtw #2]; and x0, x8, x10` (3-4 insns)

The zero-safe mask using `(1 << nb_bits) - 1` compiles to the same cost as C's table lookup on aarch64. **No change needed.**

### 2. Reload is the Biggest Per-Piece Overhead (+44 insns/iter)

| | Rust | C | Cause |
|---|---|---|---|
| reload_fast | 36 | 16 | **+20** -- bounds checking in `read_usize()` |
| reload (full) | 59 | 35 | **+24** -- same + panic/unwind paths |

Rust's `read_usize()` uses `first_chunk::<8>().unwrap()` which generates:
- Bounds check (`subs x9, x1, x8; b.lo panic`)
- Size check (`cmp x9, #8; b.lo panic`)
- Two panic cold paths with `slice_index_fail` and `unwrap_failed` calls
- Exception handling personality + LSDA tables
- Stack frame setup for unwinding (`stp x29, x30, [sp, #-16]!`)

C's `MEM_readLEST(bitD->ptr)` is just `ldr x9, [x10]` -- a single instruction.

### 3. Wildcopy Loop Structure (+41 insns/iter)

| | Rust | C | Cause |
|---|---|---|---|
| literal copy | 39 | 14 | **+25** -- 32+16+exact tail structure vs simple 16-byte loop |
| match copy (large) | 26 | 10 | **+16** -- same |

Rust's wildcopy does: 32-byte bulk -> 16-byte chunk -> exact tail copy.
C's `ZSTD_wildcopy` does: simple 16-byte loop.

### 4. FSE State Update (+6 insns/iter)

Rust: 15 insns, C: 13 insns (+2 per call x 3 calls = +6/iter).

The overhead comes from the zero-safe mask in `read_bits_fast_zero_safe` vs C's `getMiddleBits` with table lookup. While standalone versions are equal (11 vs 11), when inlined into FSE update the Rust version is slightly larger because LLVM can't fold the mask computation as efficiently when it sees the surrounding code.

### 5. Guard Checks (+9 insns for full guard vs C bounds-only)

Rust's hot path has TWO guard regions:
1. Bounds check (literal_end + seq_end) -- 7 insns, matches C
2. History window check (offset vs produced_in_frame.min(window_size)) -- 6 insns (C: 4)

C folds the window check into `ZSTD_execSequence` differently: `offset > (oLitEnd - prefixStart)` is a single comparison. Rust computes `produced_in_frame.min(window_size)` which adds an extra `cmp + csel`.

### 6. Offset Resolution is Actually SMALLER (-11 insns)

Rust: 31, C: 42. Rust's offset resolution is more compact because:
- C's version includes bitstream reads (BIT_readBitsFast) inline
- Rust reads extra bits separately, before offset resolution
- LLVM's branch optimization for the if/else chain is effective

## Overhead Budget

| Source | Extra insns/iter | % of measured gap |
|--------|-----------------|-------------------|
| Reload bounds checking | +44 | **48%** |
| Wildcopy loop structure | +41 | **45%** |
| FSE update (zero-safe mask) | +6 | 7% |
| Window check min() | +2 | 2% |
| Prefetch | +1 | 1% |
| Offset resolution (Rust smaller) | -11 | -12% |
| Match copy small offset | -1 | -1% |
| **Total measured per-piece** | **+82** | -- |

The total per-piece overhead (+82) accounts for only a fraction of the full hot-loop gap (~876 insns). The remaining gap comes from register spills (~129 stores + ~488 reloads from inlining everything into one monolithic function), macro duplication, and panic/unwind infrastructure.

## Optimization Experiments

We tested several approaches to close the gap. **None improved throughput** -- each caused ~12-13% regression on trained-dict-l3 benchmarks. The findings below explain why.

### Experiment: Unsafe reload reads (`read_usize_unchecked`)

Replaced bounds-checked `read_usize()` with `unsafe { read_usize_unchecked() }` in `reload()` and `reload_fast()`.

- **Assembly result:** reload_fast went from 36 to 16 insns (matches C exactly). reload went from 59 to 41. Full decode step composite went from 121 to 95 (1 less than C's 96).
- **Benchmark result:** -12% throughput on trained-dict-l3 (1,911 vs 2,164 MiB/s).
- **Why it hurts:** Total instruction count in the monolithic decode function was unchanged (3,129 in both). Removing the bounds-check cold paths shifted code layout, causing worse instruction-cache alignment for the hot loop. The out-of-order engine already hid the bounds-check latency since the cold paths are never taken.

### Experiment: Simplified wildcopy (16-byte loop)

Replaced the 3-tier wildcopy (32-byte + 16-byte + exact tail) with C's simple 16-byte loop.

- **Assembly result:** Fewer instructions per copy function.
- **Benchmark result:** -5% to -13% throughput.
- **Why it hurts:** The 32-byte unrolled loop processes data faster per iteration despite more setup instructions. On typical data, most literals and matches are 16-48 bytes, so the unrolled inner loop runs 1-2 times and the 2x bandwidth wins.

### Experiment: Outlined `execute_sequence_copy` (`#[inline(never)]`)

Extracted prefetch + literal copy + match copy into an `#[inline(never)]` function to create a register-allocation boundary.

- **Assembly result:** Clean separation -- 7 params all in registers, no error paths.
- **Benchmark result:** -13% throughput.
- **Why it hurts:** Each sequence processes ~30 bytes of literals + ~30 bytes of match on average. At ~4,500 sequences/block, even ~5 ns of call overhead adds ~22 us/block -- a significant fraction of the ~80 us total decode time. The monolithic approach wins because L1-cache stack spills are cheaper than function call save/restore.

### Experiment: Outlined `copy_match_inline` only

Marked just the largest single piece (`copy_match_inline`, ~80 insns) as `#[inline(never)]`.

- **Benchmark result:** -13% throughput. Same issue as above.

### Experiment: `#[inline(always)]` extracted function (structural reorder only)

Extracted execute body into a function with `#[inline(always)]` -- purely a structural change with no call overhead.

- **Benchmark result:** -13% throughput.
- **Why it hurts:** Moving the window check before the copies and combining `out_pos` updates changed the data-dependency chain. LLVM's scheduling decisions for the original instruction ordering were better.

### Key Takeaway

The monolithic inlined decode loop is optimal for the current hot-loop structure on aarch64. Register spills go to L1-cache stack slots and are hidden by the out-of-order engine. Code layout alignment effects dominate -- any structural change that shifts the binary layout by even a few bytes can swing throughput by 10-15%.

Future optimization should focus on:
- **Profile-guided optimization (PGO)** to let LLVM make layout decisions based on actual branch frequencies
- **Compiler-level improvements** (e.g. LLVM hints for register allocation boundaries without function call overhead)
- **Algorithmic changes** that reduce total work per sequence rather than restructuring the existing work
