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
| small-alphabet | 0/22 | 1.10x | 0.98x | 13 | 0/22 | 4.43x | 2.45x | 17 |
| repeated-chunk | 0/22 | 1.16x | 1.05x | 4 | 0/22 | 3.29x | 1.59x | 17 |
| json-records | 0/22 | 1.00x | 0.96x | 5 | 2/22 | 0.98x | 0.90x | 20 |
| log-lines | 0/22 | 0.97x | 0.93x | 9 | 1/22 | 0.93x | 0.90x | 3 |
| mixed-entropy | 7/22 | 0.95x | 0.76x | 20 | 0/22 | 0.95x | 0.92x | 9 |
| wikipedia | 0/22 | 1.05x | 0.95x | 1 | 6/22 | 0.96x | 0.88x | 3 |
| tabular-csv | 0/22 | 0.97x | 0.93x | 5 | 2/22 | 1.00x | 0.87x | 18 |
| binary-structured | 0/22 | 1.00x | 0.96x | 4 | 0/22 | 1.02x | 0.94x | 2 |
| pseudorandom | 10/22 | 0.97x | 0.59x | 14 | 0/22 | 1.18x | 1.11x | 2 |
| raw-dictionary | 7/22 | 0.95x | 0.67x | 10 | 6/22 | 0.91x | 0.88x | 10 |
| trained-dictionary | 8/22 | 0.94x | 0.67x | 9 | 1/22 | 0.94x | 0.86x | 14 |

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
| 1 | ok | ok | 0.0001 | 0.0001 | 16218.16 | 14436.77 | 39435.17 | 5583.77 |
| 2 | ok | ok | 0.0001 | 0.0001 | 16007.09 | 14445.32 | 39946.29 | 5592.03 |
| 3 | ok | ok | 0.0001 | 0.0001 | 8843.45 | 8183.34 | 39721.35 | 5589.19 |
| 4 | ok | ok | 0.0001 | 0.0001 | 8290.37 | 8141.42 | 39920.47 | 5591.19 |
| 5 | ok | ok | 0.0001 | 0.0001 | 4067.44 | 3980.97 | 39987.76 | 5592.96 |
| 6 | ok | ok | 0.0001 | 0.0001 | 2808.04 | 2533.63 | 40910.91 | 8802.86 |
| 7 | ok | ok | 0.0001 | 0.0001 | 2708.75 | 2571.76 | 37472.32 | 8598.50 |
| 8 | ok | ok | 0.0001 | 0.0001 | 1516.66 | 1385.07 | 37771.34 | 8513.13 |
| 9 | ok | ok | 0.0001 | 0.0001 | 1520.42 | 1431.70 | 40437.68 | 8771.48 |
| 10 | ok | ok | 0.0001 | 0.0001 | 1518.53 | 1402.14 | 37928.97 | 8694.55 |
| 11 | ok | ok | 0.0001 | 0.0001 | 1516.14 | 1391.97 | 39693.13 | 8962.46 |
| 12 | ok | ok | 0.0001 | 0.0001 | 1514.25 | 1305.36 | 39638.84 | 8829.36 |
| 13 | ok | ok | 0.0001 | 0.0001 | 1429.76 | 1452.43 | 41694.38 | 8787.17 |
| 14 | ok | ok | 0.0001 | 0.0001 | 1361.87 | 1359.26 | 38531.97 | 8762.21 |
| 15 | ok | ok | 0.0001 | 0.0001 | 1298.28 | 1234.62 | 40683.35 | 8742.72 |
| 16 | ok | ok | 0.0001 | 0.0001 | 4140.17 | 463.24 | 39473.56 | 15908.79 |
| 17 | ok | ok | 0.0001 | 0.0001 | 3624.93 | 489.51 | 39335.07 | 16054.29 |
| 18 | ok | ok | 0.0001 | 0.0001 | 3547.96 | 413.96 | 39865.04 | 16054.47 |
| 19 | ok | ok | 0.0001 | 0.0001 | 2532.74 | 404.96 | 40464.85 | 16007.61 |
| 20 | ok | ok | 0.0001 | 0.0001 | 2135.65 | 406.00 | 40200.74 | 16073.40 |
| 21 | ok | ok | 0.0001 | 0.0001 | 2142.61 | 407.19 | 40091.22 | 16198.35 |
| 22 | ok | ok | 0.0001 | 0.0001 | 2132.91 | 405.69 | 40103.39 | 16077.85 |
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
| 1 | ok | ok | 0.0001 | 0.0001 | 20524.88 | 17805.60 | 45992.33 | 13810.74 |
| 2 | ok | ok | 0.0001 | 0.0001 | 20249.29 | 17759.89 | 45975.46 | 13816.69 |
| 3 | ok | ok | 0.0001 | 0.0001 | 11829.45 | 10391.72 | 45524.93 | 13767.37 |
| 4 | ok | ok | 0.0001 | 0.0001 | 10875.75 | 10342.96 | 45525.61 | 13810.46 |
| 5 | ok | ok | 0.0001 | 0.0001 | 7369.39 | 6758.89 | 45456.56 | 13813.70 |
| 6 | ok | ok | 0.0001 | 0.0001 | 4068.08 | 3519.11 | 45535.23 | 13833.87 |
| 7 | ok | ok | 0.0001 | 0.0001 | 4063.83 | 3499.48 | 46251.48 | 13784.25 |
| 8 | ok | ok | 0.0001 | 0.0001 | 2348.84 | 2104.62 | 45671.98 | 13822.67 |
| 9 | ok | ok | 0.0001 | 0.0001 | 2355.64 | 2080.72 | 45747.47 | 13800.50 |
| 10 | ok | ok | 0.0001 | 0.0001 | 2341.77 | 2011.06 | 44946.12 | 13834.79 |
| 11 | ok | ok | 0.0001 | 0.0001 | 2337.53 | 2009.52 | 45304.72 | 13803.88 |
| 12 | ok | ok | 0.0001 | 0.0001 | 2341.82 | 1823.83 | 45496.96 | 13785.93 |
| 13 | ok | ok | 0.0001 | 0.0001 | 291.13 | 263.03 | 46098.44 | 13832.49 |
| 14 | ok | ok | 0.0001 | 0.0001 | 212.34 | 186.48 | 46037.30 | 13818.73 |
| 15 | ok | ok | 0.0001 | 0.0001 | 119.50 | 98.39 | 45701.88 | 13728.27 |
| 16 | ok | ok | 0.0001 | 0.0001 | 5358.19 | 1417.28 | 40865.33 | 25661.75 |
| 17 | ok | ok | 0.0001 | 0.0001 | 4468.55 | 1349.58 | 40924.12 | 25703.30 |
| 18 | ok | ok | 0.0001 | 0.0001 | 4360.16 | 1343.38 | 45336.65 | 14131.24 |
| 19 | ok | ok | 0.0001 | 0.0001 | 2936.13 | 1315.36 | 45362.42 | 14351.37 |
| 20 | ok | ok | 0.0001 | 0.0001 | 2421.02 | 1226.73 | 44926.24 | 15183.36 |
| 21 | ok | ok | 0.0001 | 0.0001 | 2425.41 | 1224.10 | 45008.35 | 14183.06 |
| 22 | ok | ok | 0.0001 | 0.0001 | 2415.62 | 1226.94 | 45270.67 | 14426.27 |
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
| 1 | ok | ok | 0.0578 | 0.0578 | 1990.18 | 1769.63 | 5368.45 | 5388.29 |
| 2 | ok | ok | 0.0900 | 0.0900 | 1371.42 | 1137.45 | 3923.82 | 3924.37 |
| 3 | ok | ok | 0.1082 | 0.1082 | 948.56 | 956.67 | 3939.39 | 3892.67 |
| 4 | ok | ok | 0.1073 | 0.1073 | 926.78 | 968.58 | 3893.29 | 3884.67 |
| 5 | ok | ok | 0.0906 | 0.0983 | 341.72 | 357.42 | 3846.09 | 3930.62 |
| 6 | ok | ok | 0.0709 | 0.0830 | 293.46 | 293.23 | 4562.57 | 4638.86 |
| 7 | ok | ok | 0.0704 | 0.0828 | 253.39 | 257.68 | 4613.21 | 4669.52 |
| 8 | ok | ok | 0.0696 | 0.0711 | 202.57 | 205.93 | 4431.58 | 4588.81 |
| 9 | ok | ok | 0.0707 | 0.0723 | 195.27 | 195.93 | 4368.23 | 4528.81 |
| 10 | ok | ok | 0.0563 | 0.0601 | 173.42 | 167.63 | 5437.49 | 5557.15 |
| 11 | ok | ok | 0.0562 | 0.0600 | 127.49 | 119.05 | 5399.59 | 5521.18 |
| 12 | ok | ok | 0.0562 | 0.0600 | 119.05 | 108.37 | 5392.92 | 5517.24 |
| 13 | ok | ok | 0.0556 | 0.0586 | 116.14 | 106.68 | 5483.22 | 5604.10 |
| 14 | ok | ok | 0.0536 | 0.0569 | 98.55 | 89.28 | 5623.16 | 5743.96 |
| 15 | ok | ok | 0.0529 | 0.0562 | 79.66 | 73.43 | 5670.01 | 5780.90 |
| 16 | ok | ok | 0.0532 | 0.0532 | 7.08 | 6.90 | 4711.79 | 4995.72 |
| 17 | ok | ok | 0.0532 | 0.0532 | 6.59 | 6.50 | 4719.56 | 4997.12 |
| 18 | ok | ok | 0.0565 | 0.0565 | 5.58 | 5.56 | 4836.97 | 5246.16 |
| 19 | ok | ok | 0.0480 | 0.0480 | 3.00 | 3.03 | 5782.53 | 6412.44 |
| 20 | ok | ok | 0.0480 | 0.0480 | 2.99 | 3.02 | 5779.29 | 6424.79 |
| 21 | ok | ok | 0.0480 | 0.0480 | 2.96 | 3.01 | 5771.49 | 6415.08 |
| 22 | ok | ok | 0.0480 | 0.0480 | 2.99 | 3.02 | 5786.55 | 6393.57 |
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
| 3 | 0.43 | 1 | 1 | 0.1 | 61.5 | 11.3 | 19.9 | 7.2 |
| 4 | 0.45 | 1 | 1 | 0.0 | 63.7 | 6.2 | 18.1 | 12.0 |
| 5 | 1.12 | 1 | 1 | 0.0 | 88.8 | 2.4 | 6.5 | 2.3 |
| 6 | 1.40 | 1 | 1 | 0.0 | 91.8 | 1.9 | 5.3 | 1.0 |
| 7 | 1.60 | 1 | 1 | 0.0 | 92.9 | 1.8 | 4.4 | 0.9 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 1.07 | 1 | 1 | 2.5 | 2.0 | 7.2 | 87.9 | 0.3 |
| 4 | 1.09 | 1 | 1 | 2.3 | 1.7 | 6.8 | 88.9 | 0.3 |
| 5 | 0.93 | 1 | 1 | 2.5 | 2.2 | 6.8 | 88.3 | 0.3 |
| 6 | 0.88 | 1 | 1 | 2.8 | 2.3 | 6.9 | 87.6 | 0.3 |
| 7 | 0.91 | 1 | 1 | 2.7 | 2.4 | 6.7 | 87.9 | 0.4 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 23.0 | 22.8 | 0.0 | 54.2 |
| 4 | 22.9 | 22.7 | 0.0 | 54.5 |
| 5 | 22.9 | 22.9 | 0.0 | 54.2 |
| 6 | 22.0 | 21.7 | 0.0 | 56.4 |
| 7 | 22.5 | 22.3 | 0.0 | 55.2 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 4.9 | 0.0 | 1.8 | 6.3 | 13.0 | 65.9 |
| 6 | 5.5 | 0.0 | 3.2 | 6.4 | 13.3 | 65.3 |
| 7 | 5.8 | 0.0 | 4.3 | 6.4 | 13.2 | 65.1 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 16.0 | 40.1 | 0.0 | 2.8 | 5.8 | 35.2 |
| 6 | 12.9 | 31.8 | 17.4 | 2.2 | 4.8 | 30.9 |
| 7 | 10.8 | 26.8 | 28.5 | 1.8 | 3.9 | 28.2 |

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
| 1 | ok | ok | 0.1715 | 0.1716 | 808.81 | 802.35 | 2132.88 | 2258.00 |
| 2 | ok | ok | 0.1671 | 0.1671 | 781.07 | 669.13 | 2017.16 | 2154.10 |
| 3 | ok | ok | 0.1917 | 0.1917 | 537.29 | 538.30 | 1807.01 | 2012.40 |
| 4 | ok | ok | 0.2016 | 0.2016 | 485.79 | 507.52 | 1752.29 | 1930.31 |
| 5 | ok | ok | 0.1675 | 0.1679 | 235.57 | 251.69 | 2241.04 | 2429.40 |
| 6 | ok | ok | 0.1665 | 0.1669 | 189.78 | 197.67 | 2310.98 | 2484.07 |
| 7 | ok | ok | 0.1604 | 0.1609 | 168.54 | 176.91 | 2645.40 | 2739.73 |
| 8 | ok | ok | 0.1490 | 0.1506 | 137.97 | 146.32 | 3023.85 | 2993.95 |
| 9 | ok | ok | 0.1493 | 0.1505 | 135.11 | 145.18 | 3091.46 | 3011.91 |
| 10 | ok | ok | 0.1291 | 0.1342 | 108.94 | 111.87 | 3214.71 | 3263.05 |
| 11 | ok | ok | 0.1235 | 0.1298 | 83.71 | 83.17 | 3409.65 | 3438.93 |
| 12 | ok | ok | 0.1233 | 0.1297 | 77.41 | 75.42 | 3422.79 | 3444.56 |
| 13 | ok | ok | 0.1237 | 0.1300 | 54.27 | 51.64 | 3405.70 | 3418.07 |
| 14 | ok | ok | 0.1232 | 0.1295 | 37.04 | 35.54 | 3408.04 | 3424.58 |
| 15 | ok | ok | 0.1197 | 0.1270 | 29.72 | 28.16 | 3552.02 | 3536.02 |
| 16 | ok | ok | 0.1058 | 0.1058 | 5.74 | 5.76 | 3456.39 | 3791.55 |
| 17 | ok | ok | 0.1035 | 0.1035 | 5.17 | 5.06 | 3651.02 | 3929.10 |
| 18 | ok | ok | 0.1050 | 0.1050 | 3.74 | 3.86 | 3600.75 | 3896.48 |
| 19 | ok | ok | 0.1010 | 0.1010 | 2.66 | 2.76 | 3124.76 | 3462.76 |
| 20 | ok | ok | 0.1010 | 0.1010 | 2.66 | 2.75 | 3122.96 | 3456.45 |
| 21 | ok | ok | 0.1010 | 0.1010 | 2.60 | 2.71 | 3123.21 | 3458.75 |
| 22 | ok | ok | 0.1010 | 0.1010 | 2.66 | 2.74 | 3121.39 | 3456.25 |
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
| 3 | 0.78 | 1 | 1 | 0.0 | 69.9 | 6.3 | 18.6 | 5.1 |
| 4 | 0.94 | 1 | 1 | 0.0 | 66.0 | 4.3 | 18.9 | 10.8 |
| 5 | 1.80 | 1 | 1 | 0.0 | 89.0 | 2.1 | 7.3 | 1.7 |
| 6 | 2.26 | 1 | 1 | 0.0 | 92.6 | 1.7 | 5.1 | 0.7 |
| 7 | 2.57 | 1 | 1 | 0.0 | 93.4 | 1.6 | 4.4 | 0.6 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 2.19 | 1 | 1 | 1.8 | 1.0 | 7.0 | 89.9 | 0.3 |
| 4 | 2.16 | 1 | 1 | 1.7 | 1.0 | 7.2 | 89.9 | 0.2 |
| 5 | 1.51 | 1 | 1 | 2.3 | 1.4 | 7.0 | 89.0 | 0.2 |
| 6 | 1.43 | 1 | 1 | 2.5 | 1.4 | 7.1 | 88.8 | 0.2 |
| 7 | 1.17 | 1 | 1 | 3.0 | 1.6 | 7.1 | 88.0 | 0.2 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 22.2 | 23.8 | 0.0 | 54.0 |
| 4 | 22.7 | 22.6 | 0.0 | 54.7 |
| 5 | 22.8 | 22.8 | 0.0 | 54.3 |
| 6 | 24.6 | 22.4 | 0.0 | 53.0 |
| 7 | 23.3 | 23.1 | 0.0 | 53.6 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 5.0 | 0.0 | 2.3 | 6.3 | 11.3 | 67.1 |
| 6 | 5.5 | 0.0 | 3.6 | 6.4 | 11.7 | 66.5 |
| 7 | 5.9 | 0.0 | 4.5 | 6.4 | 12.1 | 65.8 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 16.0 | 40.7 | 0.0 | 2.7 | 5.8 | 34.8 |
| 6 | 13.0 | 32.9 | 16.5 | 2.1 | 4.4 | 30.9 |
| 7 | 11.3 | 28.1 | 26.3 | 1.8 | 3.8 | 28.8 |

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
| 1 | ok | ok | 0.3336 | 0.3336 | 8322.76 | 4450.92 | 36362.43 | 28139.53 |
| 2 | ok | ok | 0.3335 | 0.3335 | 8063.12 | 4100.26 | 36128.21 | 27809.46 |
| 3 | ok | ok | 0.3327 | 0.3327 | 3265.73 | 2958.52 | 35613.06 | 38473.10 |
| 4 | ok | ok | 0.3327 | 0.3327 | 3168.31 | 2955.88 | 35670.22 | 38426.90 |
| 5 | ok | ok | 0.3327 | 0.3327 | 1251.34 | 1305.41 | 35407.52 | 38030.16 |
| 6 | ok | ok | 0.3327 | 0.3327 | 1133.25 | 1157.07 | 35716.01 | 38426.90 |
| 7 | ok | ok | 0.3327 | 0.3327 | 1126.77 | 1142.18 | 35658.92 | 38320.48 |
| 8 | ok | ok | 0.3327 | 0.3327 | 909.55 | 930.70 | 35161.28 | 38013.22 |
| 9 | ok | ok | 0.3327 | 0.3327 | 860.25 | 887.97 | 35096.73 | 38311.88 |
| 10 | ok | ok | 0.3327 | 0.3327 | 644.89 | 694.03 | 35311.05 | 38021.68 |
| 11 | ok | ok | 0.3327 | 0.3327 | 601.45 | 667.45 | 35459.01 | 38117.93 |
| 12 | ok | ok | 0.3327 | 0.3327 | 487.85 | 501.08 | 35466.60 | 38047.11 |
| 13 | ok | ok | 0.3326 | 0.3326 | 293.81 | 321.09 | 36290.47 | 37037.04 |
| 14 | ok | ok | 0.3326 | 0.3326 | 221.62 | 233.61 | 35612.05 | 37487.19 |
| 15 | ok | ok | 0.3326 | 0.3326 | 212.52 | 220.39 | 35646.77 | 37317.78 |
| 16 | ok | ok | 0.3324 | 0.3328 | 68.25 | 87.44 | 36347.07 | 38441.32 |
| 17 | ok | ok | 0.3326 | 0.3328 | 68.41 | 82.21 | 36202.80 | 38123.60 |
| 18 | ok | ok | 0.3328 | 0.3327 | 51.15 | 66.23 | 38499.14 | 39083.97 |
| 19 | ok | ok | 0.3328 | 0.3327 | 49.95 | 64.49 | 38289.20 | 39086.95 |
| 20 | ok | ok | 0.3328 | 0.3327 | 42.04 | 55.54 | 38650.14 | 39239.73 |
| 21 | ok | ok | 0.3328 | 0.3327 | 42.46 | 55.44 | 38694.32 | 39197.67 |
| 22 | ok | ok | 0.3328 | 0.3327 | 42.48 | 55.52 | 38765.12 | 39421.00 |
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
| 1 | ok | ok | 0.0700 | 0.0700 | 1478.11 | 1554.17 | 3822.81 | 3885.51 |
| 2 | ok | ok | 0.0716 | 0.0716 | 1558.02 | 1564.33 | 3970.85 | 3937.77 |
| 3 | ok | ok | 0.0129 | 0.0131 | 3680.83 | 3428.48 | 11460.09 | 13032.32 |
| 4 | ok | ok | 0.0129 | 0.0131 | 3595.10 | 3407.97 | 11484.71 | 13053.91 |
| 5 | ok | ok | 0.0050 | 0.0050 | 2431.19 | 2317.64 | 25529.95 | 26287.42 |
| 6 | ok | ok | 0.0039 | 0.0039 | 2133.69 | 2014.89 | 28543.88 | 29508.39 |
| 7 | ok | ok | 0.0027 | 0.0028 | 2524.42 | 2380.50 | 28825.11 | 30087.56 |
| 8 | ok | ok | 0.0027 | 0.0028 | 1725.40 | 1667.56 | 28887.52 | 30107.02 |
| 9 | ok | ok | 0.0027 | 0.0028 | 1699.87 | 1633.46 | 28591.11 | 30126.91 |
| 10 | ok | ok | 0.0026 | 0.0027 | 1487.38 | 1395.64 | 29604.36 | 30741.52 |
| 11 | ok | ok | 0.0026 | 0.0027 | 1348.18 | 1230.96 | 29395.30 | 30754.44 |
| 12 | ok | ok | 0.0026 | 0.0027 | 1329.53 | 1149.76 | 29480.63 | 30447.19 |
| 13 | ok | ok | 0.0026 | 0.0027 | 769.75 | 795.75 | 29756.85 | 30917.64 |
| 14 | ok | ok | 0.0026 | 0.0026 | 718.76 | 717.83 | 29885.59 | 30912.27 |
| 15 | ok | ok | 0.0025 | 0.0026 | 657.54 | 630.22 | 30254.46 | 31281.53 |
| 16 | ok | ok | 0.0025 | 0.0025 | 79.22 | 68.61 | 28607.28 | 31956.06 |
| 17 | ok | ok | 0.0024 | 0.0024 | 75.67 | 65.04 | 28674.37 | 31657.70 |
| 18 | ok | ok | 0.0026 | 0.0026 | 71.83 | 62.23 | 28224.40 | 31954.07 |
| 19 | ok | ok | 0.0026 | 0.0026 | 32.27 | 31.08 | 29103.52 | 32462.59 |
| 20 | ok | ok | 0.0026 | 0.0026 | 32.18 | 30.75 | 29770.40 | 32429.69 |
| 21 | ok | ok | 0.0023 | 0.0023 | 19.32 | 18.95 | 28551.18 | 32072.16 |
| 22 | ok | ok | 0.0022 | 0.0022 | 5.56 | 5.52 | 29244.40 | 32097.82 |
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
| 1 | ok | ok | 0.2001 | 0.2001 | 658.52 | 671.80 | 1995.74 | 2102.31 |
| 2 | ok | ok | 0.1380 | 0.1380 | 678.05 | 633.01 | 1934.84 | 1924.70 |
| 3 | ok | ok | 0.1518 | 0.1518 | 553.25 | 556.42 | 2009.96 | 1941.11 |
| 4 | ok | ok | 0.1514 | 0.1514 | 561.63 | 581.34 | 2133.97 | 2044.45 |
| 5 | ok | ok | 0.1476 | 0.1499 | 247.47 | 264.70 | 1848.38 | 1796.35 |
| 6 | ok | ok | 0.1349 | 0.1384 | 148.45 | 154.32 | 2301.76 | 2185.15 |
| 7 | ok | ok | 0.1391 | 0.1421 | 130.80 | 134.87 | 2189.42 | 2155.52 |
| 8 | ok | ok | 0.1269 | 0.1295 | 98.22 | 101.08 | 2156.80 | 2148.27 |
| 9 | ok | ok | 0.1272 | 0.1297 | 98.01 | 101.56 | 2155.45 | 2151.48 |
| 10 | ok | ok | 0.1276 | 0.1310 | 73.22 | 72.74 | 2117.41 | 2122.72 |
| 11 | ok | ok | 0.1349 | 0.1400 | 55.29 | 53.47 | 2296.56 | 2323.35 |
| 12 | ok | ok | 0.1341 | 0.1396 | 52.94 | 50.81 | 2351.34 | 2371.64 |
| 13 | ok | ok | 0.1448 | 0.1524 | 37.09 | 37.85 | 2574.59 | 2585.33 |
| 14 | ok | ok | 0.1456 | 0.1539 | 31.16 | 30.63 | 2662.24 | 2671.20 |
| 15 | ok | ok | 0.1367 | 0.1474 | 25.03 | 24.12 | 2836.85 | 2800.76 |
| 16 | ok | ok | 0.1172 | 0.1172 | 5.96 | 6.27 | 1555.87 | 1729.42 |
| 17 | ok | ok | 0.1111 | 0.1111 | 5.28 | 5.48 | 1578.81 | 1746.03 |
| 18 | ok | ok | 0.0896 | 0.0897 | 3.20 | 3.33 | 1083.09 | 1245.08 |
| 19 | ok | ok | 0.1099 | 0.1099 | 2.38 | 2.51 | 1560.82 | 1721.84 |
| 20 | ok | ok | 0.1099 | 0.1099 | 2.37 | 2.50 | 1557.16 | 1719.09 |
| 21 | ok | ok | 0.1099 | 0.1099 | 2.36 | 2.51 | 1558.03 | 1720.82 |
| 22 | ok | ok | 0.1094 | 0.1094 | 1.25 | 1.33 | 1555.81 | 1722.96 |
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
| 1 | ok | ok | 0.1379 | 0.1379 | 1550.83 | 1484.39 | 4334.67 | 4588.57 |
| 2 | ok | ok | 0.1379 | 0.1379 | 1618.30 | 1132.66 | 4325.59 | 4584.58 |
| 3 | ok | ok | 0.1337 | 0.1337 | 1312.68 | 1339.42 | 5745.06 | 5426.08 |
| 4 | ok | ok | 0.1337 | 0.1337 | 1265.86 | 1313.91 | 5730.73 | 5431.09 |
| 5 | ok | ok | 0.1337 | 0.1337 | 376.46 | 378.44 | 5734.48 | 5414.46 |
| 6 | ok | ok | 0.1333 | 0.1333 | 285.53 | 285.68 | 6290.19 | 5858.15 |
| 7 | ok | ok | 0.1333 | 0.1333 | 262.58 | 267.21 | 6294.16 | 5860.19 |
| 8 | ok | ok | 0.1361 | 0.1361 | 194.77 | 199.56 | 4987.33 | 4881.62 |
| 9 | ok | ok | 0.1361 | 0.1361 | 177.06 | 183.23 | 4972.38 | 4866.52 |
| 10 | ok | ok | 0.1361 | 0.1361 | 128.90 | 118.82 | 4986.09 | 4881.08 |
| 11 | ok | ok | 0.1361 | 0.1361 | 100.39 | 97.98 | 4982.84 | 4880.65 |
| 12 | ok | ok | 0.1361 | 0.1361 | 90.92 | 86.94 | 4965.64 | 4872.99 |
| 13 | ok | ok | 0.1358 | 0.1358 | 90.52 | 82.91 | 4965.23 | 4880.25 |
| 14 | ok | ok | 0.1358 | 0.1358 | 74.10 | 64.94 | 4976.16 | 4874.86 |
| 15 | ok | ok | 0.1358 | 0.1358 | 70.11 | 60.50 | 4973.96 | 4854.05 |
| 16 | ok | ok | 0.1245 | 0.1245 | 6.10 | 5.90 | 3166.68 | 3284.00 |
| 17 | ok | ok | 0.1245 | 0.1245 | 5.90 | 5.75 | 3167.71 | 3288.19 |
| 18 | ok | ok | 0.1242 | 0.1242 | 4.49 | 4.53 | 3097.58 | 3192.20 |
| 19 | ok | ok | 0.1242 | 0.1242 | 4.44 | 4.44 | 3086.21 | 3192.54 |
| 20 | ok | ok | 0.1242 | 0.1242 | 4.36 | 4.43 | 3093.59 | 3199.02 |
| 21 | ok | ok | 0.1242 | 0.1242 | 4.39 | 4.43 | 3089.56 | 3193.06 |
| 22 | ok | ok | 0.1242 | 0.1242 | 4.35 | 4.42 | 3092.13 | 3196.39 |
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
| 3 | 0.36 | 1 | 1 | 0.1 | 60.8 | 4.3 | 27.0 | 7.9 |
| 4 | 0.39 | 1 | 1 | 0.0 | 62.7 | 3.5 | 20.4 | 13.4 |
| 5 | 1.18 | 1 | 1 | 0.0 | 86.2 | 4.5 | 7.0 | 2.2 |
| 6 | 1.34 | 1 | 1 | 0.0 | 92.2 | 1.1 | 5.6 | 1.1 |
| 7 | 1.56 | 1 | 1 | 0.0 | 93.5 | 0.9 | 4.7 | 0.9 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 1.09 | 1 | 1 | 0.0 | 2.4 | 7.5 | 89.7 | 0.3 |
| 4 | 1.07 | 1 | 1 | 0.0 | 2.2 | 7.2 | 90.3 | 0.3 |
| 5 | 1.11 | 1 | 1 | 0.0 | 2.0 | 7.0 | 90.7 | 0.3 |
| 6 | 0.93 | 1 | 1 | 0.0 | 2.5 | 7.2 | 90.0 | 0.3 |
| 7 | 0.92 | 1 | 1 | 0.0 | 2.5 | 7.2 | 90.0 | 0.3 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 23.1 | 23.1 | 0.0 | 53.8 |
| 4 | 22.3 | 22.5 | 0.0 | 55.2 |
| 5 | 22.4 | 22.4 | 0.0 | 55.3 |
| 6 | 23.1 | 23.3 | 0.0 | 53.6 |
| 7 | 23.6 | 23.6 | 0.0 | 52.8 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 5.4 | 0.0 | 1.0 | 6.2 | 12.6 | 66.5 |
| 6 | 5.7 | 0.0 | 2.5 | 6.1 | 12.7 | 65.6 |
| 7 | 5.7 | 0.0 | 4.0 | 6.1 | 12.6 | 65.2 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 5 | 17.2 | 40.6 | 0.0 | 2.1 | 4.3 | 35.8 |
| 6 | 15.1 | 35.3 | 12.2 | 1.6 | 3.3 | 32.5 |
| 7 | 13.3 | 31.0 | 21.4 | 1.4 | 2.9 | 30.1 |

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
| 1 | ok | ok | 1.0000 | 1.0000 | 12567.86 | 7856.56 | 45986.76 | 36063.96 |
| 2 | ok | ok | 1.0000 | 1.0000 | 12027.93 | 8521.72 | 41175.07 | 37203.89 |
| 3 | ok | ok | 1.0000 | 1.0000 | 9941.51 | 7751.51 | 45214.04 | 38902.82 |
| 4 | ok | ok | 1.0000 | 1.0000 | 9497.52 | 7775.35 | 45605.87 | 38152.01 |
| 5 | ok | ok | 1.0000 | 1.0000 | 4883.35 | 4564.76 | 45151.74 | 37558.69 |
| 6 | ok | ok | 1.0000 | 1.0000 | 4887.37 | 4611.14 | 45824.58 | 38790.82 |
| 7 | ok | ok | 1.0000 | 1.0000 | 4810.07 | 4540.87 | 46252.17 | 39179.68 |
| 8 | ok | ok | 1.0000 | 1.0000 | 4800.08 | 4535.00 | 45506.90 | 40314.96 |
| 9 | ok | ok | 1.0000 | 1.0000 | 4356.97 | 4170.32 | 45934.22 | 38166.23 |
| 10 | ok | ok | 1.0000 | 1.0000 | 2843.87 | 2848.29 | 45867.01 | 36959.50 |
| 11 | ok | ok | 1.0000 | 1.0000 | 2648.87 | 2795.94 | 46463.62 | 37794.35 |
| 12 | ok | ok | 1.0000 | 1.0000 | 2001.23 | 1810.77 | 46541.57 | 38058.43 |
| 13 | ok | ok | 1.0000 | 1.0000 | 175.24 | 293.31 | 46468.89 | 40429.56 |
| 14 | ok | ok | 1.0000 | 1.0000 | 113.39 | 191.15 | 45285.86 | 38773.19 |
| 15 | ok | ok | 1.0000 | 1.0000 | 111.76 | 182.13 | 46084.95 | 40551.24 |
| 16 | ok | ok | 1.0000 | 1.0000 | 20.32 | 24.08 | 45435.89 | 39048.20 |
| 17 | ok | ok | 1.0000 | 1.0000 | 18.70 | 22.22 | 45587.60 | 36815.99 |
| 18 | ok | ok | 1.0000 | 1.0000 | 14.18 | 18.85 | 45881.05 | 37544.91 |
| 19 | ok | ok | 1.0000 | 1.0000 | 13.97 | 18.98 | 45438.58 | 39622.49 |
| 20 | ok | ok | 1.0000 | 1.0000 | 14.28 | 18.91 | 45626.87 | 39946.95 |
| 21 | ok | ok | 1.0000 | 1.0000 | 14.27 | 18.86 | 44955.33 | 37914.69 |
| 22 | ok | ok | 1.0000 | 1.0000 | 13.82 | 18.84 | 45808.02 | 37002.24 |
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
| 1 | ok | ok | 0.0148 | 0.0148 | 4220.03 | 3721.33 | 9690.57 | 10662.92 |
| 2 | ok | ok | 0.0148 | 0.0148 | 4059.66 | 3677.97 | 9725.89 | 10607.55 |
| 3 | ok | ok | 0.0138 | 0.0187 | 2641.87 | 2720.99 | 8643.59 | 9452.85 |
| 4 | ok | ok | 0.0334 | 0.0334 | 682.87 | 883.05 | 5795.72 | 5884.17 |
| 5 | ok | ok | 0.0315 | 0.0315 | 563.96 | 728.65 | 7665.90 | 7663.24 |
| 6 | ok | ok | 0.0289 | 0.0289 | 425.28 | 526.97 | 8714.18 | 8482.98 |
| 7 | ok | ok | 0.0289 | 0.0289 | 415.67 | 505.31 | 8699.67 | 8483.79 |
| 8 | ok | ok | 0.0289 | 0.0289 | 415.01 | 504.60 | 8695.76 | 8485.36 |
| 9 | ok | ok | 0.0139 | 0.0139 | 307.31 | 459.32 | 10690.33 | 12074.74 |
| 10 | ok | ok | 0.0139 | 0.0139 | 306.85 | 459.02 | 10676.20 | 12066.77 |
| 11 | ok | ok | 0.0124 | 0.0124 | 29.73 | 25.34 | 14092.49 | 15442.94 |
| 12 | ok | ok | 0.0125 | 0.0125 | 28.51 | 24.76 | 13483.55 | 14918.82 |
| 13 | ok | ok | 0.0125 | 0.0125 | 28.45 | 24.75 | 13335.65 | 14843.10 |
| 14 | ok | ok | 0.0124 | 0.0124 | 28.43 | 24.74 | 14042.85 | 15344.15 |
| 15 | ok | ok | 0.0127 | 0.0127 | 4.62 | 4.85 | 12577.10 | 13848.54 |
| 16 | ok | ok | 0.0125 | 0.0125 | 28.39 | 24.56 | 12906.37 | 14434.12 |
| 17 | ok | ok | 0.0126 | 0.0126 | 27.87 | 24.28 | 12777.05 | 14042.71 |
| 18 | ok | ok | 0.0127 | 0.0127 | 4.62 | 4.85 | 12463.40 | 13833.02 |
| 19 | ok | ok | 0.0127 | 0.0127 | 4.61 | 4.85 | 12460.58 | 13857.78 |
| 20 | ok | ok | 0.0127 | 0.0127 | 4.62 | 4.85 | 12468.75 | 13854.00 |
| 21 | ok | ok | 0.0127 | 0.0127 | 4.62 | 4.85 | 12470.16 | 13784.64 |
| 22 | ok | ok | 0.0127 | 0.0127 | 4.62 | 4.85 | 12461.02 | 13853.42 |

