# Reproducers

Inputs kept so a finding can be re-triggered without rediscovering it. Each is a
fuzz target input: the first bytes are the control prefix the target splits off,
the rest is the body seed.

## `btlazy2-quadratic-fuzzer-found.bin`

Six bytes for the `encode_roundtrip` target. libFuzzer found it in under five
minutes once the encode body cap in `src/fuzz.rs` was raised past its shipped
per-family values.

```sh
cargo +nightly fuzz run encode_roundtrip dev/repro/btlazy2-quadratic-fuzzer-found.bin
```

It decodes to level 15, `block_size: 1031`, amplification on, over a body of
`tile_with_drift(&[0x9f, 0xb1, 0x9f], n)`: a three-byte seed tiled with one byte
perturbed per tile. Encoding cost grows superlinearly once `n` passes 256 KiB,
so at the shipped caps the target never reaches the size where it bites.

**This is not a defect of this crate.** Upstream `v1.5.7` costs the same time on
the same bytes, both sides driven through the same block boundaries -- ours by
`EncoderOptions::block_size`, C's by a `ZSTD_compressStream2` flush every `bs`
bytes, since nothing else makes the C encoder cut where we cut. Ours against
upstream: 215 ms against 246 ms at 512 KiB, 1076 against 1224 at 1 MiB, 2987
against 3402 at 2 MiB. Within 15% at every size and faster at the top.

The time is the DUBT match finder of `zstd_lazy.c` (`ZSTD_updateDUBT` and
`ZSTD_DUBT_findBestMatch`, not the `zstd_opt.c` tree), which levels 13 to 15
select; 99.6% of samples sit in `<BinaryTreeFinder as LazySearchFinder>::find_match`.
The knee is the cparams tier boundary at 256 KiB (`upstream_cparams_tier`,
`src/encode.rs:661`), which is inclusive: 256 KiB exactly takes 1.8 ms and one
byte more takes 34.7 ms, because that byte moves level 15 off the reduced tier
and onto `windowLog 22, chainLog 23, searchLog 6`. Cost tracks block size
against the data's period rather than size alone -- at 512 KiB, `block_size`
1024 takes 2.2 ms and 1031 takes 222 ms.

Kept for two reasons. It sets the fuzz budget: `body_cap_for` stops at 320 KiB
for levels 5 and up, which is 61 ms per body here against 2 s at 1.5 MiB. And
the shape is alarming enough to be rediscovered, so it is worth being able to
re-price it rather than hunt for a bug that is not there. Price anything against
upstream before reading it as a defect, and never compare a fuzz-build timing
with anything else: the fuzz build is about 29x release cost here, and a debug
build 78x.

## `row-hash-salt-reuse-fuzzer-found.bin`

456 bytes for the `dictionary_encode_roundtrip` target: eight control bytes and
a 448-byte body. The dictionary split byte is zero, so the dictionary is empty
and the body is the rest of the file.

```sh
cargo +nightly fuzz run dictionary_encode_roundtrip dev/repro/row-hash-salt-reuse-fuzzer-found.bin
```

It decodes to level 7 with `min_match: 7` and the row match finder forced on,
and it failed the target's reused-encoder assertion: 108 bytes on the first
encode, 109 on the second, 108 on the third. `RowHashFinder::reset` rotated its
hash salt rather than restoring it, following C's `ZSTD_advanceHashSalt()`, so
the second frame filed the same bytes into different rows. The dictionary is
incidental -- an empty one takes the same contiguous path as none -- and
`encode_all_with_options` reproduces it just as well.

Kept because the body is not reducible to a generator. The divergence needs a
tag collision that survives the salt change, which depends on the exact bytes:
eight pattern generators over eight sizes, eight levels and every `min_match`
reproduced it on none of them, while the fuzzer found it in five minutes. It is
also what `reusing_an_encoder_with_the_row_match_finder_reproduces_a_fresh_encode`
encodes, via `include_bytes!`, so this file is load-bearing rather than
archival.

## `dictionary-offset-outlives-window-fuzzer-found.bin`

1461 bytes for the `dictionary_encode_roundtrip` target: eight control bytes,
then a 45-byte dictionary and a 1408-byte body.

```sh
cargo +nightly fuzz run dictionary_encode_roundtrip dev/repro/dictionary-offset-outlives-window-fuzzer-found.bin
```

It decodes to level 7 at `window_log` 10, and produced a frame this crate's own
decoder refused: `sequence offset exceeds the available history window`. Blocks
are capped at the window, so the body splits at 1024. The first block keeps the
dictionary -- C retires it on `blockEndIdx > maxDist + loadedDictEnd`, which is
`1069 > 1069` and false -- and an offset of 1025 found there is legal, because a
dictionary may be referenced in full while its last byte is in the window and
the decoder holds it outside the window. The second block is retired, and that
offset was still `offset_1`, so the parser repeated it at source 1025: one byte
past the window, addressing nothing either side can reach.

The missing step was C's block-start repeat-offset clamp, which it applies under
`if (dictMode == ZSTD_noDict)` -- a guard that catches a retired dictionary too,
since `dictMode` is recomputed per block. Kept because the offset has to be
found in the live block and still be `offset_1` at the first position of the
retired one, which is not a shape worth rediscovering by hand;
`a_dictionary_offset_does_not_outlive_the_window_it_was_found_in` is the same
frame.
