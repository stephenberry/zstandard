# Benchmark Report

This report compares the local `zstandard` checkout against the official `zstd` implementation at the revision pinned in `upstream-zstd.ref`. Numbers measured against any other revision are not comparable: upstream changes its level mapping, parser heuristics, and block splitter between releases.

Notes:
- Rust encode is benchmarked for the currently supported public compression levels `1..=22`.
- Upstream encode is benchmarked for levels `1..=22`.
- Rust decode and upstream decode are benchmarked on the same upstream-produced frame for each row.
- Dictionary cases use the deterministic raw and trained dictionary fixtures emitted by the upstream helper.
- Throughput is machine-local and depends on the current host; use this file for relative comparison, not cross-machine claims.
- Each throughput number is the fastest of 3 timing trials, since background load can only make a trial slower. Ratios are exact byte counts and reproduce exactly; throughput does not. The two sides of each comparison are timed seconds apart inside a ten-minute sweep, so a case's throughput carries drift from wherever it sits in the run: across three sweeps of identical code one case's slow-level count read 4, 4 and 8, and another's decode count 0, 2 and 16. Read the throughput columns as indicative, not as a gate, and do not act on a change of one or two.
- Each trial runs as many iterations as fit its time budget, sized per row from a single probe iteration, so a fast row is measured over a longer loop rather than a slower one being measured more times. Reports generated before this replaced a fixed byte target read low levels over windows of a few milliseconds; their fast-level throughput is measured over too short an interval to compare against numbers here.

| Setting | Value |
| --- | --- |
| Output file | `BENCHMARKS.md` |
| Corpus cases | 11 |
| Input bytes per case | 4194304 |
| Benchmarked levels | `1-22` |
| Case filters | all |
| Block size | 131072 |
| Streaming piece size | 32 KiB |
| Streaming sensitivity pieces | 16 KiB, 128 KiB, 1 MiB |
| zstandard revision | `v0.1.0-4-g1019ed7` |
| Upstream zstd reference | `v1.5.7` |
| Timing trial budget | 60 ms |
| Iterations per trial | 1-128, sized per row |
| Stage profiling target bytes | 33554432 |
| Timing trials per row (fastest reported) | 3 |

## Coverage Summary

| Metric | Value |
| --- | --- |
| Corpus cases | 11 |
| Total case/level rows | 242 |
| Rust encode rows supported | 242/242 |
| Rust encode rows completed | 242 |
| Rust decode rows completed | 242 |
| Rust encoder level range | -131072..=22 |
| Benchmarked levels | 1-22 |


## Target Gaps

| Metric | Value |
| --- | --- |
| Encode rows below 50% | 0 |
| Decode rows below 50% | 0 |
| Ratio regressions | 25 |
| Ratio regressions above 1% | 0 |
| Cases behind upstream on a third of encode levels | 2 |
| Cases behind upstream on a third of decode levels | 0 |
| Streaming rows above upstream | 18 |
| Streaming rows above upstream by 1% | 0 |
| Streaming rows below upstream by 1% | 65 |

### Throughput by Case

This crate's throughput as a fraction of upstream's, summarized over the benchmarked levels. Above 1.00x is this crate being faster.

The `slow` column counts the case's levels below 90% of upstream, and it is the one to read. A single row's throughput moves a tenth between sweeps of identical code, so no one row means anything here; the median is worth less than it looks too, because a case split into a slow band and a fast one has its median on the boundary between them and crosses it on noise. The worst column says which level to open first once `slow` has flagged the case.

| Case | Encode slow | Encode median | Encode worst | Level | Decode slow | Decode median | Decode worst | Level |
| :--- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| small-alphabet | 0/22 | 1.09x | 0.99x | 13 | 0/22 | 4.49x | 2.44x | 18 |
| repeated-chunk | 0/22 | 1.17x | 1.06x | 4 | 0/22 | 3.28x | 1.56x | 16 |
| json-records | 0/22 | 1.01x | 0.95x | 4 | 2/22 | 0.97x | 0.90x | 19 |
| log-lines | 3/22 | 0.96x | 0.84x | 7 | 2/22 | 0.93x | 0.85x | 19 |
| mixed-entropy | 7/22 | 0.94x | 0.77x | 22 | 0/22 | 0.95x | 0.92x | 6 |
| wikipedia | 0/22 | 1.05x | 0.95x | 13 | 7/22 | 0.94x | 0.87x | 4 |
| tabular-csv | 0/22 | 0.97x | 0.94x | 5 | 2/22 | 0.99x | 0.87x | 18 |
| binary-structured | 0/22 | 1.00x | 0.96x | 4 | 0/22 | 1.02x | 0.94x | 1 |
| pseudorandom | 10/22 | 0.96x | 0.58x | 14 | 0/22 | 1.18x | 1.07x | 17 |
| raw-dictionary | 7/22 | 0.95x | 0.65x | 9 | 7/22 | 0.91x | 0.88x | 9 |
| trained-dictionary | 8/22 | 0.94x | 0.67x | 10 | 1/22 | 0.94x | 0.85x | 14 |

### Ratio Regressions

Rows where this crate emitted more bytes than upstream, largest relative excess first. The comparison is on exact byte counts: most of these rows differ by a handful of bytes on a multi-megabyte case and are listed for completeness, not as defects.

- raw-dictionary L18 +0.08% (53411 vs 53368 bytes, +43)
- raw-dictionary L19 +0.08% (53411 vs 53368 bytes, +43)
- raw-dictionary L20 +0.08% (53411 vs 53368 bytes, +43)
- raw-dictionary L21 +0.08% (53411 vs 53368 bytes, +43)
- raw-dictionary L22 +0.08% (53411 vs 53368 bytes, +43)
- raw-dictionary L9 +0.05% (58376 vs 58345 bytes, +31)
- raw-dictionary L10 +0.05% (58376 vs 58345 bytes, +31)
- raw-dictionary L14 +0.02% (52212 vs 52200 bytes, +12)
- wikipedia L21 +0.01% (9719 vs 9718 bytes, +1)
- tabular-csv L22 +0.01% (459064 vs 459021 bytes, +43)
- binary-structured L16 +0.01% (522221 vs 522182 bytes, +39)
- binary-structured L17 +0.01% (522226 vs 522187 bytes, +39)
- tabular-csv L20 +0.01% (460818 vs 460784 bytes, +34)
- tabular-csv L21 +0.01% (460818 vs 460784 bytes, +34)
- tabular-csv L19 +0.01% (460820 vs 460787 bytes, +33)
- mixed-entropy L18 +0.01% (1395682 vs 1395604 bytes, +78)
- mixed-entropy L19 +0.01% (1395682 vs 1395605 bytes, +77)
- mixed-entropy L20 +0.01% (1395682 vs 1395605 bytes, +77)
- mixed-entropy L21 +0.01% (1395682 vs 1395605 bytes, +77)
- mixed-entropy L22 +0.01% (1395682 vs 1395605 bytes, +77)
- binary-structured L19 +0.00% (520950 vs 520945 bytes, +5)
- binary-structured L20 +0.00% (520950 vs 520945 bytes, +5)
- binary-structured L21 +0.00% (520950 vs 520945 bytes, +5)
- binary-structured L22 +0.00% (520950 vs 520945 bytes, +5)
- binary-structured L18 +0.00% (520963 vs 520958 bytes, +5)

### Streaming Size Deltas Above 1%

Signed against upstream's streaming encoder at the same piece size, largest excess first. Negative rows are this crate emitting fewer bytes; they are listed because a streaming size difference in either direction is a block-layout difference, and the direction alone does not say which implementation made the better choice.