## trained-dictionary

Structured multi-endpoint records aligned with the trained dictionary fixture.

- Input bytes: 4194304
- Dictionary mode: trained
- Dictionary bytes: 512 (1 per 8192 bytes of input)
- Timing trial budget: 60 ms

| Level | Rust encode | Rust decode upstream | Rust ratio | zstd ratio | Rust enc MiB/s | zstd enc MiB/s | Rust dec MiB/s | zstd dec MiB/s |
| ---: | :--- | :--- | ---: | ---: | ---: | ---: | ---: | ---: |
| 1 | ok | ok | 0.0456 | 0.0456 | 1755.94 | 1768.32 | 4386.08 | 4345.69 |
| 2 | ok | ok | 0.0528 | 0.0528 | 1504.46 | 1587.87 | 3992.51 | 3994.48 |
| 3 | ok | ok | 0.0582 | 0.0587 | 958.92 | 1277.96 | 3326.66 | 3319.90 |
| 4 | ok | ok | 0.0643 | 0.0643 | 480.19 | 655.63 | 2834.03 | 2913.43 |
| 5 | ok | ok | 0.0614 | 0.0614 | 354.33 | 468.38 | 2976.73 | 3047.51 |
| 6 | ok | ok | 0.0532 | 0.0532 | 242.84 | 315.97 | 3228.13 | 3449.37 |
| 7 | ok | ok | 0.0532 | 0.0532 | 242.66 | 315.51 | 3230.96 | 3447.18 |
| 8 | ok | ok | 0.0532 | 0.0532 | 242.66 | 315.31 | 3225.72 | 3445.12 |
| 9 | ok | ok | 0.0528 | 0.0528 | 129.81 | 194.65 | 3246.91 | 3467.39 |
| 10 | ok | ok | 0.0528 | 0.0528 | 130.06 | 194.87 | 3252.04 | 3461.76 |
| 11 | ok | ok | 0.0458 | 0.0467 | 32.64 | 34.69 | 4324.98 | 4406.44 |
| 12 | ok | ok | 0.0431 | 0.0439 | 29.36 | 31.97 | 4682.30 | 4598.74 |
| 13 | ok | ok | 0.0436 | 0.0442 | 20.99 | 22.83 | 4433.51 | 4471.89 |
| 14 | ok | ok | 0.0384 | 0.0385 | 9.83 | 9.91 | 2856.98 | 3338.61 |
| 15 | ok | ok | 0.0393 | 0.0442 | 7.55 | 8.08 | 3968.54 | 4176.13 |
| 16 | ok | ok | 0.0432 | 0.0432 | 15.25 | 16.15 | 3821.14 | 4076.04 |
| 17 | ok | ok | 0.0393 | 0.0441 | 7.55 | 7.99 | 3674.69 | 3947.14 |
| 18 | ok | ok | 0.0393 | 0.0441 | 7.55 | 7.99 | 3671.67 | 3944.63 |
| 19 | ok | ok | 0.0393 | 0.0441 | 7.54 | 8.00 | 3675.20 | 3951.31 |
| 20 | ok | ok | 0.0393 | 0.0441 | 7.55 | 7.99 | 3668.09 | 3948.67 |
| 21 | ok | ok | 0.0393 | 0.0441 | 7.55 | 7.98 | 3670.28 | 3951.73 |
| 22 | ok | ok | 0.0393 | 0.0441 | 7.53 | 7.99 | 3673.11 | 3940.28 |
### Rust First-Block Stage Timing

