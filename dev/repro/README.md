# Reproducers

Inputs that trigger a known open defect, kept so the defect can be re-triggered
without rediscovering it. Each is a fuzz target input: the first bytes are the
control prefix the target splits off, the rest is the body seed.

## `btlazy2-quadratic-fuzzer-found.bin`

Six bytes for the `encode_roundtrip` target. libFuzzer found it in under five
minutes once the encode body cap in `src/fuzz.rs` was raised past its shipped
per-family values.

```sh
cargo +nightly fuzz run encode_roundtrip dev/repro/btlazy2-quadratic-fuzzer-found.bin
```

It decodes to level 15, `block_size: 1031`, amplification on, over a body of
`tile_with_drift(&[0x9f, 0xb1, 0x9f], n)`. Encoding cost grows as roughly
`n^2.3` once `n` passes 256 KiB, so at the shipped caps the target never reaches
the size where it bites. What causes the growth is not established beyond that
curve.
