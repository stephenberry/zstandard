# Semver Policy

`zstandard` follows [Cargo's semver
guidelines](https://doc.rust-lang.org/cargo/reference/semver.html). This
document records what the maintainers consider a breaking change and what
they reserve the right to change without one.

## Pre-1.0

While the crate is in `0.x`:

- Each `0.MINOR` bump may break the public API. The crate is not yet stable.
- Each `0.MINOR.PATCH` bump is bug-fix only — no breaking API changes, no
  intentional encoder output changes that would invalidate already-stored
  frames produced by an earlier patch (frames remain decodable).

## Permanent guarantees (every release, including 0.x)

The following are treated as load-bearing public contracts. Breaking them is
always a major-version (or 0.minor in 0.x) change with a CHANGELOG note.

- **Decoder accepts every well-formed Zstandard frame the format
  spec describes** (within the implemented feature surface). New decoder
  releases never reject a frame that the previous release accepted, unless
  the previous behavior was unsound or violated the spec.
- **Decoder rejects every malformed frame in a finite, panic-free way.**
  Reaching `panic!`, `unreachable!`, integer overflow, or `Vec` allocation panic
  on adversarial input is a security bug, not an API change — see
  `SECURITY.md`. Two mechanisms enforce this, and neither covers the other half:
  the fuzz suite catches crashes but has no oracle for a frame that *should*
  have been rejected and quietly was not, so the accept/reject verdict is
  enforced instead by the upstream parity suite, which runs the reference
  decoder over the same input and compares verdicts.
- **Frames produced by the encoder remain decodable by the official `zstd`
  C library.** Interop is verified by the standing test suite.
- **`unsafe` stays confined and does not grow silently.** It is limited to
  the entropy coders, match finders, and sequence execution loop, each use
  individually annotated with `#[allow(unsafe_code)]` and guarded by CI
  against an increase in count. New `unsafe` outside those modules is a
  deliberate decision recorded in the CHANGELOG.

- **`Error` is `#[non_exhaustive]`.** New variants can be added in a minor
  release, so `match` on it must carry a wildcard arm.
- **`EncoderOptions` and `DecoderOptions` gain fields in minor releases.**
  Construct them with `..Default::default()`; an exhaustive struct literal or
  exhaustive destructuring will break when a field is added, and adding
  options is expected work rather than an exceptional event.
- **`CompressionLevel` is backed by `i32`,** matching upstream, and accepts
  `-131072..=22`: the ordinary levels plus the negative "fast mode" ones.
  `CompressionLevel::MIN` is the negative floor and `MIN_POSITIVE` is `1`.
- **`ParameterOverrides` gains fields in minor releases,** for the same reason
  `EncoderOptions` does; construct it with `..Default::default()`. The
  `ParameterBounds` constants on it may *widen* in a minor release as more of
  a parameter's range becomes reachable, and narrow only in a major one.
- **`Strategy` and `Format` are exhaustive.** Both mirror a fixed set in the
  Zstandard format and its reference implementation, so `match` on them needs
  no wildcard arm and a new variant would be a breaking change.

## Not a breaking change

The following can change in any release, including patch releases, without a
semver bump:

- **Exact bytes of encoder output.** Two different versions may produce
  byte-different `.zst` for the same input at the same compression level,
  as long as both decode to the input and both upstream-decode cleanly.
  This applies to compression ratio, internal block split decisions, parser
  strategy heuristics, dictionary-derived match-state contents, and the
  output of internal randomization.
- **Internal compression performance characteristics** — speed, memory use,
  and ratio at any given level may shift in either direction between
  releases. The CHANGELOG calls out intentional regressions or notable
  improvements.
- **Items marked `#[doc(hidden)]` or gated behind a non-default feature**
  (`internal-trace`, `internal-fuzz`, `asm-inspect`). These are maintainer
  diagnostics and are not part of the supported surface.
- **Error message strings inside `Error::Corruption(&'static str)` and
  `Error::InvalidParameter(&'static str)`.** The variants and their shape
  are stable; the human-readable text is not.
- **MSRV bumps within the documented policy** (next paragraph).

## Minimum Supported Rust Version

The current MSRV is **Rust 1.96** (2024 edition). The MSRV is treated as part
of the public API:

- In `0.x`, MSRV bumps require a minor-version release.
- After `1.0`, MSRV bumps require a major-version release.

Bug-fix patch releases will never raise the MSRV.

A policy is only worth as much as the check behind it. This value read 1.85
long after the crate had stopped building on 1.85, because nothing verified it:
CI named an MSRV leg, but the whole workflow was failing for an unrelated
reason, so that leg's result was never read. The MSRV leg of CI is what keeps
this number honest. Treat a change to it as a change to the public API, and
confirm it with `cargo +<msrv> check --all-targets --all-features` before
committing.

## What constitutes a breaking change

The Cargo guidelines linked above are authoritative. The cases that come up
most often for `zstandard`:

- Removing or renaming a `pub` item exposed by `src/lib.rs`.
- Changing the signature of a `pub` function (return type, parameter types
  or count, generic parameters, lifetime bounds).
- Changing the layout of a `pub` struct in a way that breaks struct-update
  syntax (adding a non-`pub` field is fine; making an existing `pub` field
  non-`pub` is not).
- Adding a variant to a non-`#[non_exhaustive]` `pub` enum.
- Changing the trait bounds required of a generic parameter on a `pub` item.
- Tightening (or loosening) the documented contract of a function in a way
  that would surprise an existing caller.

When in doubt, the Cargo guideline doc is the reference; this file just
records the codec-specific clarifications that wouldn't be obvious from it.