- log-lines L8 piece 32 KiB -1.04% (624891 vs 631474 bytes)
- tabular-csv L4 piece 32 KiB -1.21% (635004 vs 642754 bytes)
- wikipedia L3 piece 16 KiB -1.41% (53978 vs 54752 bytes)
- wikipedia L3 piece 32 KiB -1.41% (53978 vs 54752 bytes)
- wikipedia L3 piece 128 KiB -1.41% (53978 vs 54752 bytes)
- wikipedia L3 piece 1 MiB -1.41% (53978 vs 54752 bytes)
- wikipedia L4 piece 32 KiB -1.41% (53978 vs 54752 bytes)
- tabular-csv L5 piece 32 KiB -1.50% (619153 vs 628604 bytes)
- tabular-csv L9 piece 16 KiB -1.94% (533529 vs 544092 bytes)
- tabular-csv L9 piece 32 KiB -1.94% (533529 vs 544092 bytes)
- tabular-csv L9 piece 128 KiB -1.94% (533529 vs 544092 bytes)
- tabular-csv L9 piece 1 MiB -1.94% (533529 vs 544092 bytes)
- tabular-csv L8 piece 32 KiB -1.97% (532403 vs 543126 bytes)
- json-records L8 piece 32 KiB -2.10% (292013 vs 298284 bytes)
- tabular-csv L7 piece 32 KiB -2.12% (583489 vs 596146 bytes)
- json-records L9 piece 16 KiB -2.31% (296381 vs 303380 bytes)
- json-records L9 piece 32 KiB -2.31% (296381 vs 303380 bytes)
- json-records L9 piece 128 KiB -2.31% (296381 vs 303380 bytes)
- json-records L9 piece 1 MiB -2.31% (296381 vs 303380 bytes)
- tabular-csv L6 piece 32 KiB -2.50% (565980 vs 580506 bytes)
- wikipedia L8 piece 32 KiB -2.53% (11319 vs 11613 bytes)
- wikipedia L9 piece 16 KiB -2.53% (11352 vs 11647 bytes)
- wikipedia L9 piece 32 KiB -2.53% (11352 vs 11647 bytes)
- wikipedia L9 piece 128 KiB -2.53% (11352 vs 11647 bytes)
- wikipedia L9 piece 1 MiB -2.53% (11352 vs 11647 bytes)
- wikipedia L7 piece 32 KiB -2.56% (11327 vs 11625 bytes)
- tabular-csv L10 piece 32 KiB -2.60% (535301 vs 549584 bytes)
- wikipedia L10 piece 32 KiB -2.83% (11047 vs 11369 bytes)
- wikipedia L11 piece 32 KiB -2.86% (11017 vs 11341 bytes)
- wikipedia L12 piece 32 KiB -2.86% (11017 vs 11341 bytes)
- wikipedia L14 piece 32 KiB -3.25% (10746 vs 11107 bytes)
- wikipedia L13 piece 32 KiB -3.26% (10807 vs 11171 bytes)
- wikipedia L15 piece 16 KiB -3.27% (10663 vs 11024 bytes)
- wikipedia L15 piece 32 KiB -3.27% (10663 vs 11024 bytes)
- wikipedia L15 piece 128 KiB -3.27% (10663 vs 11024 bytes)
- wikipedia L15 piece 1 MiB -3.27% (10663 vs 11024 bytes)
- tabular-csv L11 piece 32 KiB -3.62% (565881 vs 587115 bytes)
- log-lines L10 piece 32 KiB -3.78% (541522 vs 562802 bytes)
- tabular-csv L12 piece 32 KiB -3.95% (562277 vs 585381 bytes)
- log-lines L13 piece 32 KiB -4.79% (518945 vs 545060 bytes)
- log-lines L14 piece 32 KiB -4.85% (516870 vs 543191 bytes)
- log-lines L11 piece 32 KiB -4.89% (517889 vs 544533 bytes)
- log-lines L12 piece 32 KiB -4.92% (517298 vs 544045 bytes)
- tabular-csv L13 piece 32 KiB -4.94% (607440 vs 639024 bytes)
- json-records L13 piece 32 KiB -5.09% (233318 vs 245824 bytes)
- tabular-csv L14 piece 32 KiB -5.40% (610769 vs 645651 bytes)
- log-lines L15 piece 16 KiB -5.72% (502035 vs 532496 bytes)
- log-lines L15 piece 32 KiB -5.72% (502035 vs 532496 bytes)
- log-lines L15 piece 128 KiB -5.72% (502035 vs 532496 bytes)
- log-lines L15 piece 1 MiB -5.72% (502035 vs 532496 bytes)
- json-records L14 piece 32 KiB -5.73% (224779 vs 238447 bytes)
- json-records L15 piece 16 KiB -5.92% (221786 vs 235753 bytes)
- json-records L15 piece 32 KiB -5.92% (221786 vs 235753 bytes)
- json-records L15 piece 128 KiB -5.92% (221786 vs 235753 bytes)
- json-records L15 piece 1 MiB -5.92% (221786 vs 235753 bytes)
- json-records L11 piece 32 KiB -6.30% (235770 vs 251618 bytes)
- json-records L12 piece 32 KiB -6.32% (235787 vs 251682 bytes)
- json-records L10 piece 32 KiB -6.34% (236264 vs 252246 bytes)
- tabular-csv L15 piece 16 KiB -7.28% (573388 vs 618401 bytes)
- tabular-csv L15 piece 32 KiB -7.28% (573388 vs 618401 bytes)
- tabular-csv L15 piece 128 KiB -7.28% (573388 vs 618401 bytes)
- tabular-csv L15 piece 1 MiB -7.28% (573388 vs 618401 bytes)
- json-records L5 piece 32 KiB -7.81% (380127 vs 412350 bytes)
- json-records L6 piece 32 KiB -14.63% (297251 vs 348192 bytes)
- json-records L7 piece 32 KiB -14.97% (295302 vs 347284 bytes)



## small-alphabet

Four-symbol high-redundancy synthetic text.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0001 | 0.0001 | 16259.39 | 14443.28 | 38629.48 | 5587.13 |
| 2 | ok | ok | 0.0001 | 0.0001 | 16079.67 | 14416.44 | 39448.72 | 5596.36 |
| 3 | ok | ok | 0.0001 | 0.0001 | 8830.36 | 8161.66 | 39372.76 | 5595.89 |
| 4 | ok | ok | 0.0001 | 0.0001 | 8308.82 | 8139.03 | 39485.35 | 5597.34 |
| 5 | ok | ok | 0.0001 | 0.0001 | 4063.89 | 3965.11 | 39326.26 | 5587.80 |
| 6 | ok | ok | 0.0001 | 0.0001 | 2809.04 | 2575.79 | 40078.54 | 8974.60 |
| 7 | ok | ok | 0.0001 | 0.0001 | 2812.41 | 2568.08 | 39806.15 | 8849.56 |
| 8 | ok | ok | 0.0001 | 0.0001 | 1521.71 | 1441.82 | 39687.74 | 8795.74 |
| 9 | ok | ok | 0.0001 | 0.0001 | 1522.80 | 1430.78 | 39512.78 | 8789.39 |
| 10 | ok | ok | 0.0001 | 0.0001 | 1519.11 | 1390.41 | 40100.25 | 8731.83 |
| 11 | ok | ok | 0.0001 | 0.0001 | 1519.21 | 1391.64 | 39125.54 | 8717.68 |
| 12 | ok | ok | 0.0001 | 0.0001 | 1505.94 | 1305.74 | 37288.68 | 8841.78 |
| 13 | ok | ok | 0.0001 | 0.0001 | 1437.83 | 1449.51 | 39859.09 | 8886.54 |
| 14 | ok | ok | 0.0001 | 0.0001 | 1367.66 | 1359.77 | 39580.36 | 8727.12 |
| 15 | ok | ok | 0.0001 | 0.0001 | 1301.25 | 1238.72 | 39603.58 | 8808.06 |
| 16 | ok | ok | 0.0001 | 0.0001 | 4184.99 | 412.01 | 39794.03 | 15973.49 |
| 17 | ok | ok | 0.0001 | 0.0001 | 3605.71 | 420.04 | 39702.10 | 16042.19 |
| 18 | ok | ok | 0.0001 | 0.0001 | 3530.75 | 408.69 | 38989.84 | 16006.29 |
| 19 | ok | ok | 0.0001 | 0.0001 | 2538.23 | 403.24 | 39757.73 | 16052.95 |
| 20 | ok | ok | 0.0001 | 0.0001 | 2138.90 | 389.65 | 39565.70 | 16010.58 |
| 21 | ok | ok | 0.0001 | 0.0001 | 2122.29 | 426.85 | 42849.37 | 16155.43 |
| 22 | ok | ok | 0.0001 | 0.0001 | 2178.57 | 386.43 | 39417.08 | 16029.20 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 406 | 406 | +0.00% | -0.25% |
| 2 | 406 | 406 | +0.00% | -0.25% |
| 3 | 406 | 406 | +0.00% | -0.25% |
| 4 | 406 | 406 | +0.00% | -0.25% |
| 5 | 406 | 407 | -0.25% | -0.25% |
| 6 | 371 | 373 | -0.54% | -0.27% |
| 7 | 371 | 373 | -0.54% | -0.27% |
| 8 | 371 | 373 | -0.54% | -0.27% |
| 9 | 371 | 371 | +0.00% | +0.00% |
| 10 | 371 | 371 | +0.00% | +0.00% |
| 11 | 371 | 371 | +0.00% | +0.00% |
| 12 | 371 | 371 | +0.00% | +0.00% |
| 13 | 371 | 372 | -0.27% | +0.00% |
| 14 | 371 | 372 | -0.27% | +0.00% |
| 15 | 371 | 372 | -0.27% | +0.00% |
| 16 | 379 | 379 | +0.00% | +0.00% |
| 17 | 379 | 379 | +0.00% | +0.00% |
| 18 | 379 | 379 | +0.00% | +0.00% |
| 19 | 379 | 379 | +0.00% | +0.00% |
| 20 | 379 | 379 | +0.00% | +0.00% |
| 21 | 379 | 379 | +0.00% | +0.00% |
| 22 | 379 | 379 | +0.00% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | -0.27% | 15 |
| 128 KiB | +0.00% | 19 | -0.27% | 15 |
| 1 MiB | +0.00% | 19 | -0.27% | 15 |


## repeated-chunk

