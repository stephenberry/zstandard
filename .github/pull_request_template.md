<!--
Thanks for contributing! Please review CONTRIBUTING.md for the engineering
rules (decoder correctness > encoder cleverness, upstream interop coverage
on every new format feature, negative tests on every new corruption rule).
-->

### Summary

<!-- One or two sentences on what this PR changes and why. -->

### Type of change

<!-- Check all that apply. -->

- [ ] Bug fix
- [ ] New feature / API
- [ ] Performance / encoder competitiveness
- [ ] Docs / tooling / CI
- [ ] Refactor (no user-visible change)

### Verification

- [ ] `cargo test` passes locally
- [ ] `cargo fmt --all` clean
- [ ] `cargo clippy --all-targets --all-features` reviewed
- [ ] If touching the encoder: ran `cargo bench --bench interop -- --quick`; ratio/throughput notes below
- [ ] If touching format-handling code: added or updated upstream interop coverage
- [ ] Updated `CHANGELOG.md` under the topmost unreleased version for any user-visible change

### Benchmark / interop notes

<!-- Paste relevant benchmark deltas, or write "n/a" if not encoder-affecting. -->
