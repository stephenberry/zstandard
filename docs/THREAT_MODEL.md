# Threat Model

`zstandard` is a codec. Decoder soundness is treated as a security property.
This document records the assumptions the maintainers make about how the
crate is used and what the crate promises in return.

## Assumed call sites

| Component             | Trust assumption                                            |
| --------------------- | ----------------------------------------------------------- |
| **Decoder input**     | Hostile. Any byte sequence may be passed in.                |
| **Decoder dictionary**| Trusted. The application chose which dictionary to attach.  |
| **Encoder input**     | Trusted. The application controls what it asks to compress. |
| **Encoder options**   | Trusted. Caller-supplied configuration.                     |
| **Encoder dictionary**| Trusted. Same as encoder input.                             |

The only adversarial position the threat model defends against is bytes fed
to a decoder. Encoder behavior on adversarial *configuration* is treated as
caller error: passing nonsense compression levels, oversized window logs, or
malformed dictionaries returns an `Error`, but is not a security boundary.

## What the decoder guarantees

For any byte sequence passed to `decode_all`, `decode_all_with_options`,
`decode_all_with_dict`, `decode_into_slice`, the streaming
`StreamingDecoder::push`/`finish`, or the frame-level helpers
(`parse_frame_header`, `parse_block_header`, `find_frame_compressed_size`,
`decompress_bound`, `decompressed_size`):

1. **No panics.** No `unwrap`, `expect`, indexing out of bounds, integer
   overflow, or arithmetic-on-overflow panics. The decoder either succeeds,
   returns an `Error`, or (in streaming) reports it has consumed input
   without error and is waiting for more.
2. **Confined `unsafe`.** The crate root is `#![deny(unsafe_code)]`, but that
   lint is silenced by an inner `#[allow]` (unlike `forbid`), and the crate
   uses 146 such escapes. They are concentrated in the entropy coders
   (`src/entropy/`), the match finders (`src/window/`), and the sequence
   execution loop (`src/sequence.rs`), and cover unchecked indexing,
   unaligned loads, and wide copies on hot paths.

   This matters here because `src/sequence.rs` executes attacker-controlled
   sequences, so it sits directly on the decode path for untrusted input.
   Bounds are validated before the unchecked region is entered — an offset or
   match length that would leave the output window is rejected first — but
   memory safety on malformed input depends on those checks being right, not
   on the compiler. A soundness bug here is a vulnerability, not a defect;
   report it per `SECURITY.md`.

   Those checks are also only as good as *when* they run. The first Miri pass
   over this crate found four places where the check was correct but the
   arithmetic it guarded had already happened: a literal cursor advanced by an
   unvalidated length before being compared against the end of the literals, a
   match prefetch built from an offset validated further down, and a bitstream
   limit computed as `start + size_of::<usize>()` on a stream shorter than a
   word. `pointer::add` is undefined the moment it leaves the allocation, so
   none of those survived to the check that would have caught them. Every one
   produced correct output; a fuzzer had run them for hours without complaint.
   `cargo miri test --test miri` sweeps for this weekly, and is worth running
   directly on any change that touches `unsafe`.
3. **Bounded memory.** Output and intermediate allocations grow at most as a
   function of declared frame parameters (window size, content size). The
   decoder enforces a configurable `DecoderOptions::max_window_size` so a
   single frame cannot trick the application into allocating arbitrarily
   large buffers.

   For the streaming decoder that bound is roughly twice the declared window, plus whatever the caller has not drained. It holds the frame's match history in the same buffer the caller reads from, and releases the front only once half the buffer is releasable — waiting is what keeps each byte from being moved more than once on average, and the price of waiting is that the buffer settles at twice the history it has to keep rather than exactly it.
4. **Bounded time per byte of input.** A pathological `.zst` cannot push the
   decoder into a loop that consumes no input. Decompression is linear in
   output size for a fixed maximum block size, on the streaming path as well as
   the one-shot one.

   The streaming qualifier is there because it did not hold: retiring history
   with `Vec::drain(..1)` made decode cost output times window, so a 329-byte
   frame could hold the decoder for 42 seconds. Nothing detected it, because a
   fuzzer without a timeout sees a slow input and a fast one alike, and the
   benchmarks only ever measured `decode_all`. What guards it now is an explicit
   time bound in `streaming_decode_of_a_window_filling_frame_is_not_quadratic`;
   a claim about asymptotics needs a test that fails when the asymptotics change.
5. **Format compliance is enforced.** Frames whose content checksum does not
   match are rejected. Frames whose declared content size does not match
   what was decoded are rejected. Reserved bits, illegal block sizes, and
   out-of-range entropy table parameters all produce `Error::Corruption(...)`.

## What the decoder does *not* defend against

- **Memory pressure from intentionally large but valid frames.** A frame
  declaring a 2 GB content size is valid; the decoder will allocate roughly
  that. Set `DecoderOptions::max_window_size` and consider streaming with a
  cap on output buffering for adversarial workloads.
- **Side channels.** Timing, cache-pattern, and power-analysis side channels
  are out of scope. Do not use this crate to encrypt, authenticate, or
  protect secrets that an attacker can observe execution against.
- **Decompression bombs that conform to the format.** A 4 KB `.zst` that
  legitimately expands to 100 GB is not a bug; the application chose to
  decompress it. Wrap calls in resource limits (process memory, output
  truncation) when input is untrusted.
- **Bugs in the host platform's standard library or allocator.**

## What the encoder guarantees

The encoder is not a security boundary. It does, however, promise:

- **Output is decodable by the official `zstd` C library** within the
  feature set the encoder supports. Interop is verified by tests.
- **Output round-trips through the crate's own decoder** for any input the
  encoder accepted.
- **No data leakage.** Encoder output depends only on input, options, and
  dictionary. No uninitialized memory is read and no allocator state reaches
  the output. The encoder's `unsafe` blocks write to buffers it has already
  sized and initialized; this is an invariant maintained by review and tests,
  not one the compiler checks.

## Reporting

If you find any failure of the decoder guarantees above, please follow
`SECURITY.md` instead of opening a public issue.

## Verification

The properties above are verified by:

- The standing test suite (`tests/codec.rs`, `tests/upstream_interop.rs`).
- The fuzz targets in `fuzz/fuzz_targets/`, which exercise frame parsing,
  literal parsing, sequence parsing, and the full decode path on random
  inputs.
- The CI lint job, which fails the build if the number of
  `#[allow(unsafe_code)]` escapes grows, and which enforces documentation
  coverage. Note that `deny(unsafe_code)` alone cannot enforce this, because
  an inner `#[allow]` silences it.

The CI fuzz workflow runs each target nightly. Sustained fuzzing through
OSS-Fuzz is on the roadmap.
