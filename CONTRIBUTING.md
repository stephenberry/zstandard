# Contributing to zstandard

Thanks for your interest in `zstandard`. This document explains how to set up the
repo, what is expected from a contribution, and the engineering rules that
keep the codec correct and interoperable.

## Ground Rules

These rules apply to every change:

1. **Decoder correctness comes before encoder cleverness.** A decoder that
   accepts every well-formed frame and rejects every malformed one matters
   more than encoder micro-wins.
2. **Every new format feature gets upstream interoperability coverage.** If
   the encoder learns a new trick, prove the official `zstd` library can
   decode it; if the decoder accepts something new, prove `zstd` produces it.
3. **Every new corruption rule gets at least one negative test.**
4. **Public APIs stay narrow until the internal model is settled.** Prefer
   widening internal options before exposing new public knobs.
5. **Performance claims only count after benchmarking against upstream.** Every
   benchmarked level holds encode and decode throughput at 50% of the C library
   or better, and compression ratio equal to it or better.
6. **The C library is the reference, never the implementation.** Nothing in the
   public encode or decode path may call `libzstd`: no FFI fast path, no
   C-backed fallback for a benchmarked workload, no release-only delegation. A
   benchmark row counts only when Rust did the work. Studying upstream,
   generating reference output, tracing its behavior, and benchmarking against
   it are all expected, and reproducing its logic in Rust is the job.

## Repository Layout

- `src/` — the codec itself. Modules are split by wire-format concern
  (`frame`, `block`, `literals`, `sequence`, `entropy`, `window`, `streaming`,
  `dictionary`).
- `tests/` — integration tests, including upstream interop coverage.
- `benches/` — Criterion benches comparing against the official C library.
- `fuzz/` — `cargo-fuzz` targets.
- `scripts/`, `dev/`, `oracles/` — developer tooling, not published.
  `oracles/` holds the shell drivers and C harnesses that diff this crate's
  assembly, resolved parameters, and long-distance sequences against the
  pinned upstream checkout.

## Local Setup

```sh
git clone https://github.com/stephenberry/zstandard
cd zstandard
cargo build
cargo test
```

For interop tests and benchmarks you also need a checkout of the official
`zstd` repository, **at the revision named in `upstream-zstd.ref`**:

```sh
git clone https://github.com/facebook/zstd upstream-zstd
cd upstream-zstd && git checkout "$(cat ../upstream-zstd.ref)" && make -j lib
cd ..
```

`upstream-zstd/` inside the crate is gitignored, is the layout CI provisions,
and is the first place the harness looks, so a checkout there needs no further
configuration. A checkout at `../zstd` is still accepted as a fallback, but
prefer the in-crate one: `../zstd` is usually whatever you have pulled for your
own work, and it only has to drift one commit off the pin for every parity test
to stop comparing anything.

The interop helpers compile small C programs into `/tmp` against this
checkout. Tests that depend on it skip cleanly when it is missing.

The revision matters. Upstream changes its level mapping, parser heuristics,
and block splitter between releases, so the same input legitimately compresses
to different bytes across them — comparing against an arbitrary checkout
produces failures that say nothing about this crate. The harness therefore
verifies the checkout is at the pinned revision and skips, with an explanation,
when it is not. `upstream-zstd.ref` is the single source of truth: CI reads the
same file.

Two environment variables control this:

| variable | effect |
| --- | --- |
| `ZSTANDARD_UPSTREAM_DIR` | use this checkout and no other, instead of searching `upstream-zstd/` then `../zstd`. Setting it means a checkout at the wrong revision is reported rather than skipped past |
| `ZSTANDARD_REQUIRE_UPSTREAM` | turn "skip" into a hard failure; CI sets this on the one leg that provisions the checkout, so the parity suite cannot silently stop comparing |

To bump the pin, change `upstream-zstd.ref`, re-run the suite against the new
revision, and update whatever parity baselines moved (see
`KNOWN_UPSTREAM_SIZE_GAPS` in `src/encode.rs`) in the same commit.

## Running Tests

```sh
cargo test                              # unit + integration
cargo test --all-features               # exercise feature-gated paths
cargo bench --bench interop -- --quick  # benchmark smoke run
```

If you add an `include!` or a `#[path]` module to anything under `src/`, check
that the published crate still compiles its own tests:

```sh
cargo package && cargo test --manifest-path "$(ls -d target/package/zstandard-*/ | tail -1)Cargo.toml"
```

`cargo package` on its own does not answer this. It verifies that the packaged
crate builds, and a build never compiles `#[cfg(test)]` code, so a test module
that reaches a file the `exclude` list keeps out of the tarball passes it. CI
runs the pair in the `package` job.

There are no `#[ignore]`d tests. The suite used to carry three, asserting
byte-exact parity with upstream on the trained-dictionary path, and they now
pass; if a parity goal is ever parked that way again it should say in its doc
comment what has to change for it to run.

An optimization is not finished when a number moves. Before claiming one, run
the suite, regenerate the report, and confirm three things: that every
benchmarked level still holds the gate in Ground Rule 5, that the rows which
moved are the ones the change can explain, and that the gain came from Rust
rather than from a changed measurement. A row that moves with no mechanism
behind it is a measurement, not a result.

Generate the markdown benchmark report:

```sh
cargo run --release --features internal-trace --bin benchmark_report -- --output BENCHMARKS.md
```

The README's comparison chart is derived from that file, so regenerate it in the
same commit:

```sh
python3 scripts/plot_benchmarks.py
```

It stamps the report's own recorded revision onto the image. A chart whose stamp
is behind `git log` is visibly stale rather than quietly wrong, so do not edit
`assets/benchmarks.svg` by hand.

To attribute one row rather than regenerate every row:

```sh
cargo run --release --features internal-trace --bin profile_decode_stage -- \
  --case json-records --level 16 --throughput
```

`--throughput` times the real decode path and reports it both allocating a fresh
output buffer and reusing one. The default mode instead splits a whole frame's
decode into literals, sequence tables, sequence commands, and execution. Read
that split as proportions only: stage attribution needs each phase timed
separately, so it decodes sequence commands into a buffer and then executes
them, where the real decoder fuses the two into one pass and runs several times
faster. `--first-block-only` restricts it to the first block, which is what to
use when comparing against upstream's first block rather than explaining a
benchmark row.

The decode hot loop's remaining gap against C is a register-allocation problem
rather than a structural one, and
[dev/DECODE_REGISTER_PRESSURE.md](dev/DECODE_REGISTER_PRESSURE.md) records the
instruction and stack-op counts behind that claim. Read it before attempting to
close the gap by restructuring the loop again.

The streaming decoder is a separate implementation and nothing in `BENCHMARKS.md` measures it — the streaming section there is about the *encoder*. It went years at a fraction of one-shot throughput because of that. `profile_streaming_decode` is what to run before believing it is fine:

```sh
cargo run --release --bin profile_streaming_decode -- --case json-records --level 16
```

It reports both paths on the same frame and the ratio between them. `--mode streaming` runs only the streaming path, so a sampling profile attributes every frame to it rather than splitting the profile across both. `--chunk` sets how much compressed input arrives per `push`, which is what decides how often the decoder gets to release history.

Two traps this report has fallen into, both of which produced numbers that
looked like decoder defects:

- **Compare like with like.** The upstream helper hoists its destination buffer
  out of its timing loop. Timing anything against it that allocates per
  iteration measures the allocator, and does so intermittently — the same row
  read 4182 MiB/s alone and 2353 MiB/s inside a seven-level sweep.
- **Best-of-three does not fix a systematic error.** It removes background-load
  noise, which is random and one-directional. It does nothing about a cost that
  is present in every trial.

## Fuzzing

Fuzz targets live in `fuzz/`. `cargo fuzz` needs nightly for `-Zsanitizer`, and
CI gets it by installing nightly as the default toolchain; locally you have to
ask for it explicitly:

```sh
cargo install cargo-fuzz
cargo +nightly fuzz run frame_parse
cargo +nightly fuzz run literals_parse
cargo +nightly fuzz run sequence_parse
cargo +nightly fuzz run full_decode
cargo +nightly fuzz run streaming_decode
cargo +nightly fuzz run encode_roundtrip
cargo +nightly fuzz run streaming_encode_roundtrip
cargo +nightly fuzz run dictionary_encode_roundtrip
```

