# Correctness oracle plan

This plan covers **how `zstandard` knows it is correct** once byte parity with upstream `zstd` stops being the objective.

It exists because that decision has a bill attached, and the bill is not obvious. `docs/PARITY_PLAN.md` covers *feature* gaps against upstream and says byte parity is the gate for the core codec; that sentence is now out of date for the encoder's parse and encoding decisions, and reconciling it is item 6 below.

## What byte parity was doing

Two jobs, and it is easy to notice only one.

It said **"this is correct."** A frame identical to upstream's needs no further argument: the match finder, the price model, the entropy coders, the block splitter and the sequence encoding all agreed at once. Nothing else in the tree gives that much signal per line of test code.

It also said **"nothing changed by accident."** Every unintended output change failed a parity sweep for free, without anyone having to decide in advance which property to assert.

The first job has replacements. The second one currently has none.

## Why the replacement has to be layered

The obvious move — keep comparing to upstream, just more loosely — does not survive contact with the goal. Two of the five layers below reference upstream's *choices* and therefore decay as this crate deliberately improves on them; two reference nothing but the format and our own history, and are durable; one is the metric we are actually optimising.

| Layer | Guarantees | Depends on | Decays as we diverge? |
| --- | --- | --- | --- |
| **1. Legality** | Upstream decodes whatever we emit, from every store path | The *format*, not upstream's choices | **No** |
| **2. Structure** | The parse still agrees where the encoding does not | Upstream's parse | **Yes** — diagnostic only |
| **3. Quality** | We are never larger than upstream, per row | Upstream's *size*, one-directionally | No — this is the objective |
| **4. Change detection** | Our own output did not move unintentionally | Nothing but our own history | **No** |
| **5. Attribution** | A feature moves our frame where it moves upstream's | Upstream's *response* to a feature | Only if it keeps gating on baseline identity |

Layers 1, 3 and 4 are the durable core. Layer 2 is worth having as the fastest way to localise a defect, but it should be written as a recorded set that is expected to shrink, not as a gate that is expected to hold. Layer 5 survives only in its differential form.

## The work

| # | Item | Layer | Size | State |
| --- | --- | --- | --- | --- |
| 1 | Self-baseline of our own output | 4 | Medium | **Done** |
| 2 | Differential feature attribution | 5 | Medium | **Done** |
| 3 | Extend the size gate to the ungated grids | 3 | Small | **Done** for the long-distance grids; the streaming and override grids carry bounds rather than sets |
| 4 | Close the legality gaps | 1 | Small | **Done** |
| 5 | Generalise the structural comparator | 2 | Small | **Done** |
| 6 | Reconcile `PARITY_PLAN.md` | — | Small | **Done** |

Ordered by value, not dependency; only item 2 blocked anything (the `row-lazy-repcodes` branch), and it is done. All six are complete.

### 1. Self-baseline of our own output

**The gap.** Nothing notices an unintended output change. A refactor that quietly costs 2% compression is invisible until somebody regenerates a report and reads it carefully.

`BENCHMARKS.md` is not this and should not be mistaken for it: it is regenerated rather than asserted, it needs the upstream helper to produce, and it has already sat 20 commits stale without anything noticing.

**Design.** A checked-in table of our own compressed output — one row per (corpus, level, mode) — asserted by a test. Record **both** size and a hash of the frame: the size is what a human reasons about and makes the diff reviewable, the hash is what catches a change that happens to keep the size identical.

Regenerated behind an explicit environment variable so that updating it is a deliberate act that appears in review as a diff of *what changed and by how much*, never a silent rewrite.

The decisive property is that this layer **needs no upstream helper**. It can therefore be far broader than any parity sweep and can run in environments that have no C checkout at all.

**Acceptance.** Any change to encoder output fails the test with a readable per-row diff. A corpus or level added to the grid without a baseline fails rather than being skipped. The grid covers every level including negative ones, and one-shot, streaming, dictionary and long-distance modes.

### 2. Differential feature attribution

**The gap.** Four sweeps compare a feature's frame against upstream's *only on rows whose baseline already matches upstream byte for byte*, so that a difference is attributable to the feature rather than inherited. Correct discipline, and it silently loses rows as the base parser diverges on purpose: on `row-lazy-repcodes`, `asserted` falls 64 → 51 and `baseline_gaps` rises 16 → 30 on the long-distance sweep, and the strategy-override sweep drops 410 → 323 of 495 against an 82% floor.

