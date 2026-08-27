# Decode Loop Register Pressure: Rust vs C zstd

## Summary

This crate's decode hot loop runs at **0.90–0.92x C zstd throughput** on aarch64 (Apple Silicon). The gap is register allocation, not algorithm: the same loop body compiles to **251 instructions / 35 stack ops** here against C's **178 / 25**, from the same LLVM backend. Five restructurings were tried and every one of them made throughput worse, because each perturbed the allocation into spilling the bitstream state. What follows records the measurements, the shape of the constraint, and which leads remain untested.

## The Problem

The inner decode loop has ~30 live variables that must coexist across one iteration. aarch64 has 28 usable GP registers. LLVM spills 6 FSE table entry fields to the stack and generates ~16 extra register-shuffle `mov` instructions compared to C (compiled with Clang, same LLVM backend).

Both C and Rust use `#[inline(always)]` / `FORCE_INLINE` — there is no function call boundary in either version. The difference is purely in how LLVM's register allocator handles the inlined code.

### Measured Loop Metrics (aarch64, Apple M-series)

|                    | C (Clang) | Rust (LLVM) | Delta |
|--------------------|-----------|-------------|-------|
| Loop instructions  | 178       | 251         | +73   |
| Stack ops          | 25        | 35          | +10   |
| Non-stack extra    | —         | —           | +63   |

The +63 non-stack instructions break down as: ~9 bounds-check instructions, ~16 register shuffles (`mov`), ~20 cold-path setup laid inline, ~18 copy-path branch differences.

## Architecture

The loop decodes and executes zstd sequences. Each iteration:

1. **Decode**: Read variable-length extra bits from a backward bitstream (3 reads + conditional reload)
2. **FSE state update**: Read combined state bits, compute 3 new FSE states, load 3 new table entries
3. **Execute**: Copy literals (memcpy-like), copy match (overlap-aware memcpy), update offsets
4. **Reload**: Refill the bitstream register for the next iteration

### Critical Constraint

The bitstream state (`bit_container: usize`, `bits_consumed: u32`) is accessed 6+ times per iteration during decode + FSE update. LLVM currently keeps these in dedicated registers (`x26`, `w28`). **Any code change that causes LLVM to spill these two values to the stack results in a 10–20% throughput regression**, because every bit read then requires a load-use-store cycle instead of pure register ops.

## Code

### BitDStream (backward bitstream reader)

```rust
pub(crate) struct BitDStream<'a> {
    pub(crate) bit_container: usize,  // 64-bit shift register
    pub(crate) bits_consumed: u32,     // how many bits have been read
    ptr: *const u8,                    // current position (moves backward)
    start: *const u8,                  // beginning of bitstream
    _phantom: PhantomData<&'a [u8]>,
}

impl BitDStream<'_> {
    #[inline(always)]
    pub(crate) fn look_bits_fast(&self, nb_bits: u32) -> usize {
        debug_assert!(nb_bits >= 1);
        let reg_mask = (size_of::<usize>() * 8 - 1) as u32;
        (self.bit_container << (self.bits_consumed & reg_mask))
            >> (((reg_mask + 1) - nb_bits) & reg_mask)
    }

    #[inline(always)]
    pub(crate) fn skip_bits(&mut self, nb_bits: u32) {
        self.bits_consumed += nb_bits;
    }

    #[inline(always)]
    pub(crate) fn read_bits_fast(&mut self, nb_bits: u32) -> usize {
        let value = self.look_bits_fast(nb_bits);
        self.skip_bits(nb_bits);
        value
    }

    #[inline(always)]
    pub(crate) fn reload_fast(&mut self) -> BitDStreamStatus {
        // ... subtract consumed bytes from ptr, reload 8-byte container ...
    }
}
```

### FSE Table Entry (packed u64)

```rust
// Layout: [new_state:16][nb_bits:8][nb_additional_bits:8][baseline:32]
pub(crate) const fn raw_entry_nb_additional_bits(raw: u64) -> u32 {
    ((raw >> 24) & 0xFF) as u32
}
pub(crate) const fn raw_entry_nb_bits(raw: u64) -> u32 {
    ((raw >> 16) & 0xFF) as u32
}
pub(crate) const fn raw_entry_new_state(raw: u64) -> usize {
    (raw & 0xFFFF) as usize
}
pub(crate) const fn raw_entry_baseline(raw: u64) -> u32 {
    (raw >> 32) as u32
}
```

### The Hot Loop

