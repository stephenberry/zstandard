# Security Policy

`zstandard` is a codec. Its decoder is meant to consume bytes from untrusted
sources, so we treat decoder soundness as a security property, not just a
correctness one.

## Supported Versions

The project is in active pre-1.0 development. Only the latest published `0.x`
release on crates.io receives security fixes. After 1.0 this policy will
expand to cover the most recent minor line.

| Version | Supported |
| ------- | --------- |
| 0.1.x   | yes       |
| < 0.1   | no        |

## What Counts as a Vulnerability

Please report any of the following privately before discussing them publicly:

- Decoder panics, aborts, or unwinds on adversarial input.
- Out-of-bounds reads or writes. The entropy coders, match finders, and
  sequence execution loop use `unsafe` for unchecked indexing and wide copies,
  and `src/sequence.rs` runs on attacker-controlled input, so a missing bounds
  check there is directly exploitable.
- Infinite loops, unbounded recursion, or other denial-of-service issues
  triggered by a malformed or maliciously crafted frame.
- Unbounded memory growth disproportionate to the input size (decompression
  bombs beyond what the format itself permits).
- Encoder behavior that emits frames the official `zstd` library rejects, or
  that decode to bytes other than the input.
- Any `unsafe` block whose safety precondition can be violated by crafted
  input, or any new `unsafe` that escapes review.

Bugs that only affect the encoder when given trusted input, or that show up
only in benchmarks, are normal issues — please open a regular GitHub issue.

## How to Report

Email **stephenberry.developer@gmail.com** with:

- a short description of the issue,
- the smallest reproducer you can produce (a `.zst` blob, a fuzz seed, or a
  minimal Rust snippet),
- the version of `zstandard` you tested against, and
- whether you would like to be credited in the eventual advisory.

You can also use GitHub's private security advisory flow at
<https://github.com/stephenberry/zstandard/security/advisories/new>.

We aim to acknowledge a report within seven days and to ship a fix or a
mitigation plan within thirty days for confirmed vulnerabilities. Please give
us a reasonable disclosure window before publishing details.

## Out of Scope

- Vulnerabilities in the official `zstd` C library or in the upstream
  Zstandard format specification.
- Performance regressions, even severe ones, when triggered by valid input.
- Issues that depend on a build flag the user explicitly opted into (for
  example `internal-fuzz` or `asm-inspect`).
