#![deny(unsafe_code)]
#![warn(missing_docs)]
#![cfg_attr(docsrs, feature(doc_cfg))]

//! Pure Rust Zstandard codec.
//!
//! `zstandard` implements the [Zstandard compression
//! format](https://datatracker.ietf.org/doc/html/rfc8878) without any C
//! dependency on the codec path. Frames produced by this crate decompress
//! cleanly with the official `zstd` library, and frames produced by `zstd`
//! decompress with this crate, for the format coverage implemented here.
//!
//! The crate is intentionally narrow: the public API exposes one-shot and
//! streaming encode/decode entry points, dictionary support, and a few
//! frame-level helpers. Internal encoder behavior — parser strategies, hash
//! tables, sequence statistics — is not part of the supported surface.
//!
//! # One-shot
//!
//! ```
//! use zstandard::{decode_all, encode_all};
//!
//! let compressed = encode_all(b"hello zstd")?;
//! assert_eq!(decode_all(&compressed)?, b"hello zstd");
//! # Ok::<(), zstandard::Error>(())
//! ```
//!
//! # Configurable encode
//!
//! [`EncoderOptions`] controls compression level, block size, content
//! checksum, and the frame header fields. [`CompressionLevel`] accepts the
//! range `-131072..=22`: `1..=22` selects progressively more thorough parsers,
//! and the negative levels are Zstandard's "fast mode", trading ratio for
//! speed below what level 1 does.
//!
//! ```
//! use zstandard::{CompressionLevel, EncoderOptions, decode_all, encode_all_with_options};
//!
//! let options = EncoderOptions {
//!     compression_level: CompressionLevel::BETTER,
//!     checksum: true,
//!     ..Default::default()
//! };
//! let compressed = encode_all_with_options(b"compress me", options)?;
//! assert_eq!(decode_all(&compressed)?, b"compress me");
//! # Ok::<(), zstandard::Error>(())
//! ```
//!
//! For finer control, [`ParameterOverrides`] replaces individual compression
//! parameters that the level would otherwise choose — window and table sizes,
//! search depth, minimum match, target length, and the parser [`Strategy`].
//! Each parameter's accepted range is published as a [`ParameterBounds`]
//! constant, and a value outside it is reported rather than clamped.
//!
//! ```
//! use zstandard::{EncoderOptions, ParameterOverrides, Strategy, decode_all, encode_all_with_options};
//!
//! let options = EncoderOptions {
//!     parameters: ParameterOverrides {
//!         strategy: Some(Strategy::Lazy2),
//!         window_log: Some(20),
//!         ..Default::default()
//!     },
//!     ..Default::default()
//! };
//! let compressed = encode_all_with_options(b"a smaller window and a chosen parser", options)?;
//! assert_eq!(decode_all(&compressed)?, b"a smaller window and a chosen parser");
//! # Ok::<(), zstandard::Error>(())
//! ```
//!
//! # Dictionaries
//!
//! Both raw-content and formatted Zstandard dictionaries are supported, and a
//! parsed dictionary can be reused across many calls via
//! [`EncoderDictionary`] and [`DecoderDictionary`]. They are separate types
//! because the two directions need different tables built from the same bytes
//! and neither should carry the other's, which is the line upstream draws as
//! `ZSTD_CDict` and `ZSTD_DDict`; to use one dictionary both ways, build both
//! over the same `Arc<[u8]>`. [`train_dictionary`] builds one from sample data.
//!
//! ```
//! use zstandard::{decode_all_with_dict, encode_all_with_dict};
//!
//! let dictionary = b"shared prefix dictionary";
//! let payload = b"shared prefix dictionary + payload";
//!
//! let compressed = encode_all_with_dict(payload, dictionary)?;
//! assert_eq!(decode_all_with_dict(&compressed, dictionary)?, payload);
//! # Ok::<(), zstandard::Error>(())
//! ```
//!
//! # Streaming
//!
//! [`StreamingEncoder`] and [`StreamingDecoder`] consume input in chunks and
//! produce output progressively. Both contexts can be reused for additional
//! frames via `reset()` after `finish()`.
//!
//! ```
//! use zstandard::{DecoderOptions, EncoderOptions, StreamingDecoder, StreamingEncoder};
//!
//! let input = b"streaming example data".repeat(1_000);
//!
//! let mut encoder = StreamingEncoder::new(EncoderOptions::default())?;
//! let mut compressed = encoder.take_output();
//! for chunk in input.chunks(1_001) {
//!     encoder.push(chunk)?;
//!     compressed.extend_from_slice(&encoder.take_output());
//! }
//! encoder.finish()?;
//! compressed.extend_from_slice(&encoder.take_output());
//!
//! let mut decoder = StreamingDecoder::new(DecoderOptions::default());
//! let mut restored = Vec::new();
//! for chunk in compressed.chunks(113) {
//!     decoder.push(chunk)?;
//!     restored.extend_from_slice(&decoder.take_output());
//! }
//! decoder.finish()?;
//! restored.extend_from_slice(&decoder.take_output());
//! assert_eq!(restored, input);
//! # Ok::<(), zstandard::Error>(())
//! ```
//!
//! # `std::io` adapters
//!
//! [`io::Writer`] and [`io::Reader`] wrap the streaming contexts in the
//! `std::io` traits, so the codec composes with `io::copy`, `BufReader`, and
//! anything else that speaks `Read`/`Write`.
//!
//! ```
//! use std::io::{Read, Write};
//! use zstandard::io::{Reader, Writer};
//!
//! let mut writer = Writer::new(Vec::new())?;
//! writer.write_all(b"through std::io")?;
//! let compressed = writer.finish()?;
//!
//! let mut restored = Vec::new();
//! Reader::new(&compressed[..]).read_to_end(&mut restored)?;
//! assert_eq!(restored, b"through std::io");
//! # Ok::<(), std::io::Error>(())
//! ```
//!
//! # Errors
//!
//! All fallible operations return [`Result<T>`](Result) (alias for
//! `Result<T, Error>`). [`Error`] reports malformed input, configuration
//! mistakes, and decoder limits that the input would exceed; the decoder is
//! the security boundary, so soundness on adversarial input is treated as a
//! bug — see `SECURITY.md`.
//!
//! # Cargo features
//!
//! - `internal-trace` *(maintainer-only, hidden)* — enables block traces and
//!   stage-profiling APIs used by the project's benchmark and parity tools.
//!   Not part of the supported surface.
//! - `internal-fuzz` *(maintainer-only, hidden)* — enables a `fuzz` module
//!   exposing internal entry points consumed by the fuzz harness.
//! - `asm-inspect` *(maintainer-only)* — enables an `asm_inspect` module of
//!   `#[no_mangle]` shims used to inspect generated assembly.
//!
//! # MSRV
//!
//! Rust 1.96, as declared by `rust-version` in `Cargo.toml` and checked by CI.
//! The 2024 edition's own floor is 1.85; this crate's is higher.