Single repeated chunk that stresses match finding and repcodes.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0001 | 0.0001 | 20647.94 | 17767.29 | 45720.24 | 13811.98 |
| 2 | ok | ok | 0.0001 | 0.0001 | 20343.83 | 17768.52 | 45357.57 | 13819.14 |
| 3 | ok | ok | 0.0001 | 0.0001 | 11894.56 | 10374.24 | 45552.78 | 13794.85 |
| 4 | ok | ok | 0.0001 | 0.0001 | 10920.60 | 10350.89 | 44875.87 | 13747.42 |
| 5 | ok | ok | 0.0001 | 0.0001 | 7376.98 | 6686.79 | 45773.03 | 13793.54 |
| 6 | ok | ok | 0.0001 | 0.0001 | 4079.05 | 3512.43 | 45555.48 | 13824.58 |
| 7 | ok | ok | 0.0001 | 0.0001 | 4078.73 | 3486.45 | 45327.79 | 13763.97 |
| 8 | ok | ok | 0.0001 | 0.0001 | 2354.08 | 2104.51 | 45353.55 | 13802.73 |
| 9 | ok | ok | 0.0001 | 0.0001 | 2355.86 | 2083.73 | 45256.00 | 13807.82 |
| 10 | ok | ok | 0.0001 | 0.0001 | 2347.68 | 2003.26 | 46077.69 | 13824.29 |
| 11 | ok | ok | 0.0001 | 0.0001 | 2342.99 | 2003.89 | 45012.47 | 13792.19 |
| 12 | ok | ok | 0.0001 | 0.0001 | 2345.55 | 1838.70 | 44986.27 | 13814.22 |
| 13 | ok | ok | 0.0001 | 0.0001 | 290.97 | 262.40 | 45284.86 | 13778.62 |
| 14 | ok | ok | 0.0001 | 0.0001 | 212.42 | 185.91 | 45800.67 | 13805.68 |
| 15 | ok | ok | 0.0001 | 0.0001 | 119.24 | 99.41 | 45680.64 | 13822.40 |
| 16 | ok | ok | 0.0001 | 0.0001 | 5348.87 | 1428.80 | 40068.34 | 25763.60 |
| 17 | ok | ok | 0.0001 | 0.0001 | 4485.74 | 1338.54 | 40119.89 | 25744.17 |
| 18 | ok | ok | 0.0001 | 0.0001 | 4362.84 | 1333.78 | 44809.59 | 15117.86 |
| 19 | ok | ok | 0.0001 | 0.0001 | 2929.05 | 1336.60 | 44794.24 | 15118.28 |
| 20 | ok | ok | 0.0001 | 0.0001 | 2405.03 | 1171.65 | 42317.69 | 14801.64 |
| 21 | ok | ok | 0.0001 | 0.0001 | 2419.49 | 1211.86 | 43212.22 | 15160.06 |
| 22 | ok | ok | 0.0001 | 0.0001 | 2430.03 | 1222.44 | 44401.08 | 14687.26 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 441 | 441 | +0.00% | -0.23% |
| 2 | 441 | 441 | +0.00% | -0.23% |
| 3 | 441 | 441 | +0.00% | -0.23% |
| 4 | 441 | 441 | +0.00% | -0.23% |
| 5 | 441 | 442 | -0.23% | -0.23% |
| 6 | 441 | 442 | -0.23% | -0.23% |
| 7 | 441 | 442 | -0.23% | -0.23% |
| 8 | 441 | 442 | -0.23% | -0.23% |
| 9 | 441 | 441 | +0.00% | +0.00% |
| 10 | 441 | 441 | +0.00% | +0.00% |
| 11 | 441 | 441 | +0.00% | +0.00% |
| 12 | 441 | 441 | +0.00% | +0.00% |
| 13 | 441 | 441 | +0.00% | +0.00% |
| 14 | 441 | 441 | +0.00% | +0.00% |
| 15 | 441 | 441 | +0.00% | +0.00% |
| 16 | 412 | 412 | +0.00% | +0.00% |
| 17 | 412 | 412 | +0.00% | +0.00% |
| 18 | 410 | 410 | +0.00% | +0.00% |
| 19 | 410 | 410 | +0.00% | +0.00% |
| 20 | 410 | 410 | +0.00% | +0.00% |
| 21 | 410 | 410 | +0.00% | +0.00% |
| 22 | 410 | 410 | +0.00% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | +0.00% | 1 |
| 128 KiB | +0.00% | 19 | +0.00% | 1 |
| 1 MiB | +0.00% | 19 | +0.00% | 1 |


## json-records

Structured JSON-like service records with repeated keys and modest value churn.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0578 | 0.0578 | 1993.06 | 1775.18 | 5378.71 | 5402.57 |
| 2 | ok | ok | 0.0900 | 0.0900 | 1367.84 | 1131.00 | 3925.36 | 3914.44 |
| 3 | ok | ok | 0.1082 | 0.1082 | 944.17 | 956.22 | 3935.94 | 3903.77 |
| 4 | ok | ok | 0.1073 | 0.1073 | 921.56 | 967.48 | 3857.42 | 3872.56 |
| 5 | ok | ok | 0.0906 | 0.0983 | 342.21 | 357.85 | 3807.77 | 3934.98 |
| 6 | ok | ok | 0.0709 | 0.0830 | 287.53 | 290.77 | 4566.64 | 4652.10 |
| 7 | ok | ok | 0.0704 | 0.0828 | 255.81 | 255.06 | 4616.91 | 4679.46 |
| 8 | ok | ok | 0.0696 | 0.0711 | 196.67 | 205.78 | 4381.51 | 4588.81 |
| 9 | ok | ok | 0.0707 | 0.0723 | 194.67 | 196.46 | 4373.79 | 4472.61 |
| 10 | ok | ok | 0.0563 | 0.0601 | 166.61 | 163.68 | 5435.61 | 5562.42 |
| 11 | ok | ok | 0.0562 | 0.0600 | 123.99 | 118.72 | 5362.36 | 5489.69 |
| 12 | ok | ok | 0.0562 | 0.0600 | 118.55 | 105.72 | 5371.19 | 5526.52 |
| 13 | ok | ok | 0.0556 | 0.0586 | 115.60 | 105.63 | 5426.03 | 5607.08 |
| 14 | ok | ok | 0.0536 | 0.0569 | 97.76 | 88.91 | 5616.58 | 5763.55 |
| 15 | ok | ok | 0.0529 | 0.0562 | 78.31 | 71.09 | 5646.81 | 5776.62 |
| 16 | ok | ok | 0.0532 | 0.0532 | 7.16 | 6.67 | 4659.49 | 4978.65 |
| 17 | ok | ok | 0.0532 | 0.0532 | 6.61 | 6.50 | 4718.43 | 4997.41 |
| 18 | ok | ok | 0.0565 | 0.0565 | 5.69 | 5.56 | 4837.65 | 5248.24 |
| 19 | ok | ok | 0.0480 | 0.0480 | 3.01 | 3.03 | 5760.63 | 6416.33 |
| 20 | ok | ok | 0.0480 | 0.0480 | 3.00 | 3.00 | 5782.60 | 6424.41 |
| 21 | ok | ok | 0.0480 | 0.0480 | 2.88 | 3.00 | 5782.52 | 6419.42 |
| 22 | ok | ok | 0.0480 | 0.0480 | 2.90 | 2.91 | 5775.77 | 6419.07 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 242385 | 242385 | +0.00% | -0.00% |
| 2 | 377551 | 377551 | +0.00% | -0.00% |
| 3 | 453910 | 456240 | -0.51% | -0.00% |
| 4 | 450201 | 452855 | -0.59% | -0.00% |
| 5 | 380127 | 412350 | -7.81% | -0.00% |
| 6 | 297251 | 348192 | -14.63% | -0.00% |
| 7 | 295302 | 347284 | -14.97% | -0.00% |
| 8 | 292013 | 298284 | -2.10% | -0.00% |
| 9 | 296381 | 303380 | -2.31% | +0.00% |
| 10 | 236264 | 252246 | -6.34% | +0.00% |
| 11 | 235770 | 251618 | -6.30% | +0.00% |
| 12 | 235787 | 251682 | -6.32% | +0.00% |
| 13 | 233318 | 245824 | -5.09% | +0.00% |
| 14 | 224779 | 238447 | -5.73% | +0.00% |
| 15 | 221786 | 235753 | -5.92% | +0.00% |
| 16 | 223302 | 223302 | +0.00% | +0.00% |
| 17 | 222962 | 222962 | +0.00% | +0.00% |
| 18 | 237130 | 237130 | +0.00% | +0.00% |
| 19 | 201232 | 201232 | +0.00% | +0.00% |
| 20 | 201232 | 201232 | +0.00% | +0.00% |
| 21 | 201232 | 201232 | +0.00% | +0.00% |
| 22 | 201232 | 201232 | +0.00% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | -5.92% | 15 |
| 128 KiB | +0.00% | 19 | -5.92% | 15 |
| 1 MiB | +0.00% | 19 | -5.92% | 15 |

### Rust First-Block Stage Timing

- Samples the first raw `block_size` chunk only, so the timing breakdown stays aligned with the real block-local hot path.
- Uses prepared dictionaries for dictionary-backed cases so the sample reflects encoder hot paths instead of repeated dictionary parsing.
- The stage table above is sampled with the planner's phase timers off, so its milliseconds and its shares are both the real encoder's. The two sub-breakdown tables below need those timers and are sampled separately, because a timer taken per lazy parser step costs far more than the step: with them on, this case's first block reads up to 18x its real time and 99% of the frame lands in `Plan`. Read the sub-breakdowns as shares of their own row and never against the table above.
- The planning sub-breakdown covers row and chain/extdict lazy paths; other planner families may still report zeros. The lazy parser phase sub-breakdown is instrumented for no-dict row and trained-dictionary chain/extdict cases, and likewise reports zeros elsewhere.
- Sampled on levels 3-7 over 3 iterations.

| Level | Sampled ms | Blocks | Compressed | Split % | Plan % | Lit % | Seq % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.43 | 1 | 1 | 0.5 | 61.0 | 11.2 | 20.1 | 7.1 |
| 4 | 0.44 | 1 | 1 | 0.0 | 63.4 | 6.3 | 18.5 | 11.8 |
| 5 | 1.13 | 1 | 1 | 0.0 | 88.8 | 2.4 | 6.6 | 2.3 |
| 6 | 1.41 | 1 | 1 | 0.0 | 91.9 | 1.9 | 5.1 | 1.1 |
| 7 | 1.60 | 1 | 1 | 0.0 | 92.9 | 1.8 | 4.4 | 0.9 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 1.07 | 1 | 1 | 2.5 | 2.0 | 7.0 | 88.2 | 0.3 |
| 4 | 1.06 | 1 | 1 | 2.4 | 2.0 | 7.0 | 88.3 | 0.3 |
| 5 | 0.89 | 1 | 1 | 2.5 | 2.1 | 7.0 | 88.1 | 0.3 |
| 6 | 0.93 | 1 | 1 | 3.0 | 2.3 | 6.9 | 87.5 | 0.3 |
| 7 | 0.91 | 1 | 1 | 2.9 | 2.2 | 7.0 | 87.6 | 0.3 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 22.8 | 22.8 | 0.0 | 54.4 |
| 4 | 22.3 | 22.1 | 0.0 | 55.6 |
| 5 | 22.6 | 22.3 | 0.0 | 55.1 |
| 6 | 22.5 | 22.5 | 0.0 | 55.0 |
| 7 | 23.0 | 22.9 | 0.0 | 54.1 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 5.0 | 0.0 | 1.9 | 6.2 | 12.9 | 66.0 |
| 6 | 5.5 | 0.0 | 3.2 | 6.4 | 13.4 | 65.3 |
| 7 | 5.9 | 0.0 | 4.0 | 6.5 | 13.3 | 65.2 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 16.1 | 40.2 | 0.0 | 2.8 | 5.8 | 35.2 |
| 6 | 12.9 | 31.8 | 17.3 | 2.2 | 4.6 | 31.1 |
| 7 | 10.9 | 26.5 | 28.6 | 1.8 | 3.8 | 28.4 |

