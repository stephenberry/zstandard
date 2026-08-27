---
name: Bug report
about: Report a correctness issue, panic, or unexpected behavior
title: ""
labels: bug
---

<!--
SECURITY: Decoder crashes on adversarial input, out-of-bounds reads, infinite
loops, or unbounded memory growth on malformed frames are SECURITY issues.
Please follow SECURITY.md to report them privately instead of filing here.
-->

### Summary

<!-- One or two sentences describing what went wrong. -->

### Reproducer

<!--
Smallest possible example: a Rust snippet, the exact input bytes (hex or
base64), the API calls used, and the expected vs. actual output.
-->

```rust
// minimal repro
```

### Environment

- `zstandard` version:
- Rust version (`rustc --version`):
- OS / arch:
- Feature flags:

### Additional context

<!-- Stack traces, related upstream zstd behavior, etc. -->