- Samples the first raw `block_size` chunk only, so the timing breakdown stays aligned with the real block-local hot path.
- Uses prepared dictionaries for dictionary-backed cases so the sample reflects encoder hot paths instead of repeated dictionary parsing.
- The stage table above is sampled with the planner's phase timers off, so its milliseconds and its shares are both the real encoder's. The two sub-breakdown tables below need those timers and are sampled separately, because a timer taken per lazy parser step costs far more than the step: with them on, this case's first block reads up to 18x its real time and 99% of the frame lands in `Plan`. Read the sub-breakdowns as shares of their own row and never against the table above.
- The planning sub-breakdown covers row and chain/extdict lazy paths; other planner families may still report zeros. The lazy parser phase sub-breakdown is instrumented for no-dict row and trained-dictionary chain/extdict cases, and likewise reports zeros elsewhere.
- Sampled on levels 3-7 over 3 iterations.

| Level | Sampled ms | Blocks | Compressed | Split % | Plan % | Lit % | Seq % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.51 | 1 | 1 | 0.0 | 74.1 | 4.4 | 18.3 | 3.1 |
| 4 | 0.76 | 1 | 1 | 0.0 | 84.7 | 2.7 | 12.0 | 0.7 |
| 5 | 1.12 | 1 | 1 | 0.0 | 89.7 | 2.0 | 7.9 | 0.3 |
| 6 | 1.44 | 1 | 1 | 0.0 | 92.3 | 1.9 | 5.3 | 0.5 |
| 7 | 1.39 | 1 | 1 | 0.0 | 92.2 | 1.9 | 5.3 | 0.6 |