Omitting `+nightly` on a stable-default machine fails at the build step with
"the option `Z` is only accepted on the nightly compiler". Check for that before
believing a run's results: `cargo fuzz build` leaves any previously built target
binary in place, so a failed build followed by a run of the stale binary looks
like a successful run of the code you just edited.

If you find a crash, please file a security report (see `SECURITY.md`) before
opening a public issue.

## Checking `unsafe` With Miri

Fuzzing finds inputs that make the crate crash. Miri finds undefined behavior on
inputs that already work, which is a different and largely disjoint set: a read
one byte past an allocation, or a pointer computed out of bounds, usually
produces a correct answer on the machine you are testing on and a wrong one after
the next inlining decision.

```sh
rustup +nightly component add miri
CARGO_INCREMENTAL=0 MIRIFLAGS="-Zmiri-disable-isolation" \
  cargo +nightly miri test --test miri
```

CI sweeps weekly, which is often enough for latent undefined behavior and too
rare to rely on if you are editing `unsafe`. Run it yourself when you do, or
trigger the workflow with `gh workflow run miri.yml`.

Four things to know before you run it:

- Use `--test miri`, not `--lib`. `tests/miri.rs` is the target sized for an
  interpreter; the rest of the suite uses bodies of 128 KiB and up, and a single
  histogram test in `src/entropy/hist.rs` runs for over twelve minutes under
  Miri without finishing.
- `CARGO_INCREMENTAL=0` matters. Miri's incremental cache went pathological
  during development here — 100% CPU and 6.9 GB of resident memory for over an
  hour, where a clean build of the same tree takes about a minute. If a rebuild
  seems stuck, `rm -rf target/miri`.
- The MIR compile takes longer than a normal build, and any edit to
  `src/sequence.rs`, `src/encode.rs`, or `src/window/` pays it again.
- Miri cannot spawn processes, so everything that shells out to upstream `zstd`
  skips. `upstream_dir_or_skip` handles that in one place, and reports it on
  stderr the same way it reports a missing checkout.

When adding coverage, prefer a new small case in `tests/miri.rs` over enlarging
an existing one. Miri's cost is superlinear in body length at the btopt and
btultra levels, where the parser runs a dynamic program over every position:
taking those bodies from 6 KiB to 2 KiB cut a full level sweep from 23 minutes to
5 while visiting exactly the same code. Bytes buy compression quality, which that
file does not check. Reach for inputs that cross a block boundary or hit a path
for the first time, not ones that repeat a path already taken.

## Style and Lints

- `cargo fmt --all` before pushing.
- `cargo clippy --all-targets --all-features -- -D warnings` must pass.
- `unsafe` is confined to the entropy coders (`src/entropy/`), match finders
  (`src/window/`), and sequence execution (`src/sequence.rs`), where each use
  carries its own `#[allow(unsafe_code)]`. CI fails if the total count grows.
  New `unsafe` — especially outside those modules — requires discussion with
  maintainers, a strong justification, and a `// SAFETY:` comment stating the
  precondition and why it holds.
- Comments should explain *why*, not *what*. Default to no comment.

## Pull Requests

- Keep PRs focused. A bug fix and a refactor belong in separate PRs.
- Update `CHANGELOG.md` under the topmost unreleased version for any user-visible change. Write it for someone deciding whether to upgrade, not for the reviewer: what changed for a caller, and what they have to do about it. Detail that only a maintainer needs belongs in the commit message.
- If your change touches the encoder, run `cargo bench --bench interop` and
  mention any ratio or throughput delta in the PR description.
- If your change touches the decoder, confirm the upstream interop tests still
  pass (or explain why they intentionally diverge).

## Reporting Issues

Use GitHub issues for bugs, feature requests, and questions. For anything that
might be a security vulnerability — decoder crashes on adversarial input,
out-of-bounds reads, infinite loops, or unbounded memory growth on malformed
frames — follow `SECURITY.md` instead of filing a public issue.

## License

By contributing, you agree that your contributions are licensed under the
project's dual `MIT OR Apache-2.0` terms.

Do not paste code from the reference C implementation, or from any other
codebase, into a contribution. Studying upstream to understand a heuristic and
then writing Rust is what this project does; copying its source is not, and it
would put the license above in question. `ATTRIBUTION.md` records that
relationship and must stay accurate.
