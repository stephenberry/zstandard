# Upstream Parity Plan

This plan covers the feature gaps between `zstandard` and upstream `zstd` at the revision pinned in `upstream-zstd.ref` (`v1.5.7`). It does not cover ratio or throughput work, which is tracked separately; the one open performance item there (encode throughput at levels 3 and 4) is unaffected by anything below.

The wire format itself is already covered. Every gap here is either an *encoder-side* feature producing ordinary v1 frames, a *parameter* the caller cannot currently reach, or a *format layered on top of* frames. Two items need new decoder capability, both in the standalone section: magicless frames need `DecoderOptions::format`, and the frame-size query helpers add public decoder API.

## Where byte parity is the gate, and where it is not

This distinction decides most of the acceptance criteria below, so it is stated once here.

> **Superseded for the core codec, 2026-08-06.** What follows described the acceptance criteria this plan was written under, and the items below were accepted against it. It is no longer how the crate decides it is correct: this crate now diverges from upstream deliberately wherever the divergence is smaller or faster, so a gate that fires on *any* difference fires on success as readily as on regression. `docs/ORACLE_PLAN.md` has the five layers that replace it, and the paragraph immediately below survives only in the weakened form recorded there.
>
> The parity apparatus is kept and still runs. It is the fastest way to *localise* a defect, which is a different job from deciding whether one exists.

**Byte parity was the gate for the core codec.** Compression parameters, negative levels, and LDM all change what the parsers emit. For these, "byte-identical to upstream at the same settings" was the acceptance criterion, because it is the only check strong enough to catch a heuristic applied in the wrong order, and because this crate's ratio story rested on it at the time.

The first clause is still true, and is why the structural comparison in `ORACLE_PLAN.md` layer 2 is worth keeping as a diagnostic. The second is not: the ratio story now rests on a one-directional size bound against upstream (layer 3) and a recorded self-baseline of this crate's own output (layer 4, `tests/baseline.rs`), neither of which treats a *smaller* frame as a failure. `BENCHMARKS.md` measures byte counts against the pinned revision but is regenerated rather than asserted, so it is a report and not a gate.

**Byte parity is explicitly not a goal for the orchestration layers.** Multithreaded compression and the seekable format sit above the codec: they decide *how input is divided into frames and jobs*, not how any given block is parsed. Upstream's particular division is one reasonable choice among several, and matching it byte for byte would mean importing design decisions rather than results. For these, the criteria are interoperability (upstream reads what this crate writes and vice versa), correctness, and a bounded ratio cost against the single-threaded path.

That choice has a concrete payoff in Phase 4. Upstream's `nbWorkers = 1` is *not* byte-identical to its own single-threaded path, because `ZSTD_compress2` only bypasses the MT machinery when `pledgedSrcSize <= ZSTDMT_JOBSIZE_MIN` (`zstd_compress.c:6392`); above that, one worker still job-splits and overlap-seeds. Measured at level 5 on 8 MiB: single-threaded 3732566 bytes, `nbWorkers = 1` 3794417 bytes. Chasing upstream's bytes would mean reproducing that, and giving up the much more useful invariant that one worker equals no workers. This plan takes the invariant.

### The repcode substitution

The concrete case that decided the paragraph above, and the one several comments in `src/window/` and `src/encode.rs` point back here for.

The lazy family's regular-match store emits a repcode whenever the distance its search produced happens to still be live in the repeat offsets, which costs an offset code of 0 or 1 instead of a full explicit offset and its extra bits. Upstream cannot do this: `ZSTD_compressBlock_lazy_generic` stores the `offBase` its search produced without ever comparing it against `offset_1/2/3`, and `REPCODE2_TO_OFFBASE` and `REPCODE3_TO_OFFBASE` appear nowhere in `zstd_lazy.c`. These are legal Zstandard frames that upstream would not itself have produced.

It is worth 3.5-4.5% at levels 13 to 15, where btlazy2 is the only strategy with no row match finder, and 0.648% over the nine non-dictionary corpora once the same substitution was extended to the row-lazy band at levels 5 to 12: `json-records` 13.40% at levels 6 and 7, `log-lines` and `wikipedia` 2.9-4.1% at levels 10 to 12, and exactly zero on the five corpora whose match distances do not recur. Restoring parity instead makes five of nine rows byte-identical and opens four size gaps. It was measured on branch `btlazy2-no-repcodes` and declined on those numbers, so **do not "fix" the divergence** without measuring both directions again.

Fast and DoubleFast are deliberately left out of it -- 0.014% to 0.071%, one row worse, and they are the throughput levels. btopt and above gain nothing, because their price model already considers repcodes while searching.

**The guarantee the choice rests on is legality, and it is a test rather than an assumption.** `upstream_decodes_frames_from_every_strategy_and_framing` in `tests/upstream_interop.rs` is what says upstream can still read whatever the substitution emits, and it earns its keep here: extending the substitution to the long-distance store *fails* to round-trip on a conforming decoder. Fast and DoubleFast track `rep1` and `rep2` in locals and write the third slot back untouched, exactly as upstream's do, so their parsers never emit a code meaning `rep3` and the stale third slot never surfaces until the long-distance store codes against it. That extension is therefore not landed. It needs a repeat-offset update that leaves the third slot alone, called by the long-distance store in place of the ordinary one.

## The gaps, in order

| # | Feature | Blocks | Size | Parity gate | Upstream reference | Status |
| --- | --- | --- | --- | --- | --- | --- |
| 1 | Advanced parameter surface | 2, 3, 4, 5 | Medium-Large | Bytes | `ZSTD_c_*` in `lib/zstd.h` | **landed** |
| 2 | Negative compression levels | — | Small | Bytes | `lib/compress/clevels.h` row 0 | **landed** |
| 3 | Long-distance matching | — | Large | Bytes | `lib/compress/zstd_ldm.c` | |
| 4 | Multithreaded compression | — | Large | Interop | `lib/compress/zstdmt_compress.c` | |
| 5 | Seekable format | — | Medium | Interop | `contrib/seekable_format/` | |
| S | Standalone small items | — | Small each | mixed | various | |

The **Parity gate** column reads as written for items already landed, which were accepted under it. For anything still open it now means the layers in `docs/ORACLE_PLAN.md`: legality against upstream's decoder, a one-directional size bound, and a self-baseline. "Bytes" survives as the diagnostic to reach for first, not as the condition for landing.

Phase 1 comes first because every later phase needs somewhere in the public API to live, and because two of its items (the window-size constant and a pledged source size) turn out to be prerequisites rather than conveniences. Phase 2 is next: it is the smallest real feature and it exercises the Phase 1 surface end to end before anything expensive is built on it.

Phases 3, 4, and 5 are independent and can be reordered or dropped by priority. LDM before threads still avoids a retrofit, and the coupling is deeper than shared state: both of upstream's MT sizing formulas have a distinct LDM branch.

---

## Phase 1: Advanced parameter surface — **landed**

Implemented. `ParameterOverrides`, `Strategy`, `ParameterBounds`, `Format`, `EncoderOptions::pledged_src_size` and `EncoderOptions::write_content_size` are public; `DecoderOptions::format` and `parse_frame_header_with_format` are the decoder half of magicless frames, which the standalone section had listed separately and which is done.

849 swept override rows are byte-identical to upstream at the matching `ZSTD_c_*` settings. A further 528 are recorded rather than asserted, almost all of them `strategy` overrides; the counts are pinned inside the sweep so neither can drift silently. The `MAX_DECLARABLE_WINDOW_SIZE` decision went the way the plan recommended: `window_log` is bounded at 27 and the streaming buffer work is not attempted.

### What the two-pass adjustment actually buys

The plan called the order load-bearing without saying where. It is: an override of `strategy`. Adjustment sizes `chain_log` from `cycleLog = chainLog - isBinaryTree(strategy)`, so pass 1 uses the *table's* strategy and only pass 2 sees the caller's. Every other override converges under either order, because the window shrink that drives the rest is a function of the source size alone. `overrides_are_applied_between_the_two_adjustment_passes` reads the applied parameters back out of upstream directly, over a 147-row matrix, rather than inferring them from a compressed size.

The plan's other worry, that the two passes take different `useRowMatchFinder` arguments, turns out not to arise. A `ZSTD_CCtx` carries `ZSTD_ps_auto` through both calls and resolves it only afterwards (`zstd_compress.c:6379`); the resolved value reaches `ZSTD_adjustCParams_internal` only if the caller sets `ZSTD_c_useRowMatchFinder`, which this crate does not expose. Both passes run with `auto`.

### Two divergences taken deliberately

**`target_length: Some(0)` has no upstream equivalent.** `ZSTD_overrideCParams` reads `0` as "unset" (`zstd_compress.c:1633`), so upstream cannot request it and there is nothing to compare against. Expressing "unset" as `None` is the point of the `Option`, and the resulting hole is one value of one parameter.

**A pledged size that disagrees with the input is rejected.** Upstream's `ZSTD_compress2` silently overwrites the pledge with the real length. This reports it, for the same reason `try_new` rejects an out-of-range level: a caller who pledges the wrong number has a bug, and compressing anyway hides it.

### Recorded gaps

**`window_log` below the size of the frame.** The parse half is closed; the block half is not.

*The parse.* Every parser but the fast pair now takes its floor at the position doing the looking, `ZSTD_getLowestMatchIndex(ms, curr, windowLog)`, instead of once per block from the block's end. With one floor per block and a block as wide as its window, that floor landed exactly on the block's start and the parser found no earlier history whatsoever. Sweeping `window_log` 15 and 17 across seven levels and nine corpora at 128 KiB, byte-identical rows went from 98 of 126 to 112, and the total from 16757 bytes above upstream to 152 below; the same sweep against a raw dictionary cut its summed absolute deviation from 16468 bytes to 1790. At ordinary levels on 4 MiB with no override, the six rows that moved are all level 5 and 7 and their summed absolute deviation fell from 845 bytes to 55, one of them to byte-identical. `overriding_the_window_below_the_frame_matches_upstream` now asserts byte parity. Cost: median +0.37% encode throughput over 24 corpus/level pairs measured interleaved, everything inside ±1.7% except `log-lines` at level 15, which is 6.7% slower — btlazy2 paying for the history the floor makes reachable, on output that did not change.