Worse, the failure is disguised. The single entry in `LDM_KNOWN_DIVERGENCES` disappears from the actual set, which reads exactly like a divergence being fixed. It was not fixed; its baseline stopped matching, so the row was skipped before the comparison it was named for ever ran.

**Design.** Replace the identity gate with a comparison of each side against itself:

- enabling the feature changes our frame **iff** it changes upstream's (already asserted in `long_distance_matching_streams_at_upstreams_size`, and the sharpest of the three);
- our frame with the feature is no larger than upstream's with the feature;
- the feature's *relative* effect on us is at least its relative effect on upstream, within tolerance — this is what catches a feature that has quietly stopped doing anything, which a size bound alone cannot.

Then delete the baseline-identity gate and the `baseline_gaps` counter it feeds.

**Affected.** `long_distance_matching_is_byte_identical_to_upstream`, `long_distance_parameters_are_byte_identical_to_upstream`, `long_distance_matching_with_a_dictionary_is_byte_identical_to_upstream`, `overriding_the_strategy_leaves_the_levels_parameter_space`, `long_distance_matching_streams_at_upstreams_size`.

**Acceptance.** Every row in each grid is compared; no row is excluded for failing to match upstream at baseline. `row-lazy-repcodes` merges without any expectation list growing.

**Done 2026-08-06, and the third property was dropped on measurement.** Rearranged, "the feature costs us no more relatively than it costs upstream" *is* the parity-gated bound, and it fails on four rows -- `json-records` at strategies 3, 4 and 5, `wikipedia` at 6 -- in every one of which we are smaller than upstream with the feature on. It measures our baseline being better, not our feature being worse. The two that survive, engagement and never-larger, hold on all 81 rows of the long-distance sweep with and without the divergence, against 51 under the old gate.

That leaves one hole, named here so it is not rediscovered: our feature could get materially worse against our *own* baseline and still satisfy both, so long as it stayed under upstream's. That question belongs to layer 4, which records our own per-row output.

**What removing the gate found.** Rows nobody had ever compared, in either direction:

- the dictionary grid: **21 of 54 rows larger than upstream**, now in `LDM_DICTIONARY_SIZE_GAPS`. Sixteen are within 10 bytes; five are not, and four of those five are Fast.
- the parameter grid: two rows at **6.3% and 7.7% over upstream**, both `wikipedia` at btopt under hash-log clamping. These are the largest known size gaps in the long-distance surface. The six entries in the *old* byte-parity divergence set were, in every case, rows where this crate's frame was the **smaller** one.
- the strategy-override sweep: its per-row bound was 2% + 64 bytes, and the worst of its 36 over-sized rows is 48 bytes on 41 KB. Tightened tenfold to 0.2% + 64; the old slack would have absorbed a 1.9% regression on every row at once without a word.

### 3. Extend the size gate to the ungated grids

`KNOWN_UPSTREAM_SIZE_GAPS` fails when we are larger than upstream and stays quiet when we are smaller. That is exactly the right question under this policy, and it covers only the one-shot, no-override, no-dictionary grid. The dictionary, long-distance, streaming and parameter-override grids assert byte parity instead, which decays.

**Acceptance.** Every (corpus, level, mode) is under a one-directional size bound with a recorded, explained exception list.

### 4. Close the legality gaps

`upstream_decodes_frames_from_every_strategy_and_framing` and its dictionary twin cover one level per parser strategy, two window regimes, and three framings. Not yet covered: **negative levels**, which select a different cparams row entirely; degenerate inputs (empty, shorter than `MIN_MATCH`, single-byte); multi-frame and skippable-frame concatenations.

**Acceptance.** No encoder-reachable configuration is absent from a decodability sweep. When a divergence is added, this layer is widened to cover its path *before* the divergence lands.

**Done 2026-08-06.** Negative levels folded into `DECODABILITY_LEVELS` (`-131072`, `-7`, `-1`, taking the grid from 486 frames to 648, and the dictionary twin from 18 to 24), plus `upstream_decodes_degenerate_inputs_from_every_strategy` and `upstream_decodes_our_concatenated_and_skippable_frames`.