### Rust First-Block Parser Stats

- One-shot trace of the first block at each level. Sequence counts, byte breakdowns, and repcode usage come from the parser trace, not from timing.
- Sampled on levels 3-7.

| Level | Sequences | C Seqs | Lit bytes | Match bytes | Rep1 | Rep2 | Rep3 | Rep1-1 | Explicit | Avg ML | Avg offset |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 3688 | 3688 | 9038 | 122034 | 1295 | 23 | 0 | 0 | 2370 | 33.1 | 24310 |
| 4 | 3688 | 3688 | 9040 | 122032 | 1294 | 21 | 0 | 0 | 2373 | 33.1 | 24390 |
| 5 | 3104 | 3104 | 8669 | 122403 | 1278 | 757 | 143 | 1 | 925 | 39.4 | 30928 |
| 6 | 3038 | 3038 | 8916 | 122156 | 1391 | 618 | 79 | 1 | 949 | 40.2 | 26890 |
| 7 | 3017 | 3017 | 8939 | 122133 | 1403 | 620 | 76 | 1 | 917 | 40.5 | 28127 |


## log-lines

Timestamped log-style lines with stable fields and changing numeric values.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.1715 | 0.1716 | 807.84 | 794.71 | 2132.52 | 2261.07 |
| 2 | ok | ok | 0.1671 | 0.1671 | 778.36 | 670.30 | 2011.21 | 2158.06 |
| 3 | ok | ok | 0.1917 | 0.1917 | 515.89 | 536.19 | 1633.91 | 1783.98 |
| 4 | ok | ok | 0.2016 | 0.2016 | 479.19 | 487.11 | 1678.42 | 1855.87 |
| 5 | ok | ok | 0.1675 | 0.1679 | 223.69 | 250.45 | 2145.91 | 2424.14 |
| 6 | ok | ok | 0.1665 | 0.1669 | 179.41 | 186.17 | 2255.51 | 2484.53 |
| 7 | ok | ok | 0.1604 | 0.1609 | 149.38 | 177.18 | 2609.06 | 2635.45 |
| 8 | ok | ok | 0.1490 | 0.1506 | 133.68 | 145.73 | 2916.96 | 2965.72 |
| 9 | ok | ok | 0.1493 | 0.1505 | 126.01 | 140.68 | 3052.80 | 2991.38 |
| 10 | ok | ok | 0.1291 | 0.1342 | 105.17 | 105.93 | 3205.79 | 3206.79 |
| 11 | ok | ok | 0.1235 | 0.1298 | 83.62 | 82.65 | 3403.44 | 3428.03 |
| 12 | ok | ok | 0.1233 | 0.1297 | 77.63 | 76.16 | 3412.54 | 3452.96 |
| 13 | ok | ok | 0.1237 | 0.1300 | 54.45 | 51.85 | 3402.16 | 3427.52 |
| 14 | ok | ok | 0.1232 | 0.1295 | 38.76 | 35.34 | 3405.26 | 3423.35 |
| 15 | ok | ok | 0.1197 | 0.1270 | 29.60 | 28.25 | 3550.87 | 3543.72 |
| 16 | ok | ok | 0.1058 | 0.1058 | 5.84 | 5.79 | 3465.86 | 3789.63 |
| 17 | ok | ok | 0.1035 | 0.1035 | 4.79 | 4.96 | 3611.43 | 3932.51 |
| 18 | ok | ok | 0.1050 | 0.1050 | 3.58 | 3.86 | 3601.72 | 3888.13 |
| 19 | ok | ok | 0.1010 | 0.1010 | 2.66 | 2.76 | 2917.85 | 3447.61 |
| 20 | ok | ok | 0.1010 | 0.1010 | 2.65 | 2.75 | 3122.36 | 3462.62 |
| 21 | ok | ok | 0.1010 | 0.1010 | 2.62 | 2.73 | 3120.79 | 3457.58 |
| 22 | ok | ok | 0.1010 | 0.1010 | 2.61 | 2.74 | 3126.60 | 3464.48 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 719428 | 719665 | -0.03% | -0.00% |
| 2 | 701002 | 701002 | +0.00% | -0.00% |
| 3 | 803842 | 805666 | -0.23% | -0.00% |
| 4 | 845587 | 847357 | -0.21% | -0.00% |
| 5 | 702601 | 704287 | -0.24% | -0.00% |
| 6 | 698181 | 699889 | -0.24% | -0.00% |
| 7 | 672599 | 674797 | -0.33% | -0.00% |
| 8 | 624891 | 631474 | -1.04% | -0.00% |
| 9 | 626405 | 631314 | -0.78% | +0.00% |
| 10 | 541522 | 562802 | -3.78% | +0.00% |
| 11 | 517889 | 544533 | -4.89% | +0.00% |
| 12 | 517298 | 544045 | -4.92% | +0.00% |
| 13 | 518945 | 545060 | -4.79% | +0.00% |
| 14 | 516870 | 543191 | -4.85% | +0.00% |
| 15 | 502035 | 532496 | -5.72% | +0.00% |
| 16 | 443842 | 443842 | +0.00% | +0.00% |
| 17 | 433911 | 433911 | +0.00% | +0.00% |
| 18 | 440258 | 440258 | +0.00% | +0.00% |
| 19 | 423592 | 423592 | +0.00% | +0.00% |
| 20 | 423592 | 423592 | +0.00% | +0.00% |
| 21 | 423592 | 423592 | +0.00% | +0.00% |
| 22 | 423592 | 423640 | -0.01% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | -5.72% | 15 |
| 128 KiB | +0.00% | 19 | -5.72% | 15 |
| 1 MiB | +0.00% | 19 | -5.72% | 15 |

### Rust First-Block Stage Timing

- Samples the first raw `block_size` chunk only, so the timing breakdown stays aligned with the real block-local hot path.
- Uses prepared dictionaries for dictionary-backed cases so the sample reflects encoder hot paths instead of repeated dictionary parsing.
- The stage table above is sampled with the planner's phase timers off, so its milliseconds and its shares are both the real encoder's. The two sub-breakdown tables below need those timers and are sampled separately, because a timer taken per lazy parser step costs far more than the step: with them on, this case's first block reads up to 18x its real time and 99% of the frame lands in `Plan`. Read the sub-breakdowns as shares of their own row and never against the table above.
- The planning sub-breakdown covers row and chain/extdict lazy paths; other planner families may still report zeros. The lazy parser phase sub-breakdown is instrumented for no-dict row and trained-dictionary chain/extdict cases, and likewise reports zeros elsewhere.
- Sampled on levels 3-7 over 3 iterations.

| Level | Sampled ms | Blocks | Compressed | Split % | Plan % | Lit % | Seq % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.76 | 1 | 1 | 0.0 | 69.8 | 6.5 | 20.0 | 3.8 |
| 4 | 0.89 | 1 | 1 | 0.0 | 69.7 | 4.5 | 18.0 | 7.9 |
| 5 | 1.79 | 1 | 1 | 0.0 | 89.2 | 2.1 | 7.0 | 1.8 |
| 6 | 2.25 | 1 | 1 | 0.0 | 92.4 | 1.7 | 5.1 | 0.8 |
| 7 | 2.57 | 1 | 1 | 0.0 | 93.4 | 1.6 | 4.4 | 0.5 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 2.16 | 1 | 1 | 1.8 | 1.0 | 7.2 | 89.8 | 0.3 |
| 4 | 2.16 | 1 | 1 | 1.7 | 1.0 | 7.2 | 90.0 | 0.1 |
| 5 | 1.48 | 1 | 1 | 2.3 | 1.3 | 7.2 | 89.0 | 0.2 |
| 6 | 1.43 | 1 | 1 | 2.5 | 1.4 | 7.3 | 88.6 | 0.2 |
| 7 | 1.17 | 1 | 1 | 3.1 | 1.6 | 7.1 | 88.0 | 0.2 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 22.4 | 22.4 | 0.0 | 55.2 |
| 4 | 22.8 | 22.7 | 0.0 | 54.4 |
| 5 | 23.0 | 22.9 | 0.0 | 54.2 |
| 6 | 22.5 | 22.5 | 0.0 | 55.0 |
| 7 | 22.0 | 21.9 | 0.0 | 56.1 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 5.0 | 0.0 | 2.3 | 6.4 | 11.4 | 67.0 |
| 6 | 5.5 | 0.0 | 3.6 | 6.3 | 11.7 | 66.5 |
| 7 | 5.9 | 0.0 | 4.2 | 6.4 | 12.1 | 66.0 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 16.0 | 40.7 | 0.0 | 2.7 | 5.8 | 34.9 |
| 6 | 13.1 | 33.0 | 16.4 | 2.1 | 4.5 | 30.8 |
| 7 | 11.2 | 28.2 | 26.4 | 1.8 | 3.7 | 28.6 |

### Rust First-Block Parser Stats

- One-shot trace of the first block at each level. Sequence counts, byte breakdowns, and repcode usage come from the parser trace, not from timing.
- Sampled on levels 3-7.