*The blocks.* **Done.** Both encoders now cap the block at the window (`block_size_max_for`, `block_size_for`) and declare the window alone (`frame_window_size_for`, `StreamingEncoder::frame_window_size`), which is upstream's arrangement. `overriding_the_window_below_the_frame_matches_upstream` no longer has to hold `block_size` at the window by hand to reach byte parity; it asserts both, held and unheld. Nothing else moved: 176 encodes across all 22 levels, one-shot and streaming, at three sizes and with a dictionary, are byte-identical either side, because no compression level produces a window narrower than a block. `a_block_never_exceeds_the_window_the_frame_declares` guards the arrangement, asserting the block *count* rather than only the declared window, so a header claiming the right window over uncapped blocks would still fail.

The block on top of the history had been load-bearing for two reasons, both now gone.

First, the **fast and double-fast** parsers deliberately keep C's block-constant floor, and this crate clamps it with `.min(block_start)`, so a block wider than its window is floored at the block's start rather than a window back from its end. A dictionary-less frame at level 1 with `window_log: Some(15)` and default blocks can emit an offset approaching the block width against a 32 KiB window. Capping the block removes it.

Capping surfaced a third defect that the wide declaration had been hiding, in the **ext-dict double-fast** parser. It merges the dictionary and the frame into one logical index space and tested candidates against `prefix_base + prefix_low` alone — the bottom of that space, so source candidates were unbounded. Streaming with a raw dictionary and no pledged size selects a 16 KiB window, and a block whose floor forbade any match at all emitted one reaching 32690 bytes. Same shape as the binary-tree defect, fixed the same way; it is the third bound in this crate that was expressed in the wrong coordinate space.

Second, the **prefixed** parsers could emit a match below the source floor they were handed. **Fixed.** It was `BinaryTreeLazy2` alone, and it was a coordinate-space error rather than anything about the floor's value. Every finder but the binary tree addresses the prefix and the source as two buffers and takes a floor into each; `BinaryTreeFinder` indexes both in one space, prefix position `p` at `p + DICT_POS_BIAS` and source position `p` at `prefix_len + p`. `best_regular_match_with_prefix_chain` passed it `source_low`, so its bound was short by the whole dictionary. A sweep of 7 strategies × 3 window logs × 4 dictionary sizes × 3 body sizes × 5 levels failed on 119 of 1260 cases, all 119 btlazy2; after folding the floor into the tree's own index space, 0 of 1260. Output is unchanged where the floor never bound: 132 ordinary dictionary encodes across all 22 levels are byte-identical either side. `every_parser_stays_inside_its_window_with_a_dictionary` guards it.

An earlier draft of this section blamed `prefixed_window_lows`' `.min(block_start)` clamp for collapsing the prefixed floor to zero. That was wrong twice over: the clamp yields `block_start`, which is zero only for the first block of a buffer, and after the per-position change only the fast pair still reads that pair at all. The reasoning that reached it was also invalid — it compared cap-off against cap-off. Measure with the cap *on* before believing any account of this.

All of it has now landed. Note what did *not* catch any of the three floor defects: the full suite passed throughout each time. Only `cargo test --lib --features internal-fuzz fuzz::` and the `dictionary_encode_roundtrip` target reached the first two, because nothing else combined a dictionary with a `window_log` override; the third was invisible until the blocks were capped, because the wide declaration covered it. Capping the blocks is therefore worth more than parity — it removes the slack that was hiding the defect.

One wart is left where the two regions meet. Prefix positions are stored biased by `DICT_POS_BIAS` and source positions are not, so stored indices `prefix_len` and `prefix_len + 1` are ambiguous: the region test `mi < prefix_len` reads the last two prefix positions as the first two source positions. It predates this work and is bounded at two positions, so it is recorded rather than fixed.

Separately, C keeps the **whole** dictionary reachable while any byte of it is inside the window, which lets its offsets exceed `Window_Size`. This crate trims the dictionary to the window instead, because its decoder bounds a dictionary match by the window. Matching C there is decoder work as much as encoder work, and it is why the prefixed floor keeps C's two branches — block-constant while the dictionary is live, per position once it has aged out — rather than one.

**Any override of `strategy`.** 85 of 459 swept rows differ. Output is always valid, always round-trips, always readable by upstream, and more often smaller than larger. The clearest family is `BinaryTreeLazy2` driven by a level whose own row is `Fast`: a `search_log` of 1 and a `chain_log` of 12 give a binary tree two probes deep over a roll buffer of 2048 positions, which no level asks for. `overriding_the_strategy_leaves_the_levels_parameter_space` holds the identical fraction above 70% so the number can only move up.

**The rows listed in `OVERRIDE_PARSE_GAPS`.** Most differ by six bytes or fewer and several by none at all, which reads as parse ties. `window_log` and `chain_log` are exact on every row, and `target_length` on all but one; the residue is concentrated in `min_match`. The one family that is not a tie is `min_match: Some(7)` against a dictionary, where this crate comes out 170 to 222 bytes *smaller*.

### Four defects the overrides found

**A hash wider than 24 bits took the fast parsers out of bounds.** `FastFinder` and `DoubleFastFinder` pack a table index and an 8-bit tag into one 32-bit hash, so `hash_bits + SHORT_CACHE_TAG_BITS` above 32 underflowed the shift: a panic in debug, an out-of-range table index in release, reached through `unsafe` accessors. Upstream has no such bound on its no-dictionary fast parser, which hashes with `hlog` alone and carries the tag only in CDict tables whose `hashLog` is already clamped to 24. The widest `Fast` row in the level table is 17, so nothing but an override could reach it. Now clamped at `MAX_TAGGED_MATCH_HASH_BITS`, with `a_hash_log_wider_than_the_tag_stays_in_bounds` covering it.

**The optimal parsers ignored a `target_length` of zero.** They substituted `good_enough_match_length` for upstream's `sufficient_len = MIN(targetLength, ZSTD_OPT_NUM - 1)`. Every optimal row in the level table carries at least 12, so only `strategy: Some(BinaryTreeOpt)` on a level whose row is `Fast` could reach it — and there it was worth up to 6 KB on a 128 KiB frame. The substitution had a plausible-looking comment claiming to match C.

**An empty-content dictionary took its history floor from the block's start.** A dictionary whose content is empty takes a third encode path — not contiguous, not prefixed, but the pair-of-prefix-slices one — and it was the only one of the three measuring from the block's start rather than its end, so a match late in a block could reach a whole block past the declared window and this crate's own decoder rejected the frame. Invisible at every level, because adjustment leaves the window at least as wide as the source plus the dictionary. Four minutes of cargo-fuzz on the dictionary target found it, which is the argument for having taught those targets to generate overrides; `a_narrow_window_with_an_empty_dictionary_stays_inside_it` covers it.

**A reused `Encoder` keyed its cached match state on the requested hash width, not the built one.** Harmless while the two always agreed; the fast tables' new 24-bit cap made them differ, and the key then stopped matching anything. `match_hash_bits` and `tagged_match_hash_bits` exist so construction and the reuse test cannot drift apart again, and `an_encoder_rebuilds_its_match_state_when_parameters_change` fails if the key is loosened.

### What was not done

**`useRowMatchFinder` is not exposed**, and neither is `ZSTD_c_srcSizeHint` as a parameter separate from the pledge. The first would make the two adjustment passes genuinely differ; the second differs from the pledge only when a caller sets both and the pledge is unknown.

### Original goal

Let a caller override the compression parameters a level would otherwise choose, with upstream's semantics and upstream's validation, without exposing internal encoder structure.

### Why it came first

`EncoderOptions` carried four fields: `block_size`, `checksum`, `write_dict_id`, `compression_level`. Upstream exposes roughly thirty `ZSTD_c_*` parameters. The seven compression parameters proper already existed internally as `UpstreamCompressionParameters`, plumbed through `compression_parameters_for_input` into `MatchFinderParameters`.

`docs/SEMVER.md` treats adding a field to `EncoderOptions` as non-breaking, provided callers construct with `..Default::default()`, which the README instructs. This phase was additive.

### Design as built

`ParameterOverrides`, every field `Option`, `None` meaning "use whatever the level chose". This mirrors upstream, where `0` means "unset", but expresses it in a type where the sentinel cannot collide with a legitimate value.

```rust
pub struct ParameterOverrides {
    pub window_log: Option<u32>,
    pub hash_log: Option<u32>,
    pub chain_log: Option<u32>,
    pub search_log: Option<u32>,
    pub min_match: Option<u32>,
    pub target_length: Option<u32>,
    pub strategy: Option<Strategy>,
}
```

`Strategy` is a public enum mirroring the internal `UpstreamStrategy`, with upstream's nine names and its discriminants. Keeping them distinct leaves the internal one free to change.

#### Upstream's resolution order

From `ZSTD_getCParamsFromCCtxParams` (`zstd_compress.c:1640-1651`):

```
srcSizeHint from CCtxParams if caller did not pass one   [:1641-1644]
table row lookup + negative-level targetLength
    → adjustCParams_internal (#1, mode, ZSTD_ps_auto)    [inside, :7780]
→ LDM windowLog force, if LDM explicitly enabled          [:1646]
→ ZSTD_overrideCParams                                    [:1647]
→ adjustCParams_internal (#2, mode, CCtxParams->useRowMatchFinder) [:1650]
```

`upstream_cparams_for_level_with_mode` reproduces this, with a comment naming the LDM force's position for Phase 3.

#### Overrides apply to the dictionary path too

`ZSTD_initLocalDict` (`zstd_compress.c:1271-1277`) builds its CDict through `ZSTD_createCDict_advanced2`, which at `:5680` calls `ZSTD_getCParamsFromCCtxParams(..., ZSTD_cpm_createCDict)` — and that function applies `ZSTD_overrideCParams` at `:1647`. Overrides land on **both** sides. `EncoderDictionary` models `ZSTD_CCtx_loadDictionary`, so `upstream_full_dict_cparams_for_level` threads them into both of its calls. Applying them to only the first produces valid, decodable, silently-wrong-versus-upstream dictionary output; `parameter_overrides_reach_the_dictionary_side_too` compares the applied parameters against upstream rather than the bytes, so a failure names the parameter that drifted.

One place they deliberately do *not* reach: dictionary *training*. `ZDICT_analyzeEntropy` goes through `ZSTD_getParams`, which takes a level and nothing else, so a caller's overrides configure their encoder and not the trainer.

#### Also in this phase

**A pledged source size.** `EncoderOptions::pledged_src_size`, upstream's `ZSTD_CCtx_setPledgedSrcSize`. Streaming used to pass `None` unconditionally and so always selected tier 0. It now selects by the pledge, declares a content size, and checks the promise at `finish`. Note this was *not* the cause of the streaming byte divergence recorded under Phase 2: upstream's streaming helper does not pledge either, so both sides were on tier 0 and still differed.

