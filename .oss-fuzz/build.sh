#!/usr/bin/env bash
# Build script for OSS-Fuzz. Compiles each fuzz target in fuzz/fuzz_targets/
# and copies the resulting binary plus a seed corpus into $OUT/.

set -euo pipefail

cd "$SRC/zstandard"

# OSS-Fuzz exports SANITIZER, FUZZING_ENGINE, etc. cargo-fuzz reads them and
# applies the right -C flags via its Rustflags pipeline.
cd fuzz
cargo fuzz build -O --debug-assertions

TARGETS=(frame_parse literals_parse sequence_parse full_decode)
for target in "${TARGETS[@]}"; do
    cp "target/x86_64-unknown-linux-gnu/release/${target}" "$OUT/${target}"
    if [ -d "corpus/${target}" ]; then
        zip -j "$OUT/${target}_seed_corpus.zip" "corpus/${target}"/* || true
    fi
done