| Level | Sequences | C Seqs | Lit bytes | Match bytes | Rep1 | Rep2 | Rep3 | Rep1-1 | Explicit | Avg ML | Avg offset |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 7594 | 7594 | 17813 | 113259 | 1010 | 625 | 0 | 0 | 5959 | 14.9 | 15575 |
| 4 | 7614 | 7614 | 17747 | 113325 | 1010 | 629 | 0 | 0 | 5975 | 14.9 | 15846 |
| 5 | 5151 | 5151 | 16871 | 114201 | 855 | 351 | 143 | 0 | 3802 | 22.2 | 24078 |
| 6 | 4978 | 4978 | 16985 | 114087 | 840 | 230 | 137 | 0 | 3771 | 22.9 | 23446 |
| 7 | 4053 | 4053 | 17678 | 113394 | 625 | 93 | 232 | 0 | 3103 | 28.0 | 23761 |


## mixed-entropy

Alternating compressible and incompressible 8 KiB regions.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.3336 | 0.3336 | 8365.73 | 4456.65 | 36780.80 | 28054.79 |
| 2 | ok | ok | 0.3335 | 0.3335 | 8085.75 | 4122.04 | 36526.96 | 27903.43 |
| 3 | ok | ok | 0.3327 | 0.3327 | 3264.98 | 2957.43 | 35806.39 | 38016.04 |
| 4 | ok | ok | 0.3327 | 0.3327 | 3177.22 | 2956.33 | 35464.33 | 38349.19 |
| 5 | ok | ok | 0.3327 | 0.3327 | 1252.85 | 1304.01 | 35534.47 | 38134.96 |
| 6 | ok | ok | 0.3327 | 0.3327 | 1132.75 | 1154.62 | 35026.37 | 38206.10 |
| 7 | ok | ok | 0.3327 | 0.3327 | 1114.24 | 1141.90 | 35791.56 | 38510.72 |
| 8 | ok | ok | 0.3327 | 0.3327 | 910.45 | 933.87 | 36042.35 | 38243.20 |
| 9 | ok | ok | 0.3327 | 0.3327 | 855.77 | 884.92 | 35541.23 | 38309.02 |
| 10 | ok | ok | 0.3327 | 0.3327 | 633.13 | 689.84 | 35385.31 | 38180.46 |
| 11 | ok | ok | 0.3327 | 0.3327 | 598.92 | 665.42 | 35442.89 | 38299.86 |
| 12 | ok | ok | 0.3327 | 0.3327 | 497.81 | 498.39 | 35740.77 | 38438.44 |
| 13 | ok | ok | 0.3326 | 0.3326 | 294.11 | 321.28 | 35928.30 | 37268.89 |
| 14 | ok | ok | 0.3326 | 0.3326 | 221.13 | 241.64 | 35547.19 | 37407.76 |
| 15 | ok | ok | 0.3326 | 0.3326 | 212.46 | 216.68 | 35655.03 | 37680.31 |
| 16 | ok | ok | 0.3324 | 0.3328 | 67.85 | 86.20 | 36439.88 | 38458.65 |
| 17 | ok | ok | 0.3326 | 0.3328 | 67.75 | 80.58 | 36321.68 | 38386.56 |
| 18 | ok | ok | 0.3328 | 0.3327 | 50.86 | 64.20 | 38873.21 | 39345.27 |
| 19 | ok | ok | 0.3328 | 0.3327 | 49.51 | 64.19 | 39066.27 | 38962.03 |
| 20 | ok | ok | 0.3328 | 0.3327 | 41.76 | 54.00 | 38620.83 | 39257.78 |
| 21 | ok | ok | 0.3328 | 0.3327 | 42.04 | 54.61 | 38305.67 | 38991.70 |
| 22 | ok | ok | 0.3328 | 0.3327 | 42.11 | 54.78 | 38762.90 | 39287.91 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 1399332 | 1399332 | +0.00% | -0.00% |
| 2 | 1398635 | 1398635 | +0.00% | -0.00% |
| 3 | 1398235 | 1398235 | +0.00% | +0.21% |
| 4 | 1398235 | 1398235 | +0.00% | +0.21% |
| 5 | 1398209 | 1398211 | -0.00% | +0.21% |
| 6 | 1398211 | 1398211 | +0.00% | +0.21% |
| 7 | 1398211 | 1398211 | +0.00% | +0.21% |
| 8 | 1398211 | 1398211 | +0.00% | +0.21% |
| 9 | 1398211 | 1398211 | +0.00% | +0.21% |
| 10 | 1398211 | 1398211 | +0.00% | +0.21% |
| 11 | 1398211 | 1398211 | +0.00% | +0.21% |
| 12 | 1398211 | 1398211 | +0.00% | +0.21% |
| 13 | 1397974 | 1397990 | -0.00% | +0.20% |
| 14 | 1397978 | 1397993 | -0.00% | +0.20% |
| 15 | 1397980 | 1397997 | -0.00% | +0.20% |
| 16 | 1394027 | 1394165 | -0.01% | +0.00% |
| 17 | 1395055 | 1395370 | -0.02% | +0.00% |
| 18 | 1395682 | 1395910 | -0.02% | +0.00% |
| 19 | 1395682 | 1395911 | -0.02% | +0.00% |
| 20 | 1395682 | 1395911 | -0.02% | +0.00% |
| 21 | 1395682 | 1395911 | -0.02% | +0.00% |
| 22 | 1395682 | 1395649 | +0.00% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 9 | -0.02% | 19 |
| 128 KiB | +0.00% | 9 | -0.02% | 19 |
| 1 MiB | +0.00% | 9 | -0.02% | 19 |


## wikipedia

Encyclopaedic prose with structural repetition and moderate vocabulary.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0700 | 0.0700 | 1479.49 | 1560.26 | 3810.70 | 3894.24 |
| 2 | ok | ok | 0.0716 | 0.0716 | 1558.96 | 1561.31 | 3983.02 | 3943.48 |
| 3 | ok | ok | 0.0129 | 0.0131 | 3687.83 | 3430.78 | 11443.91 | 13078.73 |
| 4 | ok | ok | 0.0129 | 0.0131 | 3599.54 | 3409.71 | 11465.62 | 13104.87 |
| 5 | ok | ok | 0.0050 | 0.0050 | 2434.89 | 2322.43 | 24997.81 | 26300.92 |
| 6 | ok | ok | 0.0039 | 0.0039 | 2134.15 | 2014.41 | 27929.05 | 29530.51 |
| 7 | ok | ok | 0.0027 | 0.0028 | 2518.62 | 2373.26 | 28428.78 | 30151.35 |
| 8 | ok | ok | 0.0027 | 0.0028 | 1725.88 | 1671.64 | 28548.86 | 30142.47 |
| 9 | ok | ok | 0.0027 | 0.0028 | 1700.63 | 1629.03 | 28410.58 | 30078.72 |
| 10 | ok | ok | 0.0026 | 0.0027 | 1488.50 | 1392.98 | 28902.60 | 30715.70 |
| 11 | ok | ok | 0.0026 | 0.0027 | 1349.64 | 1228.88 | 29080.45 | 30612.86 |
| 12 | ok | ok | 0.0026 | 0.0027 | 1321.40 | 1139.57 | 29044.22 | 30570.81 |
| 13 | ok | ok | 0.0026 | 0.0027 | 771.96 | 814.93 | 29212.70 | 30957.13 |
| 14 | ok | ok | 0.0026 | 0.0026 | 717.96 | 720.09 | 29084.02 | 30785.56 |
| 15 | ok | ok | 0.0025 | 0.0026 | 657.77 | 637.71 | 29651.87 | 31257.63 |
| 16 | ok | ok | 0.0025 | 0.0025 | 79.58 | 69.38 | 29396.14 | 31974.02 |
| 17 | ok | ok | 0.0024 | 0.0024 | 75.53 | 65.32 | 28716.59 | 31912.24 |
| 18 | ok | ok | 0.0026 | 0.0026 | 71.95 | 62.42 | 28136.31 | 31934.14 |
| 19 | ok | ok | 0.0026 | 0.0026 | 32.38 | 31.17 | 28746.01 | 32454.36 |
| 20 | ok | ok | 0.0026 | 0.0026 | 32.27 | 30.83 | 28745.94 | 32452.30 |
| 21 | ok | ok | 0.0023 | 0.0023 | 19.32 | 18.99 | 28423.59 | 32152.73 |
| 22 | ok | ok | 0.0022 | 0.0022 | 5.56 | 5.52 | 28885.48 | 31796.07 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 293507 | 293507 | +0.00% | -0.00% |
| 2 | 300467 | 300467 | +0.00% | -0.00% |
| 3 | 53978 | 54752 | -1.41% | -0.00% |
| 4 | 53978 | 54752 | -1.41% | -0.00% |
| 5 | 21077 | 21081 | -0.02% | -0.00% |
| 6 | 16374 | 16399 | -0.15% | -0.01% |
| 7 | 11327 | 11625 | -2.56% | -0.01% |
| 8 | 11319 | 11613 | -2.53% | -0.01% |
| 9 | 11352 | 11647 | -2.53% | +0.00% |
| 10 | 11047 | 11369 | -2.83% | +0.00% |
| 11 | 11017 | 11341 | -2.86% | +0.00% |
| 12 | 11017 | 11341 | -2.86% | +0.00% |
| 13 | 10807 | 11171 | -3.26% | +0.00% |
| 14 | 10746 | 11107 | -3.25% | +0.00% |
| 15 | 10663 | 11024 | -3.27% | +0.00% |
| 16 | 10310 | 10310 | +0.00% | +0.00% |
| 17 | 10114 | 10114 | +0.00% | +0.00% |
| 18 | 11101 | 11101 | +0.00% | +0.00% |
| 19 | 10787 | 10787 | +0.00% | +0.00% |
| 20 | 10787 | 10787 | +0.00% | +0.00% |
| 21 | 9719 | 9718 | +0.01% | +0.00% |
| 22 | 9245 | 9248 | -0.03% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | -3.27% | 15 |
| 128 KiB | +0.00% | 19 | -3.27% | 15 |
| 1 MiB | +0.00% | 19 | -3.27% | 15 |