**`MAX_DECLARABLE_WINDOW_SIZE`** stays at `1 << 27`, and `ParameterOverrides::WINDOW_LOG` is bounded to match. Raising it would mean reworking `frame_capacity_for`, which is `history_limit * 2 + block_size` and reaches 4 GiB at `windowLog` 31.

**`write_content_size`.** Implemented as writer selection, as the revised plan predicted: `write_single_segment_header` structurally cannot omit the field, and upstream derives `singleSegment = contentSizeFlag && (windowSize >= pledgedSrcSize)` (`zstd_compress.c:4704`) rather than taking it as a setting.

**`Format`.** Both halves. The type lives in `frame.rs`, since the header writer and the header parser both need it.

### Trap avoided

`src/encode.rs` has an `UpstreamStrategy::DoubleFast => target_length.max(1)` arm feeding `fast_search_step`, which nothing reads: `zstd_double_fast.c` contains no `targetLength` reference at all. Wiring it up would break parity. It is still there and still unread; the two dead functions in `src/window/fast.rs` that would consume it (`search_fast_without_prefix`, `search_fast_with_prefix`) still have zero call sites. Left alone deliberately — a `strategy: Some(DoubleFast)` override does not reach them.

---

## Phase 2: Negative compression levels — **landed**

Implemented. 480 of 484 swept rows are byte-identical to upstream; the four that are not are recorded below and in `negative_levels_are_byte_identical_to_upstream`.

The estimate held, but the *reason* it was called small did not survive contact. The plan said the work was only the level mapping, because the acceleration already matched. Both halves of that were true and it was still incomplete: upstream's `ZSTD_literalsCompressionIsDisabled` (`zstd_compress_internal.h:696`) turns Huffman coding of the literals section off whenever `strategy == ZSTD_fast && targetLength > 0`, which is exactly and only the negative levels. Nothing in the parser, the level table, or the cparams hints at it.

Without that, every negative-level row came out around **0.64x of upstream's size** — uniform enough across corpora to look like a win rather than a defect. The cparams probe is what ruled the parse out: ours were byte-for-byte upstream's (`W=19 C=12 H=13 S=1 L=6 TL=|level|`, Fast) while the output was wildly different, which left only the entropy stage. Worth carrying into Phase 1: a "level" in upstream is not just a row of the cparams table, and a parameter surface that only exposes cparams will not reproduce it.

Two details of the disable that matter: it returns raw literals *before* the RLE check as well as before the Huffman attempt, so a disabled frame emits raw even where RLE would be one byte; and it is keyed on the resolved parameters, not the level, so it stays correct once Phase 1 lets a caller ask for `Fast` with a non-zero target length directly.

### Recorded gaps

Sweeping every corpus at 1 MiB against levels `-40..=-1` plus `-50`, `-100`, `-1000` and the floor left four rows: `wikipedia` at -10 (4 bytes), -27 (83) and -34 (2), and `raw-dictionary` at -2 (1). All four are *smaller* than upstream.

Each is one block's payload inside an otherwise identical frame: same block count, same size for every other block, same parse. On `wikipedia` at -10 the gap is absent at 512 KiB and 640 KiB and present from 768 KiB up, and at 1.5 MiB two further levels join it, so it is a real per-block encoding difference under acceleration rather than a tie. It is unexplained, and it is the natural first customer for the parser-output trace harness, which remains unbuilt.

### Two divergences that are not negative-level defects

Both were found while reviewing this work, both predate it, and both are recorded here because a negative level is the easiest place to notice them.

**Streaming is not byte-identical to upstream at any level.** On a 1 MiB text corpus fed in 64 KiB pieces with no pledged size, levels -5 and -10 come out 0.25% and 0.14% smaller than upstream; at 3 MiB, levels 3 and 5 diverge too (level 3 by 0.49%). Block layout still matches and the first blocks are byte-identical, so this is the same class of streaming divergence the existing `streaming_block_layout_matches_upstream_streaming` bound already tolerates, not something negative levels introduced. The obvious suspect was the missing pledged source size, and Phase 1 ruled it out: upstream's streaming helper does not pledge either, so both sides were already on tier 0. Still open.

**An RLE-dominated block can diverge by a whole block.** On 1024 identical bytes at level -1000, this crate emits an 11-byte RLE block and upstream emits a 1034-byte raw block: upstream's parser finds no match at that acceleration, every byte becomes a literal, raw literals push the block past `srcSize`, and it falls back to `ZSTD_noCompressBlock`. The same divergence exists at levels 1 and 3 at 11 vs 19 bytes; acceleration amplifies it from 8 bytes to ~1023. Ours is smaller and upstream decodes it, so it is a parity gap rather than a correctness one.

### Divergences taken deliberately

`CompressionLevel::MIN` was redefined from `1` to `-131072`, matching `ZSTD_minCLevel()`, and `MIN_POSITIVE` added. `FASTEST` stays `1`. `as_u8` is gone; every call site moved to `as_i32`, including six in `src/fuzz.rs` and `src/bin/trace_bad_blocks.rs` that the plan's first draft did not name.

`try_new` rejects out-of-range levels where upstream clamps. A caller asking for `-200000` has a bug, and compressing at a level they did not name would hide it. Every level accepted is byte-compatible with upstream, so the divergence is in error reporting only.

### Original goal

Accept levels below 1, matching upstream's `ZSTD_minCLevel()` of `-131072`, and produce byte-identical output.

### Why it is small

Only the level-to-parameters mapping is missing. Upstream (`zstd_compress.c:7766-7778`) selects table row 0 for any negative level and sets `targetLength = -MAX(ZSTD_minCLevel(), level)`. Row 0 is absent from the Rust table, whose `match` arms start at 1 and end in `unreachable!()` (`src/encode.rs:711`). The four values, from `lib/compress/clevels.h:28`, `:54`, `:80`, `:106`, in the same `W C H S L TL` order the Rust `upstream_cparams(…)` takes:

| Tier | Source size | W | C | H | S | L | TL | Strategy |
| --- | --- | ---: | ---: | ---: | ---: | ---: | ---: | --- |
| 0 | > 256 KiB | 19 | 12 | 13 | 1 | 6 | 1 | Fast |
| 1 | ≤ 256 KiB | 18 | 12 | 13 | 1 | 5 | 1 | Fast |
| 2 | ≤ 128 KiB | 17 | 12 | 12 | 1 | 5 | 1 | Fast |
| 3 | ≤ 16 KiB | 14 | 12 | 13 | 1 | 5 | 1 | Fast |

Row 0 is always `ZSTD_fast`, so negative levels exercise only the fast parser, and its acceleration already matches upstream on all three variants:

| Upstream | zstandard |
| --- | --- |
| `zstd_fast.c:200` noDict, `tl + !tl + 1` (min 2) | `fast_step_size` = `fast_search_step.max(2)` = `tl.max(1) + 1` (`src/window/fast.rs:210`), used at `:578`, `:923`, `:1541` |
| `zstd_fast.c:491` dictMatchState, `tl + !tl` | `fast_dict_match_step_size` = `fast_search_step - 1` = `tl.max(1)` (`:214`), used at `:1329`, `:1967` |
| `zstd_fast.c:717` extDict, `tl + !tl + 1` | `fast_step_size` at `:1541` |

Level `0` is not a negative level: upstream maps it to `ZSTD_CLEVEL_DEFAULT` (3). Preserve that aliasing and test it, since `0` is the value a caller most easily reaches by accident.

### A trap to avoid while doing this

`src/encode.rs:532` has a `UpstreamStrategy::DoubleFast => target_length.max(1)` arm. **It is read by nothing.** `zstd_double_fast.c` contains no `targetLength` reference at all; its step is `1` (`:168`) grown adaptively (`:227`). The only consumers of `fast_search_step` are in `src/window/fast.rs`, and the two functions that would consume it for the double-fast path, `search_fast_without_prefix` (`src/window/fast.rs:2194`) and `search_fast_with_prefix` (`:2250`), have zero call sites.

This matters once Phase 1 lets a caller set `strategy = DoubleFast, target_length = 64`. The obvious-looking fix of wiring `fast_search_step` into `double_fast.rs` would break parity, because upstream does not do that. Either delete the dead arm and the two dead functions, or comment the arm as deliberately unused.

### Work breakdown

- [ ] Add row 0 to all four tiers of `upstream_cparams_table_entry`.
- [ ] Apply row-0 selection and `targetLength = -level`, clamped at the floor.
- [ ] Resolve `CompressionLevel::MIN`, which **already exists and already means `1`** (`src/encode.rs:330`) and is read by `try_new` (`:343`). Redefining it to `-131072` is a silent semantic change: existing code keeps compiling and changes behavior. Either redefine it deliberately, as a called-out breaking change while the crate is pre-`0.1.0` and its meaning then matches `ZSTD_minCLevel()`, or leave `MIN` alone and add a separate floor constant. Recommendation: redefine, and add `MIN_POSITIVE = 1`, since matching upstream's vocabulary is worth more than source compatibility on a crate that has not shipped.
- [ ] Relax `try_new` to the new range.
- [ ] Audit `as_u8` (`src/encode.rs:362`), whose doc comment already anticipates this and which becomes wrong for negative levels. Call sites: `src/encode.rs:676`, `:2750`; `src/fuzz.rs:143`, `:345`, `:665`, `:684`, `:689`; `src/bin/trace_bad_blocks.rs:269`, `:306`; `src/bin/benchmark_report.rs:138`, `:280`, `:1541`. **`src/streaming.rs` has none.** The load-bearing one is `src/encode.rs:676`, where `upstream_cparams_table_entry(tier, level.as_u8())` takes a `u8` and needs a signature change.
- [ ] Decide what `src/fuzz.rs:143` does, where a `match level.as_u8()` drives fuzz body sizing and needs a negative-level arm.

### Tests

- Byte-identical interop for levels `-1` through `-10`, plus `-100`, `-1000`, and the floor.
- Levels below the floor clamp rather than error, matching `MAX(ZSTD_minCLevel(), level)`.
- `try_new(0)` yields the default level, and encoding at it is byte-identical to encoding at 3.
- Tier selection by source size at a negative level: 1 MiB, 200 KiB, 100 KiB and 8 KiB should select `W19`, `W18`, `W17` and `W13` respectively.
- Dictionary compression at negative levels, which routes through the CDict path with row 0.
- Property round-trips extended to cover negative levels.

### Acceptance

Byte-identical to upstream at every negative level tested, on every corpus case, with `BENCHMARKS.md` extended to report them.

### Risks