```rust
// Pre-read FSE entries as raw u64 for the first sequence.
let mut lit_raw = unsafe { tables.seq_ll.get_entry_raw(literal_state.state) };
let mut off_raw = unsafe { tables.seq_of.get_entry_raw(offset_state.state) };
let mut ml_raw = unsafe { tables.seq_ml.get_entry_raw(match_state.state) };

let mut remaining_sequences = section.number_of_sequences;
while remaining_sequences > 1 {
    remaining_sequences -= 1;

    // ── DECODE: extract extra bits from bitstream ──
    let offset_code = fse::raw_entry_nb_additional_bits(off_raw);
    let match_extra_bits = fse::raw_entry_nb_additional_bits(ml_raw);
    let literal_extra_bits = fse::raw_entry_nb_additional_bits(lit_raw);

    let offset_extra = if offset_code > 0 {
        reader.read_bits_fast(offset_code) as u32
    } else { 0 };

    let match_extra = if match_extra_bits > 0 {
        reader.read_bits_fast(match_extra_bits) as u32
    } else { 0 };

    if offset_code + match_extra_bits + literal_extra_bits >= 31 {
        let _ = reader.reload();
    }

    let literal_extra = if literal_extra_bits > 0 {
        reader.read_bits_fast(literal_extra_bits) as u32
    } else { 0 };

    let literal_length = (fse::raw_entry_baseline(lit_raw) + literal_extra) as usize;
    let match_length = (fse::raw_entry_baseline(ml_raw) + match_extra) as usize;
    let offset_value = (1u32 << offset_code) + offset_extra;

    // ── FSE STATE UPDATE: read state bits, compute new states ──
    let ll_bits = fse::raw_entry_nb_bits(lit_raw);
    let ml_bits = fse::raw_entry_nb_bits(ml_raw);
    let of_bits = fse::raw_entry_nb_bits(off_raw);
    let combined_state_bits = ll_bits + ml_bits + of_bits;
    let state_val = if combined_state_bits > 0 {
        reader.read_bits_fast(combined_state_bits)
    } else { 0 };

    literal_state.state = fse::raw_entry_new_state(lit_raw)
        + ((state_val >> (ml_bits + of_bits))
           & ((1usize << ll_bits).wrapping_sub(1)));
    match_state.state = fse::raw_entry_new_state(ml_raw)
        + ((state_val >> of_bits)
           & ((1usize << ml_bits).wrapping_sub(1)));
    offset_state.state = fse::raw_entry_new_state(off_raw)
        + (state_val & ((1usize << of_bits).wrapping_sub(1)));

    // ── TABLE LOOKUPS for next iteration ──
    lit_raw = unsafe { tables.seq_ll.get_entry_raw(literal_state.state) };
    off_raw = unsafe { tables.seq_of.get_entry_raw(offset_state.state) };
    ml_raw = unsafe { tables.seq_ml.get_entry_raw(match_state.state) };

    // ── EXECUTE: resolve offset, copy literals, copy match ──
    let total = literal_length + match_length;
    let new_ip = unsafe { ip.add(literal_length) };
    if total > remaining_budget || new_ip > ip_end {
        return Err(corruption_error("..."));
    }
    remaining_budget -= total;

    let offset = if offset_value > 3 {
        let o = offset_value - 3;
        rep2 = rep1; rep1 = rep0; rep0 = o;
        o as usize
    } else if offset_value == 1 && literal_length != 0 {
        rep0 as usize
    } else {
        resolve_rep_offset_decode(...)  // #[cold] #[inline(never)]
    };

    unsafe {
        // Literal copy (16-byte chunks)
        copy_16(out_base.add(out_pos), ip);
        if literal_length > 16 { /* loop */ }
    }
    out_pos += literal_length;
    ip = new_ip;

    if offset <= out_pos - frame_start && offset <= window_size {
        unsafe { copy_match_inline(out_base, out_pos, out_pos - offset, match_length); }
        out_pos += match_length;
    } else {
        execute_dictionary_match(...)?;  // #[cold] #[inline(never)]
    }

    // ── RELOAD bitstream ──
    if reader.reload() == BitDStreamStatus::Overflow {
        return Err(corruption_error("sequence bitstream overflow"));
    }
}
```

## Live Variables at Peak Pressure

During the FSE state update section, these values are simultaneously live:

**In registers (14 values):**
- `bit_container`, `bits_consumed` — bitstream (accessed 6x/iter)
- `ptr` — bitstream pointer (for reload)
- `lit_raw`, `off_raw`, `ml_raw` nb_additional_bits — 3 values (pre-read, used in decode)
- `lit_raw` baseline, `off_raw` packed low bits — 2 values (pre-read, used in decode)
- `literal_length`, `match_length`, `offset_value` — 3 values (produced by decode, consumed by execute)
- `remaining_sequences` — loop counter
- `remaining_budget` — bounds check
- constant `-1` — used for mask generation via `bic`