## tabular-csv

CSV rows with column repetition and numeric variation.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.2001 | 0.2001 | 658.99 | 670.87 | 1995.01 | 2108.26 |
| 2 | ok | ok | 0.1380 | 0.1380 | 679.02 | 634.63 | 1933.89 | 1929.20 |
| 3 | ok | ok | 0.1518 | 0.1518 | 549.43 | 556.78 | 2007.31 | 1945.56 |
| 4 | ok | ok | 0.1514 | 0.1514 | 562.66 | 580.95 | 2135.64 | 2049.79 |
| 5 | ok | ok | 0.1476 | 0.1499 | 248.26 | 263.48 | 1850.28 | 1801.91 |
| 6 | ok | ok | 0.1349 | 0.1384 | 148.41 | 153.94 | 2301.61 | 2188.98 |
| 7 | ok | ok | 0.1391 | 0.1421 | 131.38 | 134.80 | 2188.95 | 2159.22 |
| 8 | ok | ok | 0.1269 | 0.1295 | 98.56 | 100.94 | 2157.25 | 2151.48 |
| 9 | ok | ok | 0.1272 | 0.1297 | 98.53 | 101.34 | 2156.59 | 2155.17 |
| 10 | ok | ok | 0.1276 | 0.1310 | 73.11 | 72.37 | 2119.68 | 2122.58 |
| 11 | ok | ok | 0.1349 | 0.1400 | 55.70 | 53.27 | 2298.39 | 2331.18 |
| 12 | ok | ok | 0.1341 | 0.1396 | 52.90 | 50.35 | 2347.71 | 2383.51 |
| 13 | ok | ok | 0.1448 | 0.1524 | 37.07 | 37.66 | 2572.12 | 2592.56 |
| 14 | ok | ok | 0.1456 | 0.1539 | 31.35 | 30.14 | 2663.60 | 2675.92 |
| 15 | ok | ok | 0.1367 | 0.1474 | 24.89 | 23.79 | 2833.27 | 2815.16 |
| 16 | ok | ok | 0.1172 | 0.1172 | 6.01 | 6.30 | 1554.51 | 1735.26 |
| 17 | ok | ok | 0.1111 | 0.1111 | 5.27 | 5.48 | 1574.97 | 1748.88 |
| 18 | ok | ok | 0.0896 | 0.0897 | 3.21 | 3.35 | 1088.96 | 1245.17 |
| 19 | ok | ok | 0.1099 | 0.1099 | 2.36 | 2.49 | 1562.38 | 1725.35 |
| 20 | ok | ok | 0.1099 | 0.1099 | 2.37 | 2.50 | 1557.42 | 1723.09 |
| 21 | ok | ok | 0.1099 | 0.1099 | 2.36 | 2.50 | 1558.86 | 1722.55 |
| 22 | ok | ok | 0.1094 | 0.1094 | 1.25 | 1.32 | 1555.28 | 1723.85 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 839143 | 839172 | -0.00% | -0.00% |
| 2 | 578789 | 578807 | -0.00% | -0.00% |
| 3 | 636656 | 641501 | -0.76% | -0.00% |
| 4 | 635004 | 642754 | -1.21% | -0.00% |
| 5 | 619153 | 628604 | -1.50% | -0.00% |
| 6 | 565980 | 580506 | -2.50% | -0.00% |
| 7 | 583489 | 596146 | -2.12% | -0.00% |
| 8 | 532403 | 543126 | -1.97% | -0.00% |
| 9 | 533529 | 544092 | -1.94% | +0.00% |
| 10 | 535301 | 549584 | -2.60% | +0.00% |
| 11 | 565881 | 587115 | -3.62% | +0.00% |
| 12 | 562277 | 585381 | -3.95% | +0.00% |
| 13 | 607440 | 639024 | -4.94% | +0.00% |
| 14 | 610769 | 645651 | -5.40% | +0.00% |
| 15 | 573388 | 618401 | -7.28% | +0.00% |
| 16 | 491468 | 491471 | -0.00% | +0.00% |
| 17 | 466149 | 466160 | -0.00% | +0.00% |
| 18 | 375870 | 376433 | -0.15% | +0.00% |
| 19 | 460820 | 460787 | +0.01% | +0.00% |
| 20 | 460818 | 460784 | +0.01% | +0.00% |
| 21 | 460818 | 460784 | +0.01% | +0.00% |
| 22 | 459064 | 460725 | -0.36% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.01% | 19 | -7.28% | 15 |
| 128 KiB | +0.01% | 19 | -7.28% | 15 |
| 1 MiB | +0.01% | 19 | -7.28% | 15 |


## binary-structured

Repeating binary records with fixed headers and variable payloads.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.1379 | 0.1379 | 1551.27 | 1478.82 | 4305.27 | 4595.01 |
| 2 | ok | ok | 0.1379 | 0.1379 | 1618.11 | 1134.09 | 4302.65 | 4588.91 |
| 3 | ok | ok | 0.1337 | 0.1337 | 1309.48 | 1339.56 | 5745.93 | 5426.29 |
| 4 | ok | ok | 0.1337 | 0.1337 | 1260.82 | 1312.61 | 5734.12 | 5420.18 |
| 5 | ok | ok | 0.1337 | 0.1337 | 376.20 | 378.22 | 5736.68 | 5429.50 |
| 6 | ok | ok | 0.1333 | 0.1333 | 285.35 | 285.55 | 6313.81 | 5877.00 |
| 7 | ok | ok | 0.1333 | 0.1333 | 263.89 | 266.77 | 6309.21 | 5883.16 |
| 8 | ok | ok | 0.1361 | 0.1361 | 195.34 | 200.68 | 5000.90 | 4884.32 |
| 9 | ok | ok | 0.1361 | 0.1361 | 178.56 | 183.09 | 4972.90 | 4889.43 |
| 10 | ok | ok | 0.1361 | 0.1361 | 123.60 | 114.69 | 4999.50 | 4868.72 |
| 11 | ok | ok | 0.1361 | 0.1361 | 101.75 | 97.76 | 4993.57 | 4881.73 |
| 12 | ok | ok | 0.1361 | 0.1361 | 90.57 | 85.01 | 5001.07 | 4878.91 |
| 13 | ok | ok | 0.1358 | 0.1358 | 87.32 | 81.54 | 4983.50 | 4868.87 |
| 14 | ok | ok | 0.1358 | 0.1358 | 71.81 | 63.29 | 4984.37 | 4869.03 |
| 15 | ok | ok | 0.1358 | 0.1358 | 70.28 | 59.26 | 4993.73 | 4877.83 |
| 16 | ok | ok | 0.1245 | 0.1245 | 6.08 | 5.94 | 3162.34 | 3284.00 |
| 17 | ok | ok | 0.1245 | 0.1245 | 5.80 | 5.76 | 3166.51 | 3286.98 |
| 18 | ok | ok | 0.1242 | 0.1242 | 4.54 | 4.54 | 3089.33 | 3192.90 |
| 19 | ok | ok | 0.1242 | 0.1242 | 4.44 | 4.48 | 3075.21 | 3200.85 |
| 20 | ok | ok | 0.1242 | 0.1242 | 4.39 | 4.40 | 3072.62 | 3196.77 |
| 21 | ok | ok | 0.1242 | 0.1242 | 4.38 | 4.39 | 3091.52 | 3201.90 |
| 22 | ok | ok | 0.1242 | 0.1242 | 4.41 | 4.37 | 3066.67 | 3160.84 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 578438 | 578542 | -0.02% | -0.00% |
| 2 | 578438 | 578438 | +0.00% | -0.00% |
| 3 | 560605 | 560605 | +0.00% | -0.00% |
| 4 | 560605 | 560605 | +0.00% | -0.00% |
| 5 | 560663 | 560664 | -0.00% | -0.00% |
| 6 | 559045 | 559052 | -0.00% | -0.00% |
| 7 | 559045 | 559052 | -0.00% | -0.00% |
| 8 | 557845 | 557855 | -0.00% | -2.26% |
| 9 | 557845 | 557845 | +0.00% | -2.26% |
| 10 | 557845 | 557845 | +0.00% | -2.26% |
| 11 | 557845 | 557845 | +0.00% | -2.26% |
| 12 | 557845 | 557845 | +0.00% | -2.26% |
| 13 | 557774 | 557774 | +0.00% | -2.10% |
| 14 | 557774 | 557774 | +0.00% | -2.10% |
| 15 | 557774 | 557774 | +0.00% | -2.10% |
| 16 | 522221 | 522182 | +0.01% | +0.00% |
| 17 | 522226 | 522187 | +0.01% | +0.00% |
| 18 | 520963 | 520958 | +0.00% | +0.00% |
| 19 | 520950 | 520945 | +0.00% | +0.00% |
| 20 | 520950 | 520945 | +0.00% | +0.00% |
| 21 | 520950 | 520945 | +0.00% | +0.00% |
| 22 | 520950 | 520945 | +0.00% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | -0.02% | 1 |
| 128 KiB | +0.00% | 19 | -0.02% | 1 |
| 1 MiB | +0.00% | 19 | -0.02% | 1 |

### Rust First-Block Stage Timing

- Samples the first raw `block_size` chunk only, so the timing breakdown stays aligned with the real block-local hot path.
- Uses prepared dictionaries for dictionary-backed cases so the sample reflects encoder hot paths instead of repeated dictionary parsing.
- The stage table above is sampled with the planner's phase timers off, so its milliseconds and its shares are both the real encoder's. The two sub-breakdown tables below need those timers and are sampled separately, because a timer taken per lazy parser step costs far more than the step: with them on, this case's first block reads up to 18x its real time and 99% of the frame lands in `Plan`. Read the sub-breakdowns as shares of their own row and never against the table above.
- The planning sub-breakdown covers row and chain/extdict lazy paths; other planner families may still report zeros. The lazy parser phase sub-breakdown is instrumented for no-dict row and trained-dictionary chain/extdict cases, and likewise reports zeros elsewhere.
- Sampled on levels 3-7 over 3 iterations.