Low. The hazard is a `u8` or `usize` conversion that silently wraps, and the `as_u8` audit is the mitigation. Make it exhaustive rather than sampled: the list above was wrong in the first draft of this plan, naming a file with no call sites and omitting the two files with the most.

---

## Phase 3: Long-distance matching

### Goal

Match upstream's LDM: a coarse matcher that finds very long matches at distances the per-block parsers cannot reach, because their tables are sized to the search window rather than the full history.

### What LDM actually does to the window

The first draft had this backwards, and the correction moves work between phases.

LDM does not permit a larger window. It **pins** one. `zstd_ldm.h:21` defines `ZSTD_LDM_DEFAULT_WINDOW_LOG` as `ZSTD_WINDOWLOG_LIMIT_DEFAULT` (27), and `zstd_compress.c:1646` assigns `cParams.windowLog = ZSTD_LDM_DEFAULT_WINDOW_LOG` whenever LDM is explicitly enabled and the caller has not overridden it. So enabling LDM on a level whose window is smaller *raises* it to 128 MiB, and enabling it on level 22 changes nothing.

Consequences: the `MAX_DECLARABLE_WINDOW_SIZE` work moved to Phase 1, where the `window_log` override actually motivates it; and the threat-model note about 2 GiB windows belongs to that override, not here. LDM's own ceiling is 128 MiB, which the existing decoder default already accepts.

Note also that the assignment sits between the table lookup and `ZSTD_overrideCParams`, so an explicit `window_log` override wins over the LDM force. Phase 1's resolution order must already have that position right.

### How narrow the auto rule really is

The rule is `strategy >= ZSTD_btopt && windowLog >= 27` (`zstd_compress.c:272`), which the first draft glossed as "levels 16 and up". In `clevels.h` tier 0 the window logs at levels 16 through 22 are 22, 23, 23, 23, 25, 26, 27. **Only level 22 reaches 27.** The rule is also evaluated on adjusted cparams (`zstd_compress.c:6378`), so it additionally needs `srcSize + dictSize > 2^26`.

Measured against the pinned checkout: levels 15 through 21 never auto-enable at any size; level 22 auto-enables at a pledged size of 67108865 and not at 67108864; level 16 with an explicit `windowLog` override of 27 auto-enables above 2^26.

So the practical urgency is small: default-configured output diverges from upstream only at level 22 on inputs above 64 MiB, which no current corpus (4 MiB) reaches. The auto rule still has to be reproduced, but it is a correctness item rather than a live ratio gap.

### Structure

LDM is not a whole-input pre-pass. `ZSTD_ldm_generateSequences` is called **per block**, inside `ZSTD_compressBlock_internal` (`zstd_compress.c:3326-3340`), against an `ldmState` whose window advances per `compressContinue` call (`:4820-4822`), carrying leftover-match state across calls (`zstd_ldm.c:583`). Only the multithreaded path pre-generates per job, serially, in `ZSTDMT_serialState_genSequences` (`zstdmt_compress.c:726`).

This matters structurally: a whole-input pre-pass is not something the streaming encoder can do, and it would not reproduce upstream's per-block boundary truncation. Build the per-block shape from the start.

Within a block, `ZSTD_ldm_blockCompress` splits in two on `cParams->strategy >= ZSTD_btopt`. Below that it walks the LDM sequences covering the block, emits them directly, and runs the ordinary parser on the gaps between them. At and above it, the store is handed to the optimal parser through `ms->ldmSeqStore` and each long-distance match becomes one more *candidate* the parser prices against what it found itself.

### Parameter defaults

`ZSTD_ldm_adjustParameters` (`zstd_ldm.c:135-167`) assigns `params->windowLog = cParams->windowLog` first (`:138`); everything below reads it. The derivations are interdependent and ordered:

| Parameter | Resolution | Bounds |
| --- | --- | --- |
| `ldm_hash_rate_log` | if unset: `windowLog - hashLog` when `hashLog` was set **and** `windowLog > hashLog` (`:145`), else `7 - (strategy / 3)`. If `hashLog >= windowLog`, stays `0`. | `0..=(WINDOWLOG_MAX - HASHLOG_MIN)` |
| `ldm_hash_log` | if unset: `clamp(windowLog - hashRateLog)` into the hash-log bounds | `HASHLOG_MIN..=HASHLOG_MAX` |
| `ldm_min_match` | if unset: 64, halved to 32 when `strategy >= btultra` | `4..=4096` |
| `ldm_bucket_size_log` | if unset: `clamp(strategy, 4, 8)` | `1..=8` |
| — | **then, unconditionally**: `bucketSizeLog = min(bucketSizeLog, hashLog)` (`:166`, outside the unset check, so it applies to caller-supplied values too) | |

Order matters: `hashRateLog` feeds `hashLog`, which caps `bucketSizeLog`. Any other order yields a different table shape and different output.

The public knob is a three-state `LdmMode { Auto, Enabled, Disabled }`, not a `bool`, because `Auto` is upstream's default and is not expressible as either boolean.

### Work breakdown

- [x] Add the four LDM parameters and `LdmMode` to the Phase 1 surface, with the ordered default derivation above.
- [x] Implement the window force at the correct position in the resolution order, and confirm an explicit `window_log` override still wins.
- [x] New `src/window/ldm.rs`: rolling hash, bucketed table, insertion at `hashRateLog` frequency, per-block sequence generation.
- [x] The block compress path that interleaves LDM matches with parser output on the gaps, for the strategies below `btopt`. Includes the two per-segment table disciplines: `ZSTD_ldm_limitTableUpdate`'s 1024/512 clamp and `ZSTD_ldm_fillFastTables`.
- [x] The optimal-parser path: from `btopt` up, C hands the store to the parser as *candidates* to price against its own matches (`ms->ldmSeqStore`, consumed by `ZSTD_opt_getNextMatchAndUpdateSeqStore`) rather than laying them down.
- [x] `ldm_skip_sequences` / `ZSTD_ldm_skipRawSeqStoreBytes`, as `RawSequenceCursor::skip_bytes`. C spells this body twice — once in `zstd_ldm.c` and once as a static in `zstd_opt.c` — and the optimal path is what gives it a caller.
- [x] Frame-scoped LDM state in `StreamingEncoder`, surviving the `frame` buffer compaction. Every entry is an index into that buffer, and unlike the match finders there is no rebuild to fall back on: the table is filled by hashing forward over each block as it arrives, so clearing it forgets the whole frame. It rebases instead, C's `ZSTD_ldm_reduceTable` (`zstd_ldm.c:516`), and the memo of which block the current matches belong to moves with it.
- [x] **The matcher** with a dictionary. C keeps its dictionary and its frame in two allocations and addresses them through `base` and `dictBase`, which is why `ZSTD_ldm_generateSequences_internal` branches on `extDict` and reaches for `ZSTD_count_2segments` and `ZSTD_ldm_countBackwardsMatch_2segments` (`zstd_ldm.c:430-444`). Both helpers exist to reconstruct one property: on running off the end of the dictionary, counting continues at the first byte of the prefix — which is the definition of a contiguous buffer. `LdmSource` names a single index space with the dictionary first, so what C calls `dictEnd` and what it calls `lowPrefixPtr` are one index and the `extDict` branch has no counterpart here at all. The two slices stay separate rather than concatenated, so nothing copies the dictionary per frame.

  Two pieces of this are not the coordinate change. `ZSTD_ldm_fillHashTable` (`zstd_ldm.c:285`, called from `ZSTD_loadDictionaryContent` at `zstd_compress.c:4958`) is a *different walk* from the one generation makes over the same bytes — it feeds from the first byte with the rolling hash in its initial state and discards the splits falling within `minMatchLength` of the start, where generation primes over those bytes and begins after them. So a dictionary cannot be handled by widening the range generation is asked to search, and `filling_from_a_dictionary_is_not_the_walk_generation_makes` is what holds that apart. And `ldmState->loadedDictEnd` is a credit against the window rather than a bound within it: the dictionary is reachable *in full* while its last byte is inside the window, and is invalidated outright the moment the frame outgrows `maxDist + loadedDictEnd` (`ZSTD_window_enforceMaxDist`). That matches what this crate's decoder already accepts (`src/sequence.rs:1006-1020`), though not what its *prefixed parsers* allow themselves — they trim a dictionary to the window, a divergence recorded at `plan_sequences_for_prefixed_contiguous_block_into` and older than this work.

- [x] **The encoder** with a dictionary. The refusal is gone from all three entry points. A dictionary frame runs through `PrefixedBlockMatchState`, so the long-distance block compressor grew a prefixed half of each piece it had only contiguously: `plan_sequences_for_prefixed_contiguous_block_into` splits into a per-block floor resolution and a per-segment planner so the gaps between matches can be parsed, `Option<&[RawSequence]>` threads down to `plan_sequences_optimal_with_prefix_two_phase_into` where the same `LdmCandidates` cursor offers each match at the same two collection points the contiguous parser uses, and `PrefixedBlockMatchState` gained `limit_update_after_ldm_match` and `fill_fast_tables`.

  One thing differs from the contiguous twin and is worth stating, because getting it wrong is silent. `PrefixedBlockMatchState::limit_update_after_long_match` clamps the tree alone, since between blocks `insert_block_tail` files whole blocks and leaves no other cursor behind. A long-distance match is *inside* a block, where nothing has been filed yet, so `limit_update_after_ldm_match` has to reach every lazily-filled finder — and each in its own coordinate space, the chain in ext-dict mode and the tree counting from the start of the prefix while the row finder and the fast pair count from the start of the source. Removing the clamp was measured, not assumed: it costs 70 bytes on `wikipedia` at `Lazy`.

  Acceptance is `long_distance_matching_with_a_dictionary_is_byte_identical_to_upstream`, and its own count assertion says how weak it is. Fifty-four rows run over three corpora, nine strategies and both dictionary kinds; **forty-three are excluded because this crate's dictionary output already differs from upstream with no matcher involved**, seven carry the byte-parity claim, and four diverge having passed that baseline. All fifty-four round-trip, and none is inert — every row's frame changes when the matcher is switched on. The four divergences are ties rather than a direction: measured at 256 KiB, 512 KiB, 1 MiB and 2 MiB the set itself moves, the sign moves with it, and at 512 KiB two rows come out the *same size* with different bytes. `cargo +nightly fuzz run dictionary_encode_roundtrip` did 253k runs over the new path without a crash.
- [ ] `LdmMode::Auto`. No longer blocked on a dictionary. What is left is that honouring the rule changes *default* output in exactly one place — level 22 above 64 MiB, the only level whose window reaches the 27 the rule requires — and nothing in the suite encodes a body that large, so it wants the auto-boundary corpus below before it can be turned on against a test that would see it either way.