**Spilled to stack (6 values, loaded once during FSE update):**
- `nb_bits` for lit, ml, off entries — 3 values
- `new_state` for lit, ml, off entries — 3 values

**Also on stack (invariant across iterations):**
- 3 table base pointers (loaded once per iteration during table lookup)
- `start` pointer (for reload)
- `frame_start`, `window_size` (for bounds checks)
- `out_base`, `ip_end`, repeat_offsets pointer

## What Was Tried

### 1. Move table lookups into decode (eliminate pre-read)
Load entries at the start of each iteration instead of pre-reading at the end. **Result:** LLVM displaced `bit_container`/`bits_consumed` to the stack (+13 stack accesses). Bitstream on stack = 10–20% regression.

### 2. Reorder execute before FSE update
Execute immediately after decode so `literal_length`/`match_length`/`offset_value` die before FSE update. **Result:** Same — LLVM spilled bitstream state. Any disruption to the contiguous decode→FSE section causes bitstream spills.

### 3. Split FSE update: read bits early, compute states after execute
Read the combined state bits right after decode (keeping bitstream hot), then compute new states after execute. **Result:** LLVM still spilled bitstream. The `state_val` + `ll_bits`/`ml_bits`/`of_bits` values bridging across execute extended live ranges enough to cause spills.

### 4. Outlined function with `#[inline(never)]` + state struct
Mimic C's `seqState_t` pattern: pack all decode state into a struct, pass `&mut` to an outlined function. **Result:** ~15–20% regression. The function call boundary requires callee-save/restore (8–12 memory ops) plus 13 struct I/O ops at entry/exit. Total overhead ≈ baseline's spill count, but serialized at function boundaries instead of distributed across the iteration.

### 5. Same outlined function, returning void (values via struct)
Eliminated sret overhead by writing return values to struct fields. **Result:** No improvement over #4 — the bottleneck is callee-save overhead, not return convention.

## The Core Constraint

LLVM's register allocator treats the entire inlined loop body as one allocation unit. With ~30 live variables competing for 28 GP registers, it must spill. It correctly prioritizes `bit_container`/`bits_consumed` in registers (most frequently accessed). The 6 FSE-only values on the stack are the minimum spillage.

Any change that perturbs LLVM's allocation — reordering sections, introducing function boundaries, restructuring data flow — causes it to make a *different* allocation that spills the bitstream instead. The current allocation is a local optimum, and nothing tried so far guides LLVM to a better one.

## Untested Leads

None of these has been ruled out. None has been tried either, so each is a lead rather than a finding.

1. **LLVM hints beyond `#[inline(always)]`.** Something that influences allocation per variable — marking `bit_container`/`bits_consumed` prefer-register, or the FSE fields prefer-stack. No such control is exposed at the Rust level today.

2. **A register boundary without a call boundary.** The goal is for LLVM to allocate decode and execute as separate scopes. `#[inline(never)]` reaches that boundary and pays callee-save overhead that cancels the gain, as attempts 4 and 5 measured.

3. **`core::arch::asm!` with register constraints**, pinning `bit_container` to a register across the loop body without writing the loop in assembly.

4. **Rust-level patterns that lower live-variable pressure.** The C version compiles through the same backend and spills less from the same algorithm, so the IR reaching LLVM differs in shape. What shapes it differently is not established.

5. **Nightly leverage**: `#[optimize(speed)]`, allocator hints, or MIR-level annotations.

6. **Stopping SROA from decomposing a by-reference struct** into individual scalars. Three raw `u64` values held memory-resident and read via base+offset, instead of nine decomposed scalar spills, would give a friendlier stack-traffic pattern.

## Reproduction

```bash
# Build with assembly output
RUSTFLAGS="--emit asm" cargo build --release

# Find the .p2align 6 in the assembly — that's the loop header
grep -n 'p2align.*6' target/release/deps/zstandard-*.s

# Count loop instructions between LBB_XX (loop header) and the back-edge
# The loop body is ~251 instructions including cold paths

# Benchmark decode throughput
RUST_MIN_STACK=67108864 cargo run --release --bin benchmark_report -- \
    --case wikipedia,json-records --levels 1,3,5 --quick
```

## What Would Count As Progress

Closing part of the 73-instruction gap (251 to 178) or the 10 stack-op gap (35 to 25) **without** displacing `bit_container`/`bits_consumed` from registers. Roughly 20 instructions is the point at which decode throughput moves measurably. A change that improves one count while spilling the bitstream is a regression, whatever the instruction total says.
