use core::fmt;

/// All errors reported by encoder, decoder, and dictionary paths.
///
/// Most variants represent malformed input or violated configuration limits;
/// the message attached to [`Error::Corruption`] and [`Error::InvalidParameter`]
/// gives a short human-readable cause.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Error {
    /// Catch-all for internal invariants that should not be reachable on valid input.
    Generic,
    /// Caller-supplied configuration was rejected; the message names the offending parameter.
    InvalidParameter(&'static str),
    /// A destination buffer was too small to hold the requested output.
    DstSizeTooSmall,
    /// The size of a source buffer disagrees with what the format encodes.
    SrcSizeWrong,
    /// Input ended before the next required byte.
    UnexpectedEof,
    /// A frame header started with an unrecognized magic number; carries the bytes seen.
    BadMagic(u32),
    /// Input bytes violated the Zstandard format; the message names the violation.
    Corruption(&'static str),
    /// An entropy table declared a `tableLog` larger than the format permits.
    TableLogTooLarge,
    /// An entropy table declared a maximum symbol value larger than the format permits.
    MaxSymbolValueTooLarge,
    /// An entropy table declared a maximum symbol value smaller than the format permits.
    MaxSymbolValueTooSmall,
    /// A scratch workspace was too small for the operation requested.
    WorkSpaceTooSmall,
    /// A frame's window size exceeded the limit configured on `DecoderOptions::max_window_size`.
    WindowSizeTooLarge {
        /// Window size declared by the frame header.
        window_size: u64,
        /// Limit currently configured on the decoder.
        max_window_size: u64,
    },
    /// A frame requires a dictionary but none was supplied; carries the requested id when present.
    DictionaryRequired(Option<u32>),
    /// The supplied dictionary id does not match the frame's declared id.
    DictionaryMismatch {
        /// Dictionary id declared by the frame.
        expected: u32,
        /// Dictionary id of the dictionary the caller provided.
        actual: u32,
    },
    /// Decoded output would exceed the limit configured on `DecoderOptions::max_output_size`.
    OutputSizeTooLarge {
        /// Total output size that would result from accepting the next chunk.
        output_size: u64,
        /// Limit currently configured on the decoder.
        max_output_size: usize,
    },
    /// A frame's declared content size cannot be represented as a `usize` on this platform.
    ContentSizeTooLarge(u64),
    /// A running output-size counter overflowed `u64` or `usize`.
    OutputSizeOverflow,
    /// Decoded output size disagreed with the frame header's declared content size.
    ContentSizeMismatch {
        /// Content size declared by the frame header.
        expected: u64,
        /// Content size actually produced by decoding.
        actual: u64,
    },
    /// The input was not exactly one Zstandard frame, and
    /// [`DecoderOptions::single_frame`](crate::DecoderOptions::single_frame)
    /// required that it be.
    ///
    /// Raised for a second frame, a skippable frame, or bytes after the first
    /// frame's end. A payload carried inside another protocol has a length its
    /// framing already agreed on, so extra bytes there are not a second
    /// message: they are evidence that the length was wrong.
    TrailingInput {
        /// Byte offset at which the unexpected input begins. Zero when the
        /// input opens with a skippable frame, which this mode rejects rather
        /// than passing over.
        offset: usize,
    },
    /// The frame trailer's content checksum did not match the recomputed XXH64 truncated to 32 bits.
    ChecksumMismatch {
        /// Checksum bytes read from the frame trailer.
        expected: u32,
        /// Checksum recomputed over the decoded output.
        actual: u32,
    },
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Generic => f.write_str("generic error"),
            Error::InvalidParameter(parameter) => write!(f, "invalid parameter: {parameter}"),
            Error::DstSizeTooSmall => f.write_str("destination buffer too small"),
            Error::SrcSizeWrong => f.write_str("source size is invalid"),
            Error::UnexpectedEof => f.write_str("unexpected end of input"),
            Error::BadMagic(magic) => write!(f, "invalid frame magic number 0x{magic:08x}"),
            Error::Corruption(message) => write!(f, "corruption detected: {message}"),
            Error::TableLogTooLarge => f.write_str("table log too large"),
            Error::MaxSymbolValueTooLarge => f.write_str("maximum symbol value too large"),
            Error::MaxSymbolValueTooSmall => f.write_str("maximum symbol value too small"),
            Error::WorkSpaceTooSmall => f.write_str("workspace too small"),
            Error::WindowSizeTooLarge {
                window_size,
                max_window_size,
            } => write!(
                f,
                "frame window size {window_size} exceeds configured limit {max_window_size}"
            ),
            Error::DictionaryRequired(Some(dictionary_id)) => {
                write!(f, "frame requires dictionary id {dictionary_id}")
            }
            Error::DictionaryRequired(None) => f.write_str("frame requires a dictionary"),
            Error::DictionaryMismatch { expected, actual } => write!(
                f,
                "dictionary mismatch: frame requires id {expected}, got {actual}"
            ),
            Error::OutputSizeTooLarge {
                output_size,
                max_output_size,
            } => write!(
                f,
                "decoded output size {output_size} exceeds configured limit {max_output_size}"
            ),
            Error::ContentSizeTooLarge(content_size) => {
                write!(f, "frame content size {content_size} exceeds this platform")
            }
            Error::OutputSizeOverflow => {
                f.write_str("decoded output size overflowed this platform")
            }
            Error::ContentSizeMismatch { expected, actual } => {
                write!(
                    f,
                    "frame content size mismatch: expected {expected}, got {actual}"
                )
            }
            Error::TrailingInput { offset } => write!(
                f,
                "input is not a single Zstandard frame: unexpected input at byte {offset}"
            ),
            Error::ChecksumMismatch { expected, actual } => write!(
                f,
                "content checksum mismatch: expected 0x{expected:08x}, got 0x{actual:08x}"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Convenience alias for `core::result::Result<T, Error>` used throughout the crate.
pub type Result<T> = core::result::Result<T, Error>;