Two defects found here were invisible to unit tests and to a whole-input comparison, and both were caught only by driving the C oracle the way the encoder drives the matcher — per block, at several block sizes. `leftoverSize` is a local in `ZSTD_ldm_generateSequences`, not state on `ldmState_t`; and forward extension is bounded by the chunk's end rather than the buffer's. `oracles/ldm/compare.sh` now sweeps 432 configurations of corpus, window log, strategy and block size.

A third was found only by the byte-parity sweep: C clamps `ms->nextToUpdate` once per block for **every** strategy (`zstd_compress.c:3297`), where this crate had applied that clamp to the binary tree alone. Without long-distance matching nothing else can fall far enough behind for it to fire, so the gap was invisible; with it, the fast table's fill started 320 positions too early and diverged five blocks into a frame.

A fourth was in the literals encoder and had nothing to do with long-distance matching except that this parse is what exposed it. `sort_nodes` had been ported from an older `HUF_sort` that insertion-sorts each bucket; the pinned checkout quicksorts any bucket over eight symbols (`huf_compress.c:620`). Symbols that tie on count come out in a different order, and the Huffman tree built from that order gives them different code lengths — the same total cost, different bytes. It was worth one byte on a block of json records at every optimal strategy, and closed the one recorded parse-tie row as well.

A fifth was found by widening that sweep to window logs below the corpus size, which the streaming work made worth doing: at 27 the whole corpus sits inside the window, the matcher's floor is zero on every chunk, and any bound measured against it is untested. Backward extension was bounded by the start of the buffer where C bounds it at `base + dictLimit` — the same floor the forward search rejects candidates against, raised by `ZSTD_window_enforceMaxDist` on every chunk. It differs on `json-records` at strategy 8 with a window of 16, and nowhere else in the grid.