### Rust First-Block Decode Timing

- Profiles Rust decode against the same upstream-produced frame family used by the decode throughput benchmark.
- Uses prepared dictionaries for dictionary-backed cases so decode attribution stays on block decode instead of dictionary parsing.
- Read these as proportions, not costs. Timing each stage separately requires decoding sequence commands into a buffer and then executing them, where the real decoder fuses the two into one pass and runs several times faster. The MiB/s column above is the real path; this table is not.
- Sampled on levels 3-7 over 3 iterations, and only on the first block. Rows in the decode column are whole frames, so this cannot by itself explain one.

| Level | Sampled ms | Blocks | Compressed | Lit % | SeqTable % | SeqCmd % | Exec % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 1.29 | 1 | 1 | 1.3 | 1.7 | 7.1 | 89.4 | 0.4 |
| 4 | 1.31 | 1 | 1 | 1.3 | 1.7 | 7.0 | 89.5 | 0.5 |
| 5 | 1.19 | 1 | 1 | 1.5 | 1.8 | 7.1 | 89.2 | 0.4 |
| 6 | 0.99 | 1 | 1 | 2.1 | 2.2 | 7.2 | 88.0 | 0.5 |
| 7 | 0.99 | 1 | 1 | 1.9 | 2.1 | 7.1 | 88.4 | 0.5 |