// Not `pub`: these shims exist to put named symbols in `--emit asm` output,
// which `#[unsafe(no_mangle)]` guarantees on its own regardless of Rust
// visibility. Exporting them publicly bought nothing and leaked `BitDStream`,
// `DState` and `SequenceDecodeEntry` — all `pub(crate)` — through a public
// signature.
#[cfg(feature = "asm-inspect")]
pub(crate) mod asm_inspect;
mod block;
mod decode;
mod decode_out;
mod dict_builder;
mod dictionary;
// Compiles the README's Rust examples as doctests. They are the first code a
// newcomer runs, so they are held to the same standard as the examples in this
// file rather than being prose that happens to look like Rust. `cfg(doctest)`
// keeps the item out of every other build, including the rendered docs.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeExamples;

mod encode;
mod entropy;
mod error;
mod frame;
#[cfg(feature = "internal-fuzz")]
#[doc(hidden)]
pub mod fuzz;
pub mod io;
mod literals;
mod outbuf;
mod sequence;
mod streaming;
mod window;
mod xxhash;

pub use block::{BLOCK_SIZE_MAX, BlockHeader, BlockType, parse_block_header};
pub use decode::{
    Decoder, DecoderOptions, decode_all, decode_all_with_dict, decode_all_with_dict_and_options,
    decode_all_with_options, decode_all_with_prepared_dict,
    decode_all_with_prepared_dict_and_options, decode_into_slice, decode_into_slice_with_options,
};
pub use dict_builder::{
    DICTIONARY_SIZE_MIN, DictionaryTrainingParameters, TrainedDictionary, train_dictionary,
    train_dictionary_with_parameters,
};
pub use dictionary::{DecoderDictionary, EncoderDictionary};
pub use encode::{
    CompressionLevel, Encoder, EncoderOptions, LiteralCompressionMode, ParameterBounds,
    ParameterOverrides, RowMatchFinderMode, Strategy, compress_bound, encode_all,
    encode_all_with_dict, encode_all_with_dict_and_options, encode_all_with_options,
    encode_all_with_prepared_dict, encode_all_with_prepared_dict_and_options, encode_into_slice,
};
pub use error::{Error, Result};
pub use frame::{
    Format, FrameHeader, SkippableFrame, ZSTD_MAGIC_NUMBER, ZstandardFrameHeader, decompress_bound,
    decompress_bound_with_format, decompressed_size, decompressed_size_with_format,
    find_frame_compressed_size, find_frame_compressed_size_with_format, parse_frame_header,
    parse_frame_header_with_format, write_skippable_frame,
};
pub use streaming::{StreamingDecoder, StreamingEncoder};
pub use window::LdmMode;

// Maintainer-only diagnostic surface: block traces, parser stats, and
// per-stage profiling. Hidden from rustdoc and from the default public API
// because the shapes are tied to internal encoder/decoder structure and
// change frequently. Enable the `internal-trace` feature to opt in.
#[cfg(feature = "internal-trace")]
#[doc(hidden)]
pub use decode::{
    DecodeStageProfile, profile_first_block_decode_with_options,
    profile_first_block_decode_with_prepared_dict_and_options, profile_frame_decode_with_options,
    profile_frame_decode_with_prepared_dict_and_options,
};
#[cfg(feature = "internal-trace")]
#[doc(hidden)]
pub use dict_builder::{DictionaryEntropyHistograms, trace_dictionary_entropy_stats};
#[cfg(feature = "internal-trace")]
#[doc(hidden)]
pub use encode::{
    BlockTrace, BlockTraceAcceptedRegularMatch, BlockTraceCompressionParameters,
    BlockTraceDecision, BlockTraceDictionaryMode, BlockTraceDictionaryTableSource,
    BlockTraceEmittedMatch, BlockTraceEmittedMatchKind, BlockTraceMatchSource, BlockTraceMode,
    BlockTraceParserStats, BlockTraceParserStrategy, BlockTraceRegularMatchSourceCounts,
    BlockTraceRepMatchSourceCounts, BlockTraceRepcodeStats, BlockTraceRowSearchContest,
    BlockTraceSequenceModes, BlockTraceUpstreamStrategy, EncodeStageProfile, PlannerPhases,
    profile_first_block_with_options, profile_first_block_with_prepared_dict_and_options,
    trace_first_block_with_options, trace_first_block_with_prepared_dict_and_options,
};
#[cfg(feature = "internal-trace")]
#[doc(hidden)]
pub use literals::{LiteralsSectionLayout, parse_literals_section_layout};