Two gaps this work measured and did **not** close, both older than it and neither involving the matcher. The first has since been fixed — see [Streaming compaction and the cycle-indexed tables](#streaming-compaction-and-the-cycle-indexed-tables) below.

- ~~The streaming encoder's *binary-tree* match state does not survive a compaction. On a body whose matches sit at seven eighths of the window it is worth 3.76x with long-distance matching switched off on both sides — 211337 bytes streamed against 56143 one-shot, at `btopt` with a window of 19.~~ **Fixed.**
- ~~`binary-structured` at `Fast` with a window of 19 streams 27.8% above upstream's streaming frame.~~ Investigated and **not a defect here** — see [Upstream's streaming beats upstream's one-shot on `binary-structured`](#upstreams-streaming-beats-upstreams-one-shot-on-binary-structured) below.

### Tests

- [x] Byte-identical interop with LDM explicitly enabled: `long_distance_matching_is_byte_identical_to_upstream` sweeps nine corpora against all nine parser families, gated on the no-LDM baseline being exact for that row. Sixty-four of the sixty-five rows with an exact baseline are upstream's bytes; the sixty-fifth is `LDM_KNOWN_DIVERGENCES`, asserted as a set so that fixing it fails the test too.
- [x] Each of the four LDM parameters. `long_distance_parameters_are_byte_identical_to_upstream` sweeps eight configurations across three corpora and three strategies, one configuration per branch of `LdmParameters::resolve`: a supplied rate deriving the hash log, a supplied hash log deriving the rate, the same spelled with an explicit zero (which C reads as unset, and which must produce the previous configuration byte for byte), a hash log the window does not exceed (which leaves the rate at zero and makes every position a split), the floor on the derived hash log, a supplied bucket size capped by the hash log, a rate wider than the minimum match (the split mask's degenerate branch), and all four supplied. The three strategies are the three ways the matcher's output is consumed: `Fast` refills its own tables over the skipped span, `Greedy` runs the bounded table update instead, and `BinaryTreeOpt` prices the sequences as candidates. Each configuration also has to move the frame off the *default* long-distance shape somewhere in the grid, since a parameter that never reached the matcher would agree with upstream on every row. `oracles/ldm/compare.sh` gained four parameters of its own, on a narrower grid and not the same eight shapes — six shapes over two of the four windows, which is what said the matcher itself was never at fault. A second test, `long_distance_parameters_resolve_what_their_cases_assume`, reads the resolved four off upstream and asserts each case still reaches the branch it is named for and that no two cases resolve alike: every value here is a subtraction from a window that is derived rather than stated, and two cases were originally written against the wrong one and passed anyway, comparing two encoders doing the same wrong thing.

  Writing it found a defect, and it was not in the matcher: the raw sequences were byte-identical to C's on every row that diverged. `plan_sequences_for_contiguous_segment_into` used to file a block's last 64 positions into the chain and row finders at the block's end. C files nothing there — a parser stops short of `iend` and the next block's first search catches up from wherever it stopped, clamped only if it is a long way behind. The eager insert was both too much and too little: it moved the cursor to the block's end, hiding the gap from those clamps, and it dropped whatever sat between where the parser stopped and those last 64 bytes. On `json-records` at greedy the parser stopped 108 bytes short of a boundary, so positions 64 through 108 back were never filed, and the next block missed a 79-byte match upstream took. Deleting it also closed five rows of the sweep above that had no long-distance matching in them at all, taking the baseline gaps from 21 to 16, and cost one row three bytes — `("wikipedia", 5, 3)` in `KNOWN_UPSTREAM_SIZE_GAPS`. The one LDM divergence now visible, `wikipedia` at greedy, was behind one of the five baselines it fixed and predates the change: 18541 bytes before and 18545 after, against upstream's 18732 both times.

  The sweep's own vacuity is measured, not assumed. `LDM_PARAMETER_INERT_ROWS` records every corpus, strategy and window where enabling the matcher changes the frame not at all, and asserts the set: those rows reproduce the no-matcher frame under all eight cases, agree with upstream doing the same, and prove nothing. Adding it evicted `repeated-chunk`, which had been in this sweep as twenty-four such rows — a megabyte of a 46-byte chunk leaves the matcher nothing the parser has not taken — in favour of `wikipedia`, where the matcher is worth 66 KiB at `Fast`. Three rows remain and are recorded: `log-lines` at the pinned window of 17.
- [x] The window force: LDM enabled at a level whose window is below 27 raises the declared window, and an explicit `window_log` override beats it. Observable only where the level's window is narrower than the source, since fitting the window to the source only ever shrinks it.
- [x] Fuzz coverage. All three encode targets now reach the matcher, from spare bits in `control[6]`: one turns it on and four pick which of its own parameters are overridden alongside it. Since the forced 128 MiB window would make every iteration allocate an eight-megabyte table to search a body that cannot reach past a block, the targets supply a `window_log` of their own, confined to `14..=18`. Both ends are measured. The ceiling bounds the *derived* `ldm_hash_log` as well as a supplied one, because nothing fits a long-distance table to the source the way it fits the parser's. The floor is about time: a block is capped at the window, so `btultra2` at a window of 10 turns a 128 KiB body into 128 blocks and spends 26 s of libFuzzer's 25 s budget — of which 20 s is there with the matcher switched off, so the floor bounds a pre-existing cost rather than one long-distance matching introduced. Nothing is lost by the band, since every offset the matcher emits is bounded by the same window the parser's are and a kilobyte of window has no long distances in it. Separately, long-distance matching alongside a dictionary is refused, so the dictionary target asserts that boundary in both directions and then takes the mode off rather than spending half its iterations on an expected error. `the_matcher_changes_the_frame_it_runs_on` is what keeps the bit from going inert: a mode that resolved to parameters and never ran would pass every round trip.
- The narrow-window cost above is worth its own line, because it is not the matcher's and it is close to failing CI on its own. `btultra2` streaming a 128 KiB body at a `window_log` of 10 takes 20 s in a fuzz build against a 25 s timeout. The cost is roughly flat per block rather than per byte, so what drives it is the block *count* a narrow window forces — a block is capped at the window, so 10 turns that body into 128 blocks. It is not a property of the strategy: the slow-unit artifact libFuzzer already held for the streaming target is level 4 at the same window with a 1.5 MiB body, which is 1500 blocks. Nothing here has measured it in a release build, where the same input should be some thirty times cheaper.
- The auto-enable boundary, which is **not** a level sweep. Level 22 at pledged sizes 2^26 and 2^26 + 1; level 16 with `window_log` overridden to 26 and to 27, above 2^26. This depends on Phase 1's override surface.
- A dedicated corpus case. It needs to be larger than 64 MiB to trigger the auto rule, and its long matches must be separated by **less than** 128 MiB, since LDM pins the window there. The first draft specified separations above 128 MiB, which LDM cannot reach and which would have tested nothing. Gate it behind the long-running-test feature.
- [x] Long-distance matching in a stream: `long_distance_matching_streams_at_upstreams_size` sweeps four corpora against all nine parsers with the window pinned narrow enough that a two-megabyte frame compacts three times, and `long_distance_matching_survives_streaming_buffer_compaction` is the unit-level version with the one-shot encoder as its control. Both halves of the assertion matter — at this window long-distance matching is often a *loss*, so the frame must change exactly where upstream's does, not merely fail to grow.

### Acceptance

Byte-identical to upstream with LDM explicitly enabled across the swept parameters and both window behaviours, and the auto rule reproduces upstream's default output at level 22 above 64 MiB. There is nothing to reproduce at levels 16 through 21.

### Risks

Largest correctness surface of the five. LDM interacts with dictionaries (the LDM window includes the dictionary), with streaming compaction, and with repcodes. Land it disabled-by-default first, prove parity on the explicit path, then turn on the auto rule as a single reviewable change with a single expected diff.

---

## Phase 4: Multithreaded compression

### Goal

Compress one frame across N worker threads, producing a valid ordinary frame, with a bounded ratio cost against the single-threaded path.

**Byte parity with upstream is not a goal here.** See the policy section above. This frees the design from reproducing ZSTDMT's job layout, its per-job parameter juggling, and its emit-then-discard frame headers, and it makes the far more useful `workers = 1` invariant available.

### Design

Split input into jobs. Each job compresses independently but is seeded with the last `overlap` bytes of the previous job, which is what keeps the ratio loss small. Blocks from all jobs concatenate into one frame with one header.

Upstream's sizing is a reasonable starting point and worth transcribing correctly even though it is not binding. From `zstdmt_compress.c`:

```
/* job size, :1184-1196 */
jobLog = ldmEnabled ? MAX(21, ZSTD_cycleLog(chainLog, strategy) + 3)
                    : MAX(20, windowLog + 2);
jobLog = MIN(jobLog, ZSTDMT_JOBLOG_MAX);        /* 30 on 64-bit */

/* overlap, :1226-1243 */
overlapRLog = 9 - overlapLog(overlapLog, strategy);
ovLog = (overlapRLog >= 8) ? 0 : (windowLog - overlapRLog);
if (ldmEnabled) ovLog = MIN(windowLog, jobLog - 2) - overlapRLog;
overlap = (ovLog == 0) ? 0 : 1 << ovLog;

/* :1309 */
targetSectionSize = MAX(targetSectionSize, targetPrefixSize);
```

Three details the first draft got wrong and that are worth keeping even as a starting point: the `MAX(20, …)` floor and the `JOBLOG_MAX` cap on job size; the LDM branch in *both* formulas; and that `overlapLog <= 1` yields overlap **zero**, not `windowSize >> 8`. The `[ZSTDMT_JOBSIZE_MIN, ZSTDMT_JOBSIZE_MAX]` clamp is a different mechanism, applying only to a caller-supplied `jobSize` (`:1266-1267`, guarded by `jobSize != 0`), so the default is never raised to the minimum. Constants: `JOBSIZE_MIN` 512 KB, `JOBSIZE_MAX` 1024 MB, `JOBLOG_MAX` 30, `NBWORKERS_MAX` 256 (`zstdmt_compress.h:29-36`). `ZSTD_OVERLAPLOG_MIN/MAX` are at `zstd.h:1284-1285`.

#### The right hook

Not `PrefixedBlockMatchState`. Its constructor hardcodes `PrefixMatchMode::ExtDict` (`src/window/mod.rs:267`), which is dictionary semantics with the prefix in a separate buffer. A job's overlap is *contiguous* with its source; upstream copies the overlap into the job's own buffer precisely so that `ZSTD_window_hasExtDict` stays false (`zstdmt_compress.c:801`).

The structure that already models "contiguous history in one buffer with an encode start offset" is `ContiguousBlockMatchState`, driven the way `StreamingEncoder::encode_buffered_block` drives it with `block_start` / `block_end` (`src/streaming.rs:374-450`).

#### Per-job state

A job is not "that call plus the block loop". `StreamingEncoder` carries state across blocks that must not carry across job boundaries, since jobs are compressed concurrently and independently: `savings` (`src/streaming.rs:62`), `literals_state`, `sequence_tables`, and `repeat_offsets`.

Repcodes are the one that is a correctness requirement rather than a tuning choice. Upstream calls `ZSTD_invalidateRepCodes` on every non-first job (`zstdmt_compress.c:757`) because a repcode refers to an offset the decoder will not have in the same state. Carrying them across a job boundary produces a corrupt frame, not merely a worse one.

Serial by nature, and not parallelizable: the XXH64 checksum, which must see input in order; the LDM table, if Phase 3 has landed; and output ordering, since job *k* must be written before job *k+1* regardless of completion order.

#### Threading

`std::thread::scope` with a bounded worker pool and an ordered completion buffer. No new dependency; `rayon` would put a thread pool in the caller's process implicitly.

Gate behind a `multithread` cargo feature, default off. Threads are the one item here that `wasm32-unknown-unknown` cannot do, and the feature gate keeps the wasm CI check honest.

#### Determinism

Output depends on `jobSize` and `overlapLog`, which set the boundaries. It does **not** depend on worker count: measured on upstream at level 5 over 8 MiB, `nbWorkers` 1 through 8 all produce identical bytes. Tests must pin job size and overlap; worker count can vary freely and is the natural thing to sweep.

### Work breakdown

- [ ] Add the `multithread` feature and the `workers`, `job_size`, `overlap_log` parameters to the Phase 1 surface.
- [ ] Job partitioning and overlap sizing.
- [ ] Worker pool with ordered output collection and bounded memory: workers must block rather than run arbitrarily far ahead. The bound is roughly `compress_bound(job_size)` per in-flight job (compare `zstdmt_compress.c:1312`).
- [ ] Per-job state reset, with repcode invalidation as a correctness requirement.
- [ ] Serial checksum, and serial LDM state feeding the jobs if Phase 3 has landed.
- [ ] Error propagation: a failing or panicking worker surfaces as an `Error` on the calling thread, not a poisoned mutex or a hang.
- [ ] Wire into the one-shot encoder; decide explicitly whether streaming gets it in the first cut.
- [ ] Confirm `compress_bound` still bounds the output, since block count changes with job layout.

### Tests

- **`workers = 1` is byte-identical to the single-threaded path.** Available because byte parity with upstream is not a goal, and it should be a hard gate.
- Round-trip at every worker count from 1 to 8, across corpora larger than one job, with job size and overlap pinned.
- Upstream decodes this crate's multithreaded output, at several worker counts. This is the interop criterion that replaces byte parity.
- Ratio at N workers stays within a stated tolerance of the single-threaded path on the benchmark corpora. Pick the tolerance from a first measurement and then hold it.
- Input below the single-job threshold takes the single-threaded path. The threshold is this crate's choice; for reference, upstream's is `pledgedSrcSize <= 512 KB` regardless of job size (`zstd_compress.c:6392`), which is why its 1 MiB inputs still go through ZSTDMT.
- A worker failure surfaces as an error, tested by injecting one.
- Miri cannot exercise this usefully. Add a `loom` or hand-written stress test for the ordering buffer, or document the gap explicitly.

### Acceptance

`workers = 1` byte-identical to single-threaded; upstream decodes multithreaded output at every tested worker count; ratio within the stated tolerance of single-threaded; and a `BENCHMARKS.md` throughput row showing scaling.

Note that the interop helper cannot currently drive upstream's own MT path at all: `src/support/upstream_zstd.rs:2871` excludes `zstdmt_compress.c` from its sources, and `HELPER_CFLAGS` (`:2776-2781`) sets no `-DZSTD_MULTITHREAD` or `-pthread`, so `ZSTD_c_nbWorkers` returns "Unsupported parameter". Under this plan that only blocks *comparative* benchmarking, not acceptance, because upstream decoding our output is single-threaded and works with the helper as built. Rebuilding the helper with MT is therefore optional, and if taken up it has to be reconciled with `helper.c` including `zstd_compress.c` and `zstd_lazy.c` directly, which is why they are excluded from the source list.

### Risks

Concurrency bugs that appear only under load. The `workers = 1` gate catches partitioning logic but not races. Bounded memory is the second risk: an unbounded completion buffer turns a slow consumer into unbounded allocation, which matters because this crate treats adversarial input as in scope.

---

## Phase 5: Seekable format

### Goal

Support upstream's seekable format so a subrange can be decompressed without touching the rest, interoperably with upstream's tools.

### Design

Layered entirely on the existing codec. From `contrib/seekable_format/zstd_seekable_compression_format.md`:

- A sequence of independently compressed frames.
- A final skippable frame, magic `0x184D2A5E`, holding the seek table.
- Entries: `Compressed_Size: u32`, `Decompressed_Size: u32`, optional `Checksum: u32` (8 to 12 bytes each).
- Footer, 9 bytes: `Number_Of_Frames: u32`, `Seek_Table_Descriptor: u8`, `Seekable_Magic_Number: u32` = `0x8F92EAB1`. Descriptor bit 7 is `Checksum_Flag`, bits 6 to 2 are reserved and must be zero, bits 1 to 0 must not be interpreted.

Upstream ships this in `contrib/`, not `libzstd`, which argues for a `seekable` cargo feature rather than the default surface.

**Dictionaries.** Upstream's seekable API has no dictionary entry point: `ZSTD_seekable_initCStream(zcs, compressionLevel, checksumFlag, maxFrameSize)` is the entire compression surface, and the decompressor's three init functions take none either. The format spec lists inline dictionaries as a *future* use of the reserved bits. So dictionary-compressed seekable output is readable by this crate and not by upstream's tools.

That is worth supporting anyway, because per-frame startup cost is exactly what a seekable format pays most and a shared dictionary is what recovers it. But it must be opt-in and documented as a non-interoperable extension, and the default path must stay readable by upstream. The first draft had it as an unqualified goal alongside an unqualified interop criterion; those two cannot both hold.

**Frame sizing.** Upstream sets `ZSTD_c_srcSizeHint` to `maxFrameSize` on every frame (`zstdseek_compress.c:234`), which selects a cparams tier appropriate to the frame rather than to an unknown stream. With byte parity out of scope this is a ratio item rather than a parity item, but it is free once Phase 1 lands the pledged size, and skipping it means every frame compresses as if its size were unknown.

### Work breakdown

- [ ] `src/seekable.rs` behind a `seekable` feature.
- [ ] `SeekableEncoder` wrapping `StreamingEncoder`, with a configurable frame size and an explicit frame-boundary call.
- [ ] Per-frame pledged size from Phase 1.
- [ ] Seek table writing, including the optional per-frame checksum.
- [ ] Seek table parsing with strict validation: reserved descriptor bits zero, entry count against frame size, and sizes against actual input length. The sum is over *compressed* sizes plus the seek-table frame itself, and it must tolerate interleaved skippable frames, which the format permits as entries with `Decompressed_Size = 0`.
- [ ] Enforce `ZSTD_SEEKABLE_MAX_FRAME_DECOMPRESSED_SIZE` (`0x40000000`, `zstd_seekable.h:19`) as the per-frame bound.
- [ ] `SeekableDecoder` with `decompress_range(offset, len)` and frame-index lookup.
- [ ] Optional dictionary support, documented as not readable by upstream's tools.

### Tests

- Interop against the upstream `contrib` tool in both directions, on the default (dictionary-free) path.
- Random-range extraction against a reference full decompression, property-tested.
- Malformed tables: truncated footer, wrong magic, overflowing entry count, sizes exceeding the file, reserved descriptor bits set, a frame above the decompressed-size cap.
- Interleaved skippable frames round-trip and are skipped correctly during seeks.
- A fuzz target for seek table parsing.

### Acceptance

Upstream's seekable tools read this crate's dictionary-free output and vice versa; every malformed-table case errors rather than panicking or over-reading; and range extraction matches full decompression on random ranges.

### Risks

Low for the codec, moderate for parsing. The seek table is fully attacker-controlled and drives allocation and indexing, so it belongs in the threat model and in `fuzz/` from the first commit.

---

## Upstream's streaming beats upstream's one-shot on `binary-structured` — **investigated, no change**

Recorded during the long-distance matching work as *"`binary-structured` at `Fast` with a window of 19 streams 27.8% above upstream's streaming frame"*, and carried since as a gap in this crate. It is not one. What the number measures is a divergence inside upstream.

Confining it took four measurements, each cheap and each ruling out a family of causes:

1. **Our one-shot is byte-identical to upstream's one-shot** at window logs 16 through 27, `Fast` and `DoubleFast`. So the parser is not the difference.
2. **Our streaming is byte-identical to our own one-shot** on the same corpus, to within the one byte of frame framing. So our streaming is not losing anything either.
3. **Upstream's streaming differs from upstream's own one-shot**: 319137 against 407789 at window 19, on identical input. Both frames declare the same window, both decode here, and this crate's decoder validates every offset against the declared window, so upstream is not reaching outside it — it is finding better matches inside it.
4. **Piece size is irrelevant.** Driving upstream's streaming API with the whole two megabytes in a single push gives the same 319137 as thirty-two-kilobyte pieces. So it is not the chunking, and it is not what the caller does.

The applied compression parameters are identical on both upstream paths (window 19, chain 18, hash 19, search 3, min-match 5, target-length 2, `Fast`), which `trace-advanced-streaming-cparams` was added to check rather than assume. Block by block, upstream's two frames are identical for five blocks and then upstream's streaming drops from about 25.5 KB per block to about 17.4 KB. Five blocks is 640 KiB, which is exactly `windowSize + blockSize` — the size of upstream's internal streaming input buffer. Sweeping the input size confirms it: at each window log the two upstream frames agree while the whole stream fits that buffer and diverge once it does not.

Past that point upstream's circular buffer wraps, `ZSTD_window_update` turns the earlier contents into an ext-dict, and upstream switches from `ZSTD_compressBlock_fast_noDict_generic` to `ZSTD_compressBlock_fast_extDict_generic`. The two are separate implementations of the same parser and they do not agree on this corpus. Neither reaches further than the other — the floors resolve to the same `blockEnd - maxDist` once a window has gone by — so the difference is in how they search, not in what they are allowed to see.

Matching it would mean reproducing upstream's buffer geometry: emulating a wrap this crate does not have, and running an ext-dict parser over history that is contiguous. That is a deliberate design change rather than a fix, and the opportunity is narrow — every other corpus at `Fast` and every strategy above it already match upstream's streaming frame at this window. Left alone, recorded here.

---

## Streaming compaction and the cycle-indexed tables — **landed**

The streaming encoder physically drops bytes off the front of its history buffer, so every position its match finders hold has to move with them. Hash tables are indexed by the bytes at each position and those bytes do not move, so subtracting the shift from each entry is enough. Chain and binary-tree finders hold a *second* table indexed by `position & mask`, and subtracting alone leaves every entry in a slot that no longer names it. Both were therefore cleared and rebuilt.

For the chain that is merely wasteful. For the binary tree it is not a rebuild at all: `ZSTD_updateDUBT` files positions *unsorted*, and only a search sorts them, only as deep as `search_depth` reaches. Re-inserting a whole retained window in one go leaves most of each bucket's chain unsorted and unreachable, so the tree the parser gets back is worse than the one it had. Measured against the one-shot encoder on identical parameters, this cost **321.7%** at level 12 and **176.3%** at level 5 on a planted-repeat body, and **12.2%** on `tabular-csv` at `btlazy2` with a window of 19. It fails quietly: the frame still decodes, it is just much larger.

C does not have the problem because it never moves bytes. It slides a base pointer and lets indices grow, correcting only on approach to 2^31 — and it chooses that correction to be cycle-aligned on purpose, `ZSTD_window_correctOverflow` composing it from `curr & cycleMask` plus whole cycles so that *"the least significant cycleLog bits of the indices must remain the same"* (`zstd_compress_internal.h:1154`). The fix is to make our drop satisfy the same property. A cycle-indexed table now survives in either of two circumstances, at opposite extremes:

- **Cycle narrower than the drop.** Round the drop down to a whole number of cycles. Costs nothing: the buffer already holds more than the window between compactions, and the parser's reach is bounded by `max_history_bytes` rather than by how much the buffer happens to hold.
- **Cycle wider than the whole buffer.** Nothing wraps, `position & mask` is the position, and the table is a plain array that shifts bodily. Alignment stops being a constraint because there is no wrapping left for it to protect.

Between the two the buffer is widened to hold one cycle, which is bounded because the band is: at most three windows and two blocks, where it was two windows and a block. Widening *above* the band would be unbounded, since there it is the cycle that is large.

A cycle wider than the window is ordinary rather than exotic, which is why the second case is needed at all. C only shrinks the chain log to fit the window when the source size is known (`zstd_compress.c:1577-1583`), and a stream by definition does not know it. Level 12 against a window of 19 runs a two-megabyte cycle against half a megabyte of history.

`streaming_compaction_keeps_the_match_state_at_every_strategy` covers all three cases against the one-shot control, and each of the three ablations fails on exactly the case it owns. The cycle length now has one definition, `MatchFinderParameters::rebase_period`, reached through `chain_cycle_log` and `binary_tree_cycle_log`, so the buffer sizing and the finders cannot drift apart.

Two things this measured and did not close:

- **Compaction still costs the hash finders a little.** Level 12 at `DoubleFast` gives up 1.60% against one-shot and `Fast` 0.39%, on a body short enough not to compact both are within 0.12%, and neither finder was changed by this work — they rebase every entry they hold either way. The `streaming_compaction_keeps_the_match_state_at_every_strategy` bound is set at 2% by that 1.60% row, so it catches the tens-of-percent failures this section is about and nothing finer.
- **`btlazy2` with long-distance matching gives up 2.1-4.5% streaming** where every other pairing is within 0.2%. About a point of that is present without any compaction at all, so roughly 2-3.5% is compaction's. `btlazy2` is the one strategy that takes the non-optimal long-distance path, laying matches down and parsing the gaps.

---

## Standalone items

~~**Remove the dead `UnsupportedFeature` variants.**~~ Done, before `0.1.0` and so at no cost. All four were constructed nowhere in `src/`, `tests/`, `fuzz/`, `benches/` or `examples/`, and neither was `Error::Unsupported` itself; they advertised limitations that no longer exist. It was two removals rather than one, as the note here said: with the variants gone the enum was uninhabited, so `Error::Unsupported(UnsupportedFeature)` went with it, along with the `UnsupportedFeature` re-export from `src/lib.rs`, its row in the README's API table, and the `#[non_exhaustive]` clause in `docs/SEMVER.md` that named it.

~~**Magicless frames.**~~ Landed with Phase 1, both halves: `EncoderOptions::format`, `DecoderOptions::format`, and `parse_frame_header_with_format`.

~~**Frame-size query helpers.**~~ Done, before `0.1.0`, as `find_frame_compressed_size`, `decompress_bound` and `decompressed_size`, each with a `_with_format` sibling. All three share one walk that reads the frame header and then each block header in turn, skipping the payload each names.

Three things are worth keeping from doing it.

**The walk validates less than it could, deliberately.** A block wider than the frame's declared maximum, a checksum that will not match, literals that will not parse: all of that is the decoder's to reject, and rejecting it here as well would leave this function and the decoder each holding half of one rule. What it does reject is only what stops the walk: a reserved block type, and input that runs out. That is also what upstream's `ZSTD_findFrameSizeInfo` does.

**The bound for an undeclared frame is `blocks * block_size_max`**, upstream's rule. It is loose by construction and is the answer to a question `decompressed_size` cannot answer at all, which is why both exist rather than one returning an `Option` and the caller guessing. A skippable frame declares `Some(0)` rather than nothing: it produces no output, and that is known rather than missing, so a stream carrying one is not turned undeclared by it.

**Neither number is a safety bound.** Four bytes of RLE block encode 128 KiB of output, so a small hostile input carries an enormous honest-looking bound, and a caller who allocates it has been handed the bomb rather than warned about it. Both doc comments say so and point at `DecoderOptions::max_output_size`, which is enforced as the decode runs rather than trusted before it starts. `docs/THREAT_MODEL.md` already covers the underlying vector; the new surface adds no parsing that the decoder did not already do.

Correctness rests on the boundary being directly checkable rather than on an oracle: `find_frame_compressed_size_lands_on_every_frame_boundary` concatenates nine differently shaped frames — compressed, raw, RLE, single-segment, checksummed, empty, dictionary-id, skippable — walks them, and decodes each slice the walk cuts. A wrong answer cannot pass, because the slice would not decode. `find_frame_compressed_size_reports_truncation_rather_than_a_short_frame` cuts every one of those nine at six positions, since answering from a truncated frame is worse than failing: the caller would slice at the wrong place.

~~**A caller-owned buffer for streamed output.**~~ Done, before `0.1.0`. `StreamingEncoder` had only `take_output`, which `mem::take`s the buffer; `StreamingDecoder` has had `read(&mut [u8])` since it was written. The asymmetry cost an allocation per block on `io::Writer`, which is the crate's most-used entry point: `drain` took the buffer, handed it to `write_all`, and left the encoder to grow a fresh one for the next block.

`read`, `pending_output` and `consume_output` now cover the `ZSTD_outBuffer` shape, over an `output_pos` cursor and a compaction rule that only shifts when the drained prefix is at least half the buffer, so a caller reading in small pieces cannot provoke worse than amortized O(1) per byte. `take_output` keeps its old semantics, including handing the allocation over, because a caller who wants the `Vec` should still get it without a copy.

Two notes. **The measurement is in `tests/allocation.rs`, not an argument**: a warm `io::Writer` over incompressible input allocates *nothing* across four megabytes, and reverting `drain` to `take_output` reports a 4 MiB allocation at the same assertion. Both new tests were confirmed against the reverted code before being kept, and both needed *incompressible* input to discriminate — a compressible corpus produces a few kilobytes per block, small enough that the regrowth slips under any ceiling worth setting. **`consume_output` panics rather than clamping** on an over-consume, because clamping would drop compressed bytes and leave a truncated frame that still parses as a frame.

`RECOMMENDED_INPUT_SIZE` and `RECOMMENDED_OUTPUT_SIZE` landed with it on both streaming types, upstream's `ZSTD_CStreamInSize` and its three siblings. The encoder's output constant is `compress_bound` of one block expressed as a `const`, sharing `FRAME_HEADER_MAX` and `CHECKSUM_SIZE` with the function so the two cannot drift; `the_recommended_output_size_takes_a_whole_block_in_one_read` asserts no `read` ever fills it exactly, on incompressible input where every block falls back to raw.

~~**Decode into a caller-owned slice.**~~ Done, before `0.1.0`, as `decode_into_slice` and three `Decoder` methods. The encode side had `encode_into_slice` and `compress_bound`; the decode side had `Decoder::decode_all_into(&mut Vec<u8>)` and nothing that wrote into a `&mut [u8]`, so a caller whose destination is an arena, an FFI buffer, an mmap or a stack array had no entry point at all, which is `ZSTD_decompress`'s ordinary shape.

**Both obstacles this entry recorded were real; one of them was mis-scoped, and the correction is the useful part.** The plan said a fixed destination needed a *monomorphized* sink threaded through roughly fifteen functions in `src/sequence.rs` and eight in `src/decode.rs`, on the grounds that an enum would put a branch in the innermost copy loop. That was wrong, and reading the loop rather than reasoning about it is what showed why: **the sequence executor does not write through the sink at all.** It takes the base pointer and the length once per block, runs the whole wildcopy loop on raw pointers, and hands the length back at the end. The variant is matched a handful of times per block, never per sequence. So `DecodeOut` is a plain two-variant enum in `src/decode_out.rs`. (The executor *is* monomorphized in the end, but on a different question and for a different reason; see below.)

**The second obstacle was as recorded.** `copy_match_inline` overshoots a match by up to 31 bytes, the `WILDCOPY_OVERLENGTH` that `reserve_block_output` reserves once per block. Against a `Vec` that slack is spare capacity; against a caller's exact-sized slice it is a write past the end. The fix is upstream's: `execute_sequence_exact` is this crate's `ZSTD_execSequenceEnd`, a byte-exact copy for the sequences that would otherwise overshoot past the end. A growable destination cannot reach it, because the per-block reservation already covers the whole block, and a `debug_assert` says so.

**What it cost, which is the part worth reading.** The obvious shape is a per-sequence comparison, which is also what upstream does: `ZSTD_execSequence` tests `oMatchEnd > oend_w` on every sequence. Measured here, it cost **1 to 1.5%** on the sequence-dense corpora (`log-lines`, `tabular-csv`, `json-records`), consistent across eight interleaved runs of prebuilt binaries. Two things had to change to get that back.

First, `wildcopy_end` has to be loop-invariant. The first version recomputed it in the cold branches, which made it a mutable local live across every iteration of a loop whose register pressure the `#[cold]` outlining already exists to manage; that alone was several percent. It is a `let` read once per block now, and it stays correct because neither destination can invalidate it: a fixed one cannot move or resize, and a growable one can only be reallocated upward past a bound the block reservation already covers, so a stale value is a bound that is too low on a path that was never going to reach it.

Second, **the decision belongs to the block, not the sequence.** `reserve_block_output` has just given a growable destination slack for the whole block, and a fixed destination has it for every block but the last one or two of a decode sized to the byte. So `decode_and_execute_sequences_unified` settles it once and dispatches into `execute_block_sequences<const CHECKED: bool>`. The `false` instantiation is the loop as it was, with the comparison folded away and `execute_sequence_exact` unreachable; the `true` one runs for the tail. The cost is one more monomorphization of the largest function in the decoder.

**That came out ahead of where it started.** Against the previous tip, the final shape measures about **5% faster** on `json-records`, `log-lines`, `tabular-csv` and `binary-structured`, over five interleaved runs with no overlap between the two distributions. Roughly a fifth of that is separable and had nothing to do with this feature: splitting the executor out meant `decode_setup` stopped returning the `BitDStream` through an `#[inline(never)]` boundary and the executor constructs it itself, which is worth about 1% on its own. The rest came with the split.

**Every figure in the two paragraphs above was measured on a machine that was not idle**, by alternating prebuilt binaries so that whatever else was running hit both arms equally. The directions held on every round; the magnitudes are approximate and should not be quoted as if they came off a quiet box. `BENCHMARKS.md` is the place for a real number, and it is stale by more than this already.

**Exactly the decompressed size is enough**, which is the whole point of taking the harder of the two roads this entry laid out: no padding requirement in the signature and nothing for a C caller to trip over.

Acceptance, as this entry specified it: byte-equality with `decode_all` across the frame zoo; `Error::DstSizeTooSmall` at every size below the exact one; `tests/allocation.rs` measuring a warm slice decode at **zero** allocations, which is stronger than the encode side manages; a `slice_decode` fuzz target with the growable decode as its oracle; and the throughput run above. Three additions the entry did not anticipate, each of which earned its place under injection:

- **Miri, on a destination sized to the byte.** `tests/miri.rs` gained two cases, and they are the ones that settle the question: forcing the executor to skip the byte-exact path makes Miri abort inside `decode_into_slice`, because it knows where the slice ends whatever the bytes past it happen to be.

- **Guard bytes past the destination.** No assertion on the *output* can see this bug, because the overshoot lands after the output and the output is correct either way. `decode_into_guarded_slice` puts 64 sentinel bytes past `dst` and checks them on every call, on the failures as well as the successes.
- **A corpus that ends in a match of every length.** The overshoot is worst when the output ends in a match, and how far it runs is decided by the match length modulo 32. A generic corpus does not reliably end in a match at all. `a_match_near_the_end_does_not_overshoot_the_destination` builds one that does and sweeps both the match length and its distance from the end. Loosening the bound by one byte does not fail it and should not, because 32 is one conservative byte over the true worst case; loosening it by two fails it on exactly one guard byte.

**Reconsider the one-shot window limit.** zstandard applies `max_window_size` in the one-shot path (`src/decode.rs:572`, `:725`) as well as streaming (`src/streaming.rs:968`). Upstream exempts one-pass decoders: *"The limit does not apply for one-pass decoders (such as `ZSTD_decompress()`), since no additional memory is allocated"* (`zstd.h:1287-1291`). Being stricter than upstream here may well be right, since this crate does allocate on that path, but it is a deliberate divergence that currently is not recorded as one. Decide it and document it either way.

~~**Split `PreparedDictionary` toward `CDict` / `DDict`.**~~ Done, before `0.1.0`, as `EncoderDictionary` and `DecoderDictionary`.

The complaint recorded here was about the name. Measuring first found a larger reason: every parsed dictionary carried **33,640 bytes** of tables and every caller carried both halves, so an encode-only caller held 22,544 bytes of decoding tables it could not reach (67%) and a decode-only caller held 11,034 bytes of encoding tables (33%). The tables were inline `Option` fields, so building only one half would have saved parse time and no memory at all; the split had to move them into separate structs to buy anything, and they are now `Arc`-held, which also makes `Clone` cheap where the doc had claimed it already was and it was a 33.6 KiB memcpy.

Two points worth keeping. **The call sites cost almost nothing**: 24 public functions take a dictionary and each already knew its direction, so the split renamed a parameter type per site and added no new functions. And **the direction of future flexibility runs one way**, which is what decided it over making the fused type lazy: from two types a unified convenience wrapper can be added later without breaking anyone, whereas a fused type cannot be un-fused after publication.

Validation is unchanged in either direction, and the route to being sure of that is worth recording because the first attempt was wrong. A check was extracted from `build_dtable` and hoisted ahead of the direction branch, on the reasoning that the encoding path never calls `build_dtable` and would otherwise validate less. Removing it again moved **no** test, because `fse::read_ncount` already bounds the symbol value and the table log and runs in both arms; the hoist was inert and has been reverted. What is not symmetric is the builders: `build_ctable` rejects a normalized count below `-1` and `build_dtable` has no equivalent check. Nothing was found that reaches it, over 200,000 random formatted dictionaries and 400,000 mutations of a real one, with zero disagreements — so it is held by `the_dictionary_directions_agree_on_what_parses` in `tests/property.rs` rather than by argument. That test generates by damaging a real dictionary rather than by generating one, because random bytes behind the dictionary magic parse only 1% of the time and would leave proptest's default 64 cases comparing two rejections; damaged real ones parse 49.2%, which `the_damaged_dictionary_generator_still_produces_parseable_dictionaries` pins. `the_two_directions_agree_on_where_content_starts` separately replaces the `debug_assert_eq!` that used to compare the two Huffman sizes inside a single parse.

**Cover and legacy dictionary trainers.** Only fastCover exists. Low priority: fastCover is what upstream's CLI uses by default, and trained dictionaries already land within about a percent of upstream's.

---

## Cross-cutting concerns

**The interop helper.** `src/support/upstream_zstd.rs` drives a helper built against the pinned checkout. Phases 1, 2 and 3 all need it to express settings it currently cannot: negative levels, explicit `ZSTD_c_*` values, LDM parameters. Make that one change early rather than three incrementally, and keep the helper singular; there have historically been two helper binaries with different mode sets in this workspace, and mixing them up fails with an empty error message. Phase 4 does *not* need helper changes under this plan, since its acceptance is interop rather than byte parity.

**The benchmark report.** `BENCHMARKS.md` reports levels 1 through 22 over 11 corpora at 4 MiB each. Phase 2 widens the level axis; Phase 3 needs a corpus above 64 MiB; Phase 4 adds a worker axis and a ratio-versus-single-threaded column. Add these as opt-in sweeps rather than growing the default matrix, which already takes about ten minutes and needs a quiet machine for its throughput columns.

**Feature flags.** Two new, both default off: `multithread` and `seekable`. The wasm32 CI check must keep running with both off, since that configuration is what makes the "builds anywhere Cargo does" claim true.

**Threat model.** `docs/THREAT_MODEL.md` needs two updates. A `window_log` override above 27 is a memory-exhaustion vector that the current 128 MiB decoder default blocks, and the interaction should be stated; this is Phase 1's item, not LDM's, since LDM caps its own window at 27. And the seekable seek table is a new attacker-controlled parsing surface needing the same treatment the frame parser gets.

**Fuzzing.** Each phase adds targets: parameter overrides as an encode round-trip with a fuzzer-chosen parameter set, LDM as an encode round-trip, seekable table parsing as a decode target. The eight existing targets under `fuzz/fuzz_targets/` are the pattern.

---

## What this plan does not cover

- **Ratio and throughput parity.** Tracked separately; the open item is encode throughput at levels 3 and 4, around 80% of upstream.
- **Byte parity for multithreaded and seekable output.** A deliberate choice, argued in the policy section above.
- **`no_std`.** The crate requires `std` and nothing here changes that. It is a larger question than a feature.
- **Block-level sequence APIs** (`ZSTD_compressSequences`) and external sequence producers.
- **Legacy format support** (zstd v0.1 through v0.7). Upstream keeps it behind `ZSTD_LEGACY_SUPPORT` and defaults it off for recent versions.
- **A 1.0 API freeze.** Phase 1 settles the largest open API question, but freezing should follow the phases rather than precede them.