Each axis was validated by injecting a defect only it reaches, and the negative-level axis was validated *against a control*: an offset corruption gated on `fast_search_step > 2` fails three sweeps with the negative levels present and passes all four with them removed. Nothing in the tree had ever put an accelerated level through a conforming decoder.

Two claims in this plan were wrong and are corrected in the tests rather than left here:

- **The degenerate band does not reach the parsers.** Measured: below 7 bytes no planner is called; at 7 and 8 the fast planner returns down its short-input branch; at 64 it runs its main loop. In all three the plan is discarded for a raw block, so a defect injected into that branch is invisible to the degenerate sweep and surfaces on the corpus sweep's block tails instead. What the sweep covers is the raw/RLE decision and the small-content-size header forms.
- **The degenerate band was not uncovered.** `tests/codec.rs` has a dozen cases across it. Every one is a round trip through *our own* decoder, which cannot catch an encoder and decoder agreeing on the same wrong reading. The gap was differential, not existential, and the same is true of the skippable-frame half of the concatenation test.

Reaching the accelerated path at all took four attempts. Three injections were inert: two landed in `_inner_tracing` (integration tests set `trace_enabled` from `cfg!(test)`, which is false for the *library* under an integration test, so the `_no_trace` copy ships there), and one lowered a window floor that no stale table entry sat below. See [[project_cfg_test_tracing_default]].

### 5. Generalise the structural comparator

Byte-identical literals prove the match/literal partition agrees, which pins match positions and lengths without asserting anything about how offsets were coded. Equal sequence counts and modes narrow it further. Together they localise a defect in one step, and they survive an encoding-only divergence like the repcode substitution.

The pattern exists in exactly one place — `no_dict_benchmark_first_block_sections_match_upstream`, on the `row-lazy-repcodes` branch — and covers two corpora at three levels, first block only.

**Written as a recorded set, not a gate.** As the parse itself improves past upstream's, rows will legitimately leave this comparison. That is success, not regression, and the test should be shaped so that it reads that way.

**Done 2026-08-06** as `tests/structure.rs`: 9 no-dictionary corpora x 9 levels, one per parser strategy, classified into an ordered `Agreement` -- `Identical`, `SameParseDifferentEncoding`, `DifferentParse`, `NoCompressedBlock`. A row may strengthen or weaken only by editing the record, and *both* directions fail, so an improvement is reviewed rather than absorbed.

Measured: **62 of 81 rows byte-identical** at block granularity. The five `same-parse` rows are all level 13 and are one divergence, btlazy2's repcode substitution; identical literals and equal sequence counts across five corpora is what says that, rather than five separate parse defects. Nine rows are `pseudorandom`, where both sides emit a raw block and there is no parse to compare -- listed row by row rather than special-cased on the corpus name, so a level that started finding a compressed block would surface as a change.

Two design points worth keeping:

- It is an **integration** test, not a `#[cfg(test)]` one in `src/encode.rs` where its ancestor lived. `trace_enabled` comes from `cfg!(test)`, so a unit test compares the tracing planner copies and an integration test compares the ones that ship. See [[project_integration_tests_run_the_no_trace_copy]].
- `NoCompressedBlock` is a class rather than a `continue`. The section parser panics on a raw block, and skipping those rows is precisely the coverage bleed that item 2 was written to undo.

It also found three frames larger than upstream's, all under 0.03% and now in `FIRST_BLOCK_SIZE_GAPS`. The split between them is the informative part: `tabular-csv` at 22 diverges in the first block, so the comparator can see it, while the two `mixed-entropy` rows have a byte-identical first block and lose their 9 bytes somewhere later. That is the honest limit of a first-block comparator, and why the size bound is taken on the whole frame.

### 6. Reconcile `PARITY_PLAN.md`

It states that byte parity is the acceptance gate for the core codec, "because it is the only check strong enough to catch a heuristic applied in the wrong order, and because this crate's whole ratio story rests on it." The first clause is still true and is why layer 2 is worth keeping as a diagnostic. The second is no longer how the ratio story works. Rewrite that section to point here.

**Done 2026-08-06.** The section carries a superseded notice naming this document, the second clause is corrected to point at layers 3 and 4, and the table's **Parity gate** column is annotated: it reads as written for items already landed, which were accepted under it, and means these layers for anything still open.