| Level | Sampled ms | Blocks | Compressed | Split % | Plan % | Lit % | Seq % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.36 | 1 | 1 | 0.1 | 61.0 | 4.0 | 27.1 | 7.8 |
| 4 | 0.38 | 1 | 1 | 0.0 | 62.0 | 3.7 | 20.9 | 13.3 |
| 5 | 1.12 | 1 | 1 | 0.0 | 85.7 | 4.8 | 7.2 | 2.3 |
| 6 | 1.33 | 1 | 1 | 0.0 | 92.3 | 1.0 | 5.6 | 1.0 |
| 7 | 1.57 | 1 | 1 | 0.0 | 93.5 | 0.8 | 4.7 | 0.9 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 1.11 | 1 | 1 | 0.0 | 2.3 | 7.0 | 90.3 | 0.4 |
| 4 | 1.11 | 1 | 1 | 0.0 | 2.1 | 7.1 | 90.4 | 0.4 |
| 5 | 1.07 | 1 | 1 | 0.0 | 2.0 | 7.2 | 90.6 | 0.2 |
| 6 | 0.93 | 1 | 1 | 0.0 | 2.6 | 7.2 | 89.9 | 0.3 |
| 7 | 0.93 | 1 | 1 | 0.0 | 2.7 | 7.1 | 89.9 | 0.3 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 22.4 | 22.4 | 0.0 | 55.2 |
| 4 | 22.4 | 22.4 | 0.0 | 55.2 |
| 5 | 21.9 | 22.1 | 0.0 | 55.9 |
| 6 | 23.5 | 24.3 | 0.0 | 52.2 |
| 7 | 21.3 | 21.3 | 0.0 | 57.4 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 5.4 | 0.0 | 1.0 | 6.2 | 12.6 | 66.4 |
| 6 | 5.6 | 0.0 | 2.5 | 6.2 | 12.8 | 65.6 |
| 7 | 5.8 | 0.0 | 3.8 | 6.2 | 12.6 | 65.3 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 17.5 | 40.8 | 0.0 | 2.0 | 4.2 | 35.6 |
| 6 | 15.0 | 35.1 | 12.3 | 1.6 | 3.4 | 32.6 |
| 7 | 13.3 | 30.9 | 21.3 | 1.4 | 2.9 | 30.1 |

### Rust First-Block Parser Stats

- One-shot trace of the first block at each level. Sequence counts, byte breakdowns, and repcode usage come from the parser trace, not from timing.
- Sampled on levels 3-7.

| Level | Sequences | C Seqs | Lit bytes | Match bytes | Rep1 | Rep2 | Rep3 | Rep1-1 | Explicit | Avg ML | Avg offset |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 3835 | 3835 | 17030 | 114042 | 3795 | 17 | 0 | 0 | 23 | 29.7 | 57 |
| 4 | 3835 | 3835 | 17030 | 114042 | 3795 | 17 | 0 | 0 | 23 | 29.7 | 57 |
| 5 | 3835 | 3835 | 17030 | 114042 | 3795 | 17 | 0 | 0 | 23 | 29.7 | 57 |
| 6 | 3298 | 3298 | 16493 | 114579 | 3257 | 18 | 0 | 0 | 23 | 34.7 | 57 |
| 7 | 3298 | 3298 | 16493 | 114579 | 3257 | 18 | 0 | 0 | 23 | 34.7 | 57 |


## pseudorandom

Deterministic incompressible-looking bytes.

- Input bytes: 4194304
- Dictionary mode: none
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 1.0000 | 1.0000 | 12591.57 | 7753.42 | 46269.94 | 36237.53 |
| 2 | ok | ok | 1.0000 | 1.0000 | 12310.37 | 8438.36 | 45149.58 | 38441.32 |
| 3 | ok | ok | 1.0000 | 1.0000 | 10030.31 | 7786.00 | 46145.70 | 39828.86 |
| 4 | ok | ok | 1.0000 | 1.0000 | 9511.80 | 7779.54 | 45703.40 | 37179.58 |
| 5 | ok | ok | 1.0000 | 1.0000 | 4884.90 | 4562.74 | 45902.47 | 39965.65 |
| 6 | ok | ok | 1.0000 | 1.0000 | 4896.71 | 4603.37 | 46030.75 | 36030.96 |
| 7 | ok | ok | 1.0000 | 1.0000 | 4769.72 | 4541.69 | 45673.85 | 39318.08 |
| 8 | ok | ok | 1.0000 | 1.0000 | 4784.00 | 4560.44 | 45588.78 | 38539.71 |
| 9 | ok | ok | 1.0000 | 1.0000 | 4418.95 | 4206.89 | 45527.97 | 38729.20 |
| 10 | ok | ok | 1.0000 | 1.0000 | 2842.31 | 2872.42 | 46596.29 | 39539.73 |
| 11 | ok | ok | 1.0000 | 1.0000 | 2613.45 | 2838.22 | 45380.68 | 37942.79 |
| 12 | ok | ok | 1.0000 | 1.0000 | 2004.78 | 1884.93 | 45877.96 | 39281.88 |
| 13 | ok | ok | 1.0000 | 1.0000 | 173.04 | 292.68 | 45874.54 | 39838.16 |
| 14 | ok | ok | 1.0000 | 1.0000 | 111.27 | 191.81 | 45704.26 | 39506.17 |
| 15 | ok | ok | 1.0000 | 1.0000 | 111.41 | 183.82 | 46260.18 | 38914.65 |
| 16 | ok | ok | 1.0000 | 1.0000 | 20.46 | 23.78 | 44610.15 | 37279.74 |
| 17 | ok | ok | 1.0000 | 1.0000 | 18.89 | 22.11 | 46089.97 | 43199.46 |
| 18 | ok | ok | 1.0000 | 1.0000 | 14.09 | 19.04 | 45753.43 | 36728.84 |
| 19 | ok | ok | 1.0000 | 1.0000 | 14.40 | 18.83 | 46627.23 | 36488.03 |
| 20 | ok | ok | 1.0000 | 1.0000 | 14.02 | 18.70 | 45900.07 | 36988.87 |
| 21 | ok | ok | 1.0000 | 1.0000 | 13.63 | 18.78 | 46049.89 | 37260.75 |
| 22 | ok | ok | 1.0000 | 1.0000 | 13.68 | 18.81 | 46203.65 | 40349.91 |
### Streaming vs Upstream Streaming

- Both sides are fed 32 KiB at a time with no pledged source size, so both frames declare a window rather than a content size.
- `delta` is signed: positive means this crate emitted more than upstream. `vs one-shot` is this crate's streaming output against its own one-shot output at the same level.
- Every frame behind this table is decoded back to the original input before its size is recorded, in both directions.

| Level | Rust stream | zstd stream | delta | vs one-shot |
| ---: | ---: | ---: | ---: | ---: |
| 1 | 4194409 | 4194409 | +0.00% | -0.00% |
| 2 | 4194409 | 4194409 | +0.00% | -0.00% |
| 3 | 4194409 | 4194409 | +0.00% | -0.00% |
| 4 | 4194409 | 4194409 | +0.00% | -0.00% |
| 5 | 4194409 | 4194409 | +0.00% | -0.00% |
| 6 | 4194409 | 4194409 | +0.00% | -0.00% |
| 7 | 4194409 | 4194409 | +0.00% | -0.00% |
| 8 | 4194409 | 4194409 | +0.00% | -0.00% |
| 9 | 4194409 | 4194409 | +0.00% | +0.00% |
| 10 | 4194409 | 4194409 | +0.00% | +0.00% |
| 11 | 4194409 | 4194409 | +0.00% | +0.00% |
| 12 | 4194409 | 4194409 | +0.00% | +0.00% |
| 13 | 4194409 | 4194409 | +0.00% | +0.00% |
| 14 | 4194409 | 4194409 | +0.00% | +0.00% |
| 15 | 4194409 | 4194409 | +0.00% | +0.00% |
| 16 | 4194409 | 4194409 | +0.00% | +0.00% |
| 17 | 4194409 | 4194409 | +0.00% | +0.00% |
| 18 | 4194409 | 4194409 | +0.00% | +0.00% |
| 19 | 4194409 | 4194409 | +0.00% | +0.00% |
| 20 | 4194409 | 4194409 | +0.00% | +0.00% |
| 21 | 4194409 | 4194409 | +0.00% | +0.00% |
| 22 | 4194409 | 4194409 | +0.00% | +0.00% |

### Streaming Piece-Size Sensitivity

- Block layout is a function of how much input arrives per call, so the table above is one sample of a curve. Sampled on levels 1,3,9,15,19, one per parser strategy.
- 128 KiB is the block max: upstream's buffered path hands its frame-chunk loop one of those per call, so it is the alignment where a chunk yields at most two blocks.

| Piece | Most over upstream | Level | Most under upstream | Level |
| ---: | ---: | ---: | ---: | ---: |
| 16 KiB | +0.00% | 19 | +0.00% | 1 |
| 128 KiB | +0.00% | 19 | +0.00% | 1 |
| 1 MiB | +0.00% | 19 | +0.00% | 1 |


## raw-dictionary

HTTP-like records aligned with the raw-content dictionary fixture.