| Level | LitCopy % of exec | PrefixMatch % | DictMatch % | Exec Other % |
| ---: | ---: | ---: | ---: | ---: |
| 3 | 24.2 | 22.9 | 0.2 | 52.6 |
| 4 | 22.6 | 21.0 | 0.1 | 56.3 |
| 5 | 22.6 | 21.0 | 0.2 | 56.2 |
| 6 | 23.2 | 21.7 | 0.1 | 55.0 |
| 7 | 23.1 | 21.6 | 0.1 | 55.2 |

| Level | Row % of plan | Chain % of plan | Match % | Rep % | Insert % | Parser % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 0.0 | 9.6 | 1.3 | 10.4 | 5.4 | 64.7 |
| 5 | 0.0 | 19.1 | 4.2 | 8.9 | 6.8 | 55.3 |
| 6 | 0.0 | 23.7 | 6.5 | 8.3 | 6.8 | 50.5 |
| 7 | 0.0 | 23.6 | 6.5 | 8.3 | 6.8 | 50.6 |

| Level | Base Rep % of parser | Base Reg % | Continue % | Store % | Rep2 % | Other % |
| ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| 3 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 | 0.0 |
| 4 | 18.9 | 15.7 | 0.0 | 5.8 | 13.8 | 45.8 |
| 5 | 14.4 | 15.5 | 16.2 | 4.3 | 9.8 | 39.9 |
| 6 | 12.0 | 12.9 | 27.0 | 3.5 | 7.1 | 37.6 |
| 7 | 12.1 | 12.9 | 27.0 | 3.5 | 7.1 | 37.4 |

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