- Input bytes: 4194304
- Dictionary mode: raw-content
- Dictionary bytes: 156 (1 per 26886 bytes of input)
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0148 | 0.0148 | 4227.53 | 3726.33 | 9707.73 | 10666.67 |
| 2 | ok | ok | 0.0148 | 0.0148 | 4081.88 | 3681.74 | 9748.59 | 10677.99 |
| 3 | ok | ok | 0.0138 | 0.0187 | 2655.39 | 2719.19 | 8645.07 | 9378.38 |
| 4 | ok | ok | 0.0334 | 0.0334 | 684.11 | 883.08 | 5785.51 | 5902.51 |
| 5 | ok | ok | 0.0315 | 0.0315 | 565.59 | 729.43 | 7679.93 | 7676.95 |
| 6 | ok | ok | 0.0289 | 0.0289 | 425.81 | 527.42 | 8703.24 | 8487.89 |
| 7 | ok | ok | 0.0289 | 0.0289 | 416.24 | 505.71 | 8711.14 | 8499.97 |
| 8 | ok | ok | 0.0289 | 0.0289 | 416.39 | 505.41 | 8711.41 | 8490.99 |
| 9 | ok | ok | 0.0139 | 0.0139 | 297.53 | 459.19 | 10679.01 | 12102.31 |
| 10 | ok | ok | 0.0139 | 0.0139 | 299.14 | 444.91 | 10499.67 | 11704.27 |
| 11 | ok | ok | 0.0124 | 0.0124 | 29.87 | 24.34 | 14006.20 | 15413.72 |
| 12 | ok | ok | 0.0125 | 0.0125 | 28.54 | 24.93 | 13465.06 | 14995.58 |
| 13 | ok | ok | 0.0125 | 0.0125 | 28.46 | 24.78 | 13327.29 | 14868.78 |
| 14 | ok | ok | 0.0124 | 0.0124 | 28.37 | 24.78 | 14008.93 | 15343.25 |
| 15 | ok | ok | 0.0127 | 0.0127 | 4.62 | 4.86 | 12614.26 | 13891.01 |
| 16 | ok | ok | 0.0125 | 0.0125 | 28.51 | 24.43 | 12913.65 | 14446.11 |
| 17 | ok | ok | 0.0126 | 0.0126 | 28.07 | 24.24 | 12792.71 | 14068.81 |
| 18 | ok | ok | 0.0127 | 0.0127 | 4.54 | 4.85 | 12414.48 | 13789.55 |
| 19 | ok | ok | 0.0127 | 0.0127 | 4.53 | 4.78 | 12454.57 | 13804.19 |
| 20 | ok | ok | 0.0127 | 0.0127 | 4.60 | 4.79 | 12448.61 | 13803.04 |
| 21 | ok | ok | 0.0127 | 0.0127 | 4.60 | 4.84 | 12488.81 | 13899.99 |
| 22 | ok | ok | 0.0127 | 0.0127 | 4.60 | 4.86 | 12475.67 | 13889.90 |

## trained-dictionary

Structured multi-endpoint records aligned with the trained dictionary fixture.

- Input bytes: 4194304
- Dictionary mode: trained
- Dictionary bytes: 512 (1 per 8192 bytes of input)
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0456 | 0.0456 | 1759.12 | 1771.76 | 4384.97 | 4344.47 |
| 2 | ok | ok | 0.0528 | 0.0528 | 1503.26 | 1590.60 | 4008.22 | 3999.86 |
| 3 | ok | ok | 0.0582 | 0.0587 | 961.97 | 1278.11 | 3332.13 | 3321.28 |
| 4 | ok | ok | 0.0643 | 0.0643 | 481.32 | 655.59 | 2831.90 | 2913.03 |
| 5 | ok | ok | 0.0614 | 0.0614 | 354.43 | 469.02 | 2976.16 | 3053.91 |
| 6 | ok | ok | 0.0532 | 0.0532 | 242.97 | 316.40 | 3240.63 | 3451.43 |
| 7 | ok | ok | 0.0532 | 0.0532 | 242.67 | 315.84 | 3238.61 | 3451.12 |
| 8 | ok | ok | 0.0532 | 0.0532 | 242.67 | 315.60 | 3239.35 | 3451.49 |
| 9 | ok | ok | 0.0528 | 0.0528 | 130.33 | 194.68 | 3250.06 | 3466.26 |
| 10 | ok | ok | 0.0528 | 0.0528 | 130.19 | 194.67 | 3256.22 | 3461.82 |
| 11 | ok | ok | 0.0458 | 0.0467 | 32.70 | 34.68 | 4321.84 | 4413.68 |
| 12 | ok | ok | 0.0431 | 0.0439 | 29.34 | 31.98 | 4672.96 | 4607.15 |
| 13 | ok | ok | 0.0436 | 0.0442 | 21.01 | 22.87 | 4431.48 | 4479.52 |
| 14 | ok | ok | 0.0384 | 0.0385 | 9.65 | 9.91 | 2844.58 | 3340.29 |
| 15 | ok | ok | 0.0393 | 0.0442 | 7.55 | 7.89 | 3959.35 | 4184.03 |
| 16 | ok | ok | 0.0432 | 0.0432 | 15.25 | 16.17 | 3820.56 | 4083.17 |
| 17 | ok | ok | 0.0393 | 0.0441 | 7.55 | 8.00 | 3669.15 | 3956.83 |
| 18 | ok | ok | 0.0393 | 0.0441 | 7.55 | 8.00 | 3673.98 | 3954.90 |
| 19 | ok | ok | 0.0393 | 0.0441 | 7.55 | 8.00 | 3676.05 | 3947.68 |
| 20 | ok | ok | 0.0393 | 0.0441 | 7.55 | 8.00 | 3669.30 | 3957.28 |
| 21 | ok | ok | 0.0393 | 0.0441 | 7.55 | 7.99 | 3659.78 | 3951.94 |
| 22 | ok | ok | 0.0393 | 0.0441 | 7.54 | 7.99 | 3668.83 | 3956.76 |
### Rust First-Block Stage Timing

- Samples the first raw `block_size` chunk only, so the timing breakdown stays aligned with the real block-local hot path.
- Uses prepared dictionaries for dictionary-backed cases so the sample reflects encoder hot paths instead of repeated dictionary parsing.
- The stage table above is sampled with the planner's phase timers off, so its milliseconds and its shares are both the real encoder's. The two sub-breakdown tables below need those timers and are sampled separately, because a timer taken per lazy parser step costs far more than the step: with them on, this case's first block reads up to 18x its real time and 99% of the frame lands in `Plan`. Read the sub-breakdowns as shares of their own row and never against the table above.
- The planning sub-breakdown covers row and chain/extdict lazy paths; other planner families may still report zeros. The lazy parser phase sub-breakdown is instrumented for no-dict row and trained-dictionary chain/extdict cases, and likewise reports zeros elsewhere.
- Sampled on levels 3-7 over 3 iterations.

| Level | Sampled ms | Blocks | Compressed | Split % | Plan % | Lit % | Seq % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.51 | 1 | 1 | 0.0 | 73.3 | 4.5 | 18.6 | 3.5 |
| 4 | 0.75 | 1 | 1 | 0.0 | 84.3 | 2.6 | 12.0 | 1.0 |
| 5 | 1.12 | 1 | 1 | 0.0 | 89.5 | 2.0 | 8.1 | 0.4 |
| 6 | 1.44 | 1 | 1 | 0.0 | 92.5 | 1.8 | 5.3 | 0.3 |
| 7 | 1.39 | 1 | 1 | 0.0 | 92.5 | 1.9 | 5.3 | 0.3 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 1.29 | 1 | 1 | 1.3 | 1.9 | 7.1 | 89.3 | 0.4 |
| 4 | 1.27 | 1 | 1 | 1.1 | 1.5 | 7.2 | 89.8 | 0.4 |
| 5 | 1.23 | 1 | 1 | 1.6 | 1.8 | 6.9 | 89.1 | 0.5 |
| 6 | 0.99 | 1 | 1 | 2.1 | 2.1 | 7.1 | 88.3 | 0.5 |
| 7 | 0.98 | 1 | 1 | 1.8 | 1.8 | 7.1 | 88.7 | 0.6 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 22.5 | 20.7 | 0.2 | 56.6 |
| 4 | 22.7 | 23.8 | 0.1 | 53.4 |
| 5 | 20.9 | 19.5 | 0.2 | 59.4 |
| 6 | 24.4 | 23.0 | 0.2 | 52.4 |
| 7 | 22.3 | 21.1 | 0.1 | 56.5 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 9.6 | 1.3 | 10.5 | 5.4 | 64.5 |
| 5 | 0.0 | 19.3 | 4.1 | 8.9 | 6.8 | 55.3 |
| 6 | 0.0 | 23.6 | 6.5 | 8.4 | 6.8 | 50.5 |
| 7 | 0.0 | 23.7 | 6.4 | 8.4 | 6.8 | 50.5 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 19.0 | 15.8 | 0.0 | 5.8 | 13.9 | 45.4 |
| 5 | 14.3 | 15.4 | 16.0 | 4.4 | 10.0 | 39.9 |
| 6 | 12.0 | 12.8 | 27.0 | 3.4 | 7.1 | 37.8 |
| 7 | 12.0 | 13.0 | 26.8 | 3.5 | 7.1 | 37.7 |

### Rust First-Block Parser Stats

- One-shot trace of the first block at each level. Sequence counts, byte breakdowns, and repcode usage come from the parser trace, not from timing.
- Sampled on levels 3-7.

| Level | Sequences | C Seqs | Lit bytes | Match bytes | Rep1 | Rep2 | Rep3 | Rep1-1 | Explicit | Avg ML | Avg offset |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 4565 | 4566 | 4579 | 126493 | 2057 | 1123 | 0 | 0 | 1385 | 27.7 | 10495 |
| 4 | 4542 | 4542 | 4455 | 126617 | 2190 | 1137 | 0 | 0 | 1215 | 27.9 | 13381 |
| 5 | 4232 | 4232 | 5285 | 125787 | 2405 | 875 | 0 | 0 | 952 | 29.7 | 14604 |
| 6 | 3489 | 3489 | 7736 | 123336 | 3181 | 139 | 0 | 0 | 169 | 35.3 | 12193 |
| 7 | 3489 | 3489 | 7736 | 123336 | 3181 | 139 | 0 | 0 | 169 | 35.3 | 12193 |


