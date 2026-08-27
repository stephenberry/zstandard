use std::time::{Duration, Instant};

use crate::{
    block::{BlockHeader, BlockType},
    decode_out::DecodeOut,
    dictionary::{DecoderDictionary, Dictionary},
    error::{Error, Result},
    frame::{
        Format, FrameHeader, SkippableFrame, ZstandardFrameHeader, parse_frame_header_with_format,
    },
    literals::LiteralsState,
    sequence::{
        OutputLimit, RepeatOffsets, SequenceCommand, SequenceTablesState, TableTarget,
        decode_and_execute_sequences, decode_sequence_commands_into_stats,
        execute_sequences_with_total_match_bytes_profiled, parse_sequence_section,
    },
    xxhash::xxh64,
};

/// Decoder configuration.
///
/// The defaults are chosen for decoding input you do not control: the window
/// size is capped, matching upstream's `ZSTD_WINDOWLOG_LIMIT_DEFAULT`. Raise
/// or remove the individual limits when you trust the producer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecoderOptions {
    /// Reject frames whose declared window size exceeds this many bytes; `None` accepts any window.
    ///
    /// Defaults to [`DecoderOptions::DEFAULT_MAX_WINDOW_SIZE`]. A frame's
    /// window is the decoder's working-memory requirement, and it is declared
    /// by the input rather than the caller, so leaving this unbounded lets a
    /// small hostile frame demand an arbitrarily large allocation.
    pub max_window_size: Option<u64>,
    /// Reject input that would expand to more than this many output bytes; `None` is unlimited.
    pub max_output_size: Option<usize>,
    /// When a frame includes a content checksum, recompute it and reject mismatches. Defaults to `true`.
    pub verify_checksum: bool,
    /// Require the input to be exactly one Zstandard frame. Defaults to `false`.
    ///
    /// The default decodes and concatenates every frame in the input and
    /// passes over skippable frames, which is what the reference library does
    /// and the right behaviour for a `.zst` file. It is the wrong behaviour
    /// for a compressed payload carried inside another protocol, where the
    /// enclosing framing already agreed how long the payload is: there, a
    /// second frame or a trailing byte is not more data to decode, it is
    /// evidence of a framing bug. Decoding the first frame and reporting
    /// success hands the caller a truncated message that looks complete.
    ///
    /// With this set, a second frame, a trailing byte, or a skippable frame in
    /// any position is [`Error::TrailingInput`](crate::Error::TrailingInput).
    /// Skippable frames are rejected rather than passed over, including one
    /// that precedes the Zstandard frame, which reports offset 0. Everything up
    /// to the offending input still decodes normally, so the limits above apply
    /// unchanged.
    pub single_frame: bool,
    /// Frame envelope the input is expected to use; upstream's `ZSTD_d_format`.
    /// Defaults to [`Format::Zstd1`].
    ///
    /// A magicless frame carries nothing that identifies it as one, so this has
    /// to be asserted rather than detected. Setting it also gives up the
    /// ability to recognise a skippable frame, whose magic number is all it
    /// has, so a [`Format::Zstd1Magicless`] stream is one Zstandard frame after
    /// another and nothing else.
    pub format: Format,
}

impl DecoderOptions {
    /// Window-size ceiling applied by [`DecoderOptions::default`]: 128 MiB.
    ///
    /// This is upstream's `ZSTD_WINDOWLOG_LIMIT_DEFAULT` (`1 << 27`), the
    /// largest window its decoder accepts without an explicit opt-in, and the
    /// largest this crate's encoder will ever declare.
    pub const DEFAULT_MAX_WINDOW_SIZE: u64 = 1 << 27;
}

impl Default for DecoderOptions {
    fn default() -> Self {
        Self {
            max_window_size: Some(Self::DEFAULT_MAX_WINDOW_SIZE),
            max_output_size: None,
            verify_checksum: true,
            // Matching the reference library on multi-frame input is the right
            // default; strictness is the opt-in.
            single_frame: false,
            format: Format::Zstd1,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[doc(hidden)]
pub struct DecodeStageProfile {
    pub total: Duration,
    pub literals: Duration,
    pub sequence_tables: Duration,
    pub sequence_commands: Duration,
    pub sequence_execute: Duration,
    pub sequence_execute_literal_copy: Duration,
    pub sequence_execute_prefix_match_copy: Duration,
    pub sequence_execute_dictionary_match_copy: Duration,
    pub blocks: usize,
    pub compressed_blocks: usize,
    pub raw_blocks: usize,
    pub rle_blocks: usize,
    /// Bytes produced. Zero for the first-block profile, which discards output.
    pub output_bytes: usize,
}

#[derive(Default)]
struct DecodeStageProfiler {
    profile: DecodeStageProfile,
}

impl DecodeStageProfiler {
    fn finish(self) -> DecodeStageProfile {
        self.profile
    }
}

/// Decode every Zstandard frame contained in `src` and concatenate the results
/// into a fresh `Vec<u8>`. Skippable frames are silently passed over.
pub fn decode_all(src: &[u8]) -> Result<Vec<u8>> {
    decode_all_with_options(src, DecoderOptions::default())
}

/// Like [`decode_all`] but uses caller-supplied [`DecoderOptions`].
pub fn decode_all_with_options(src: &[u8], options: DecoderOptions) -> Result<Vec<u8>> {
    decode_all_with_scratch(src, options, None, &mut DecodeScratch::default())
}

/// Decode `src` using a pre-parsed dictionary. The dictionary id, if any, must
/// match the one declared by every Zstandard frame in `src`.
pub fn decode_all_with_prepared_dict(src: &[u8], dict: &DecoderDictionary<'_>) -> Result<Vec<u8>> {
    decode_all_with_prepared_dict_and_options(src, dict, DecoderOptions::default())
}

/// Like [`decode_all_with_prepared_dict`] but uses caller-supplied [`DecoderOptions`].
pub fn decode_all_with_prepared_dict_and_options(
    src: &[u8],
    dict: &DecoderDictionary<'_>,
    options: DecoderOptions,
) -> Result<Vec<u8>> {
    decode_all_with_scratch(
        src,
        options,
        Some(dict.as_inner()),
        &mut DecodeScratch::default(),
    )
}

/// Decode `src` using `dict` as a dictionary. The slice is parsed on every
/// call; prefer [`decode_all_with_prepared_dict`] when reusing the same
/// dictionary across many calls.
pub fn decode_all_with_dict(src: &[u8], dict: &[u8]) -> Result<Vec<u8>> {
    decode_all_with_dict_and_options(src, dict, DecoderOptions::default())
}

/// Like [`decode_all_with_dict`] but uses caller-supplied [`DecoderOptions`].
pub fn decode_all_with_dict_and_options(
    src: &[u8],
    dict: &[u8],
    options: DecoderOptions,
) -> Result<Vec<u8>> {
    let dictionary = DecoderDictionary::new(dict)?;
    decode_all_with_prepared_dict_and_options(src, &dictionary, options)
}

/// Decode `src` into `dst`, returning how many bytes were written.
///
/// The decoding counterpart to [`encode_into_slice`](crate::encode_into_slice),
/// and the shape upstream's `ZSTD_decompress` has: the destination is the
/// caller's, so it can be an arena slot, an FFI buffer, an mmap, or a stack
/// array rather than a `Vec` this crate hands back. Nothing is allocated for
/// the output.
///
/// `dst` must be large enough for everything `src` decodes to, and *exactly*
/// that large is enough -- there is no padding requirement. Two ways to size
/// it, both reading only the frame headers:
///
/// - [`decompressed_size`](crate::decompressed_size) is the exact answer when
///   every frame declares its content size, and `None` when one does not.
/// - [`decompress_bound`](crate::decompress_bound) is an upper bound that
///   always answers, at the cost of over-allocating for frames that declared
///   nothing.
///
/// Every frame in `src` is decoded and the results concatenated, and skippable
/// frames are passed over, matching [`decode_all`]. If `dst` runs out,
/// [`Error::DstSizeTooSmall`](crate::Error::DstSizeTooSmall) is reported and
/// the contents of `dst` are unspecified; nothing partial is claimed to be
/// output, because a truncated decode is not a shorter message but a corrupt
/// one.
///
/// ```
/// use zstandard::{decode_into_slice, decompressed_size, encode_all};
///
/// let compressed = encode_all(b"into a buffer I already own")?;
/// let size = decompressed_size(&compressed)?.expect("this encoder declares it");
///
/// let mut dst = vec![0u8; size as usize];
/// let written = decode_into_slice(&compressed, &mut dst)?;
/// assert_eq!(&dst[..written], b"into a buffer I already own");
/// # Ok::<(), zstandard::Error>(())
/// ```
pub fn decode_into_slice(src: &[u8], dst: &mut [u8]) -> Result<usize> {
    decode_into_slice_with_options(src, dst, DecoderOptions::default())
}

/// Like [`decode_into_slice`] but uses caller-supplied [`DecoderOptions`].
pub fn decode_into_slice_with_options(
    src: &[u8],
    dst: &mut [u8],
    options: DecoderOptions,
) -> Result<usize> {
    decode_into_slice_with_scratch(src, options, None, &mut DecodeScratch::default(), dst)
}

#[cfg(any(feature = "internal-trace", test))]
// Compiled into test builds so unit tests can reach the trace API without
// turning the feature on. Not every entry point has a default-feature test,
// so under `cfg(test)` alone some are legitimately unused.
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
#[doc(hidden)]
pub fn profile_first_block_decode_with_options(
    src: &[u8],
    options: DecoderOptions,
) -> Result<DecodeStageProfile> {
    profile_first_block_decode_with_scratch(src, options, None, &mut DecodeScratch::default())
}

#[cfg(any(feature = "internal-trace", test))]
// Compiled into test builds so unit tests can reach the trace API without
// turning the feature on. Not every entry point has a default-feature test,
// so under `cfg(test)` alone some are legitimately unused.
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
#[doc(hidden)]
pub fn profile_frame_decode_with_options(
    src: &[u8],
    options: DecoderOptions,
) -> Result<DecodeStageProfile> {
    profile_frame_decode_with_scratch(src, options, None, &mut DecodeScratch::default())
}

#[cfg(any(feature = "internal-trace", test))]
// Compiled into test builds so unit tests can reach the trace API without
// turning the feature on. Not every entry point has a default-feature test,
// so under `cfg(test)` alone some are legitimately unused.
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
#[doc(hidden)]
pub fn profile_frame_decode_with_prepared_dict_and_options(
    src: &[u8],
    dict: &DecoderDictionary<'_>,
    options: DecoderOptions,
) -> Result<DecodeStageProfile> {
    profile_frame_decode_with_scratch(
        src,
        options,
        Some(dict.as_inner()),
        &mut DecodeScratch::default(),
    )
}

#[cfg(any(feature = "internal-trace", test))]
// Compiled into test builds so unit tests can reach the trace API without
// turning the feature on. Not every entry point has a default-feature test,
// so under `cfg(test)` alone some are legitimately unused.
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
#[doc(hidden)]
pub fn profile_first_block_decode_with_prepared_dict_and_options(
    src: &[u8],
    dict: &DecoderDictionary<'_>,
    options: DecoderOptions,
) -> Result<DecodeStageProfile> {
    profile_first_block_decode_with_scratch(
        src,
        options,
        Some(dict.as_inner()),
        &mut DecodeScratch::default(),
    )
}

#[derive(Default)]
struct DecodeScratch {
    literals: Vec<u8>,
    sequences: Vec<SequenceCommand>,
}

/// Reusable one-shot decoder that amortizes buffer allocation across calls.
///
/// The buffers reused across calls are the internal ones: decoded literals and
/// the sequence command list. The `decode_all*` methods still return a fresh
/// `Vec` each time, and for most frames that allocation is far larger than the
/// scratch it saves. Two families keep the output buffer as well: the
/// [`decode_all_into`](Decoder::decode_all_into) family reuses a caller's
/// `Vec`, and the [`decode_into_slice`](Decoder::decode_into_slice) family
/// writes into a caller's `&mut [u8]`, which is upstream's `ZSTD_decompress`
/// and allocates nothing once the decoder is warm.
pub struct Decoder {
    scratch: DecodeScratch,
}

impl Decoder {
    /// Construct a `Decoder` with empty scratch buffers.
    pub fn new() -> Self {
        Self {
            scratch: DecodeScratch::default(),
        }
    }

    /// See [`decode_all`] for the equivalent free-function form.
    pub fn decode_all(&mut self, src: &[u8]) -> Result<Vec<u8>> {
        self.decode_all_with_options(src, DecoderOptions::default())
    }

    /// See [`decode_all_with_options`] for the equivalent free-function form.
    pub fn decode_all_with_options(
        &mut self,
        src: &[u8],
        options: DecoderOptions,
    ) -> Result<Vec<u8>> {
        decode_all_with_scratch(src, options, None, &mut self.scratch)
    }

    /// See [`decode_all_with_prepared_dict`] for the equivalent free-function form.
    pub fn decode_all_with_prepared_dict(
        &mut self,
        src: &[u8],
        dict: &DecoderDictionary<'_>,
    ) -> Result<Vec<u8>> {
        self.decode_all_with_prepared_dict_and_options(src, dict, DecoderOptions::default())
    }

    /// See [`decode_all_with_prepared_dict_and_options`] for the equivalent free-function form.
    pub fn decode_all_with_prepared_dict_and_options(
        &mut self,
        src: &[u8],
        dict: &DecoderDictionary<'_>,
        options: DecoderOptions,
    ) -> Result<Vec<u8>> {
        decode_all_with_scratch(src, options, Some(dict.as_inner()), &mut self.scratch)
    }

    /// See [`decode_all_with_dict`] for the equivalent free-function form.
    pub fn decode_all_with_dict(&mut self, src: &[u8], dict: &[u8]) -> Result<Vec<u8>> {
        self.decode_all_with_dict_and_options(src, dict, DecoderOptions::default())
    }

    /// See [`decode_all_with_dict_and_options`] for the equivalent free-function form.
    pub fn decode_all_with_dict_and_options(
        &mut self,
        src: &[u8],
        dict: &[u8],
        options: DecoderOptions,
    ) -> Result<Vec<u8>> {
        let dictionary = DecoderDictionary::new(dict)?;
        self.decode_all_with_prepared_dict_and_options(src, &dictionary, options)
    }

    /// Decode into `dst`, replacing its contents and keeping its allocation.
    ///
    /// Equivalent to [`decode_all`](Decoder::decode_all) except that the caller
    /// owns the destination. Decoding a stream of frames through one buffer
    /// costs one allocation rather than one per frame, which for a 4 MiB frame
    /// is a larger saving than every internal buffer this type already reuses.
    /// This is the decoding counterpart to [`Encoder::encode_into`].
    ///
    /// `dst` is cleared first, so anything it held is discarded, including when
    /// decoding fails partway through. On error its contents are unspecified
    /// but its capacity is retained.
    ///
    /// [`Encoder::encode_into`]: crate::Encoder::encode_into
    pub fn decode_all_into(&mut self, src: &[u8], dst: &mut Vec<u8>) -> Result<()> {
        self.decode_all_into_with_options(src, dst, DecoderOptions::default())
    }

    /// Like [`decode_all_into`](Decoder::decode_all_into) but uses caller-supplied [`DecoderOptions`].
    pub fn decode_all_into_with_options(
        &mut self,
        src: &[u8],
        dst: &mut Vec<u8>,
        options: DecoderOptions,
    ) -> Result<()> {
        decode_into_with_scratch(
            src,
            options,
            None,
            &mut self.scratch,
            &mut DecodeOut::growable(dst),
            None,
        )
    }

    /// Like [`decode_all_into`](Decoder::decode_all_into) but decodes with a pre-parsed dictionary.
    pub fn decode_all_into_with_prepared_dict(
        &mut self,
        src: &[u8],
        dst: &mut Vec<u8>,
        dict: &DecoderDictionary<'_>,
    ) -> Result<()> {
        self.decode_all_into_with_prepared_dict_and_options(
            src,
            dst,
            dict,
            DecoderOptions::default(),
        )
    }

    /// Like [`decode_all_into_with_prepared_dict`](Decoder::decode_all_into_with_prepared_dict)
    /// but uses caller-supplied [`DecoderOptions`].
    pub fn decode_all_into_with_prepared_dict_and_options(
        &mut self,
        src: &[u8],
        dst: &mut Vec<u8>,
        dict: &DecoderDictionary<'_>,
        options: DecoderOptions,
    ) -> Result<()> {
        decode_into_with_scratch(
            src,
            options,
            Some(dict.as_inner()),
            &mut self.scratch,
            &mut DecodeOut::growable(dst),
            None,
        )
    }

    /// See [`decode_into_slice`] for the equivalent free-function form.
    ///
    /// Reusing a `Decoder` here keeps the literals and sequence scratch across
    /// calls, which for a fixed destination is every allocation there is: the
    /// output already belongs to the caller, so a warm `Decoder` decoding into
    /// a slice allocates nothing at all after the first frame.
    pub fn decode_into_slice(&mut self, src: &[u8], dst: &mut [u8]) -> Result<usize> {
        self.decode_into_slice_with_options(src, dst, DecoderOptions::default())
    }

    /// See [`decode_into_slice_with_options`] for the equivalent free-function form.
    pub fn decode_into_slice_with_options(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
        options: DecoderOptions,
    ) -> Result<usize> {
        decode_into_slice_with_scratch(src, options, None, &mut self.scratch, dst)
    }

    /// Like [`decode_into_slice`](Decoder::decode_into_slice) but decodes with a
    /// pre-parsed dictionary.
    pub fn decode_into_slice_with_prepared_dict(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
        dict: &DecoderDictionary<'_>,
    ) -> Result<usize> {
        self.decode_into_slice_with_prepared_dict_and_options(
            src,
            dst,
            dict,
            DecoderOptions::default(),
        )
    }

    /// Like [`decode_into_slice_with_prepared_dict`](Decoder::decode_into_slice_with_prepared_dict)
    /// but uses caller-supplied [`DecoderOptions`].
    pub fn decode_into_slice_with_prepared_dict_and_options(
        &mut self,
        src: &[u8],
        dst: &mut [u8],
        dict: &DecoderDictionary<'_>,
        options: DecoderOptions,
    ) -> Result<usize> {
        decode_into_slice_with_scratch(src, options, Some(dict.as_inner()), &mut self.scratch, dst)
    }

    #[doc(hidden)]
    pub fn profile_first_block_decode_with_options(
        &mut self,
        src: &[u8],
        options: DecoderOptions,
    ) -> Result<DecodeStageProfile> {
        profile_first_block_decode_with_scratch(src, options, None, &mut self.scratch)
    }

    #[doc(hidden)]
    pub fn profile_first_block_decode_with_prepared_dict_and_options(
        &mut self,
        src: &[u8],
        dict: &DecoderDictionary<'_>,
        options: DecoderOptions,
    ) -> Result<DecodeStageProfile> {
        profile_first_block_decode_with_scratch(
            src,
            options,
            Some(dict.as_inner()),
            &mut self.scratch,
        )
    }

    #[doc(hidden)]
    pub fn profile_frame_decode_with_options(
        &mut self,
        src: &[u8],
        options: DecoderOptions,
    ) -> Result<DecodeStageProfile> {
        profile_frame_decode_with_scratch(src, options, None, &mut self.scratch)
    }

    #[doc(hidden)]
    pub fn profile_frame_decode_with_prepared_dict_and_options(
        &mut self,
        src: &[u8],
        dict: &DecoderDictionary<'_>,
        options: DecoderOptions,
    ) -> Result<DecodeStageProfile> {
        profile_frame_decode_with_scratch(src, options, Some(dict.as_inner()), &mut self.scratch)
    }
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn profile_first_block_decode_with_scratch(
    src: &[u8],
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    scratch: &mut DecodeScratch,
) -> Result<DecodeStageProfile> {
    let total_start = Instant::now();
    if src.is_empty() {
        return Err(Error::UnexpectedEof);
    }

    let mut cursor = Cursor::new(src);
    while !cursor.is_empty() {
        match parse_frame_header_with_format(cursor.remaining_slice(), options.format)? {
            FrameHeader::Skippable(skippable) => skip_skippable_frame(&mut cursor, skippable)?,
            FrameHeader::Zstandard(header) => {
                let mut profiler = DecodeStageProfiler::default();
                profile_first_zstandard_block(
                    &mut cursor,
                    header,
                    options,
                    dictionary,
                    &mut scratch.literals,
                    &mut scratch.sequences,
                    &mut profiler,
                )?;
                let mut profile = profiler.finish();
                profile.total = total_start.elapsed();
                return Ok(profile);
            }
        }
    }

    Err(Error::UnexpectedEof)
}

/// Decode every block of every frame in `src`, attributing the time to stages.
///
/// The first-block variant above answers a different question. It exists to
/// compare one block against upstream's first block, so it decodes with no
/// history behind it and never reaches the paths that only a later block can
/// take: a match pointing back into an earlier block, a repeated entropy table,
/// a literals section that reuses the previous Huffman tree. Every row in the
/// benchmark report's decode column is a whole frame, so a stage attribution
/// that stops after one block cannot explain one.
fn profile_frame_decode_with_scratch(
    src: &[u8],
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    scratch: &mut DecodeScratch,
) -> Result<DecodeStageProfile> {
    let mut profiler = DecodeStageProfiler::default();
    let mut out = Vec::new();
    let total_start = Instant::now();
    decode_into_with_scratch(
        src,
        options,
        dictionary,
        scratch,
        &mut DecodeOut::growable(&mut out),
        Some(&mut profiler),
    )?;
    let mut profile = profiler.finish();
    profile.total = total_start.elapsed();
    profile.output_bytes = out.len();
    Ok(profile)
}

/// Decode into a caller-owned slice, reporting how much of it was filled.
///
/// The fixed destination is the whole difference from
/// [`decode_into_with_scratch`]: it cannot grow, so a write that does not fit
/// is [`Error::DstSizeTooSmall`] rather than an allocation.
fn decode_into_slice_with_scratch(
    src: &[u8],
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    scratch: &mut DecodeScratch,
    dst: &mut [u8],
) -> Result<usize> {
    let mut out = DecodeOut::fixed(dst);
    decode_into_with_scratch(src, options, dictionary, scratch, &mut out, None)?;
    Ok(out.len())
}

fn decode_all_with_scratch(
    src: &[u8],
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    scratch: &mut DecodeScratch,
) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    decode_into_with_scratch(
        src,
        options,
        dictionary,
        scratch,
        &mut DecodeOut::growable(&mut out),
        None,
    )?;
    Ok(out)
}

/// Decode into a caller-owned buffer, replacing whatever it held.
///
/// `out` is truncated rather than freed, so a caller that decodes repeatedly
/// keeps one allocation instead of returning a fresh one to the allocator on
/// every frame.
fn decode_into_with_scratch(
    src: &[u8],
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    scratch: &mut DecodeScratch,
    out: &mut DecodeOut<'_>,
    mut profiler: Option<&mut DecodeStageProfiler>,
) -> Result<()> {
    out.clear();
    if src.is_empty() {
        return Err(Error::UnexpectedEof);
    }

    let mut cursor = Cursor::new(src);

    while !cursor.is_empty() {
        match parse_frame_header_with_format(cursor.remaining_slice(), options.format)? {
            FrameHeader::Skippable(skippable) => {
                if options.single_frame {
                    return Err(Error::TrailingInput {
                        offset: cursor.position(),
                    });
                }
                skip_skippable_frame(&mut cursor, skippable)?
            }
            FrameHeader::Zstandard(header) => {
                decode_zstandard_frame(
                    &mut cursor,
                    header,
                    options,
                    dictionary,
                    &mut scratch.literals,
                    &mut scratch.sequences,
                    out,
                    profiler.as_deref_mut(),
                )?;
                // Checked after decoding rather than before, so a caller that
                // gets this error still knows the frame itself was sound.
                if options.single_frame && !cursor.is_empty() {
                    return Err(Error::TrailingInput {
                        offset: cursor.position(),
                    });
                }
            }
        }
    }

    Ok(())
}

fn skip_skippable_frame(cursor: &mut Cursor<'_>, frame: SkippableFrame) -> Result<()> {
    let total_size = frame
        .header_size
        .checked_add(frame.size as usize)
        .ok_or(Error::OutputSizeOverflow)?;
    cursor.read_exact(total_size)?;
    Ok(())
}

fn decode_zstandard_frame(
    cursor: &mut Cursor<'_>,
    header: ZstandardFrameHeader,
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    literals_scratch: &mut Vec<u8>,
    sequences_scratch: &mut Vec<SequenceCommand>,
    out: &mut DecodeOut<'_>,
    mut profiler: Option<&mut DecodeStageProfiler>,
) -> Result<()> {
    cursor.read_exact(header.header_size)?;

    let dictionary = match (dictionary, header.dictionary_id) {
        (None, Some(dictionary_id)) => {
            return Err(Error::DictionaryRequired(Some(dictionary_id)));
        }
        (Some(dictionary), Some(dictionary_id)) if dictionary.id() != dictionary_id => {
            return Err(Error::DictionaryMismatch {
                expected: dictionary_id,
                actual: dictionary.id(),
            });
        }
        _ => dictionary,
    };
    if let Some(max_window_size) = options.max_window_size {
        if header.window_size > max_window_size {
            return Err(Error::WindowSizeTooLarge {
                window_size: header.window_size,
                max_window_size,
            });
        }
    }

    let frame_start = out.len();
    let mut literals_state =
        dictionary.map_or_else(LiteralsState::default, Dictionary::literals_state);
    let mut sequence_tables =
        dictionary.map_or_else(SequenceTablesState::default, Dictionary::sequence_tables);
    let mut repeat_offsets =
        dictionary.map_or_else(RepeatOffsets::default, Dictionary::repeat_offsets);
    if let Some(content_size) = header.content_size {
        ensure_total_size_limit(out.len(), content_size, options.max_output_size)?;
        let reserve =
            usize::try_from(content_size).map_err(|_| Error::ContentSizeTooLarge(content_size))?;
        // `content_size` is attacker-controlled and unverified until the frame
        // has actually been decoded, so it cannot be reserved directly: a
        // 17-byte frame declaring 2^46 bytes would abort the process inside
        // the allocator, which no caller can catch.
        //
        // Bound it by what the remaining input could possibly expand to. The
        // densest encoding is an RLE block: 3 header bytes plus one literal
        // yields at most `block_size_max` output, so `n` remaining input bytes
        // decode to at most `(n / 4) * block_size_max`. Honest frames stay on
        // the single-reserve fast path because their declared size is far
        // below that bound; hostile ones fall back to growing per block via
        // `reserve_block_output`.
        let expansion_bound = (cursor.remaining_slice().len() / (BlockHeader::SIZE + 1))
            .saturating_mul(header.block_size_max as usize);
        out.try_reserve(reserve.min(expansion_bound).saturating_add(32))?;
    }

    loop {
        let header_bytes = cursor.read_exact(BlockHeader::SIZE)?;
        let block_header = BlockHeader::parse(header_bytes)?;

        if let Some(profiler) = profiler.as_deref_mut() {
            profiler.profile.blocks += 1;
        }

        match block_header.block_type {
            BlockType::Raw => {
                if let Some(profiler) = profiler.as_deref_mut() {
                    profiler.profile.raw_blocks += 1;
                }
                if block_header.block_size > header.block_size_max {
                    return Err(Error::Corruption(
                        "block size exceeds frame block size limit",
                    ));
                }
                let data = cursor.read_exact(block_header.payload_size())?;
                append_bytes(
                    out,
                    data,
                    OutputLimit::whole_output(options.max_output_size),
                )?;
            }
            BlockType::Rle => {
                if let Some(profiler) = profiler.as_deref_mut() {
                    profiler.profile.rle_blocks += 1;
                }
                if block_header.block_size > header.block_size_max {
                    return Err(Error::Corruption(
                        "block size exceeds frame block size limit",
                    ));
                }
                let byte = cursor.read_u8()?;
                append_repeated(
                    out,
                    byte,
                    block_header.block_size as usize,
                    OutputLimit::whole_output(options.max_output_size),
                )?;
            }
            BlockType::Compressed => {
                if let Some(profiler) = profiler.as_deref_mut() {
                    profiler.profile.compressed_blocks += 1;
                }
                let data = cursor.read_exact(block_header.payload_size())?;
                decode_compressed_block_inner(
                    data,
                    header.block_size_max as usize,
                    frame_start,
                    usize::try_from(header.window_size).unwrap_or(usize::MAX),
                    dictionary.map(Dictionary::content),
                    &mut literals_state,
                    &mut sequence_tables,
                    &mut repeat_offsets,
                    literals_scratch,
                    sequences_scratch,
                    out,
                    options.max_output_size,
                    profiler.as_deref_mut(),
                )?;
            }
        }

        if block_header.last_block {
            break;
        }
    }

    if header.checksum {
        let expected = cursor.read_u32_le()?;
        if options.verify_checksum {
            let actual = xxh64(&out.as_slice()[frame_start..], 0) as u32;
            if actual != expected {
                return Err(Error::ChecksumMismatch { expected, actual });
            }
        }
    }

    if let Some(expected_size) = header.content_size {
        let actual_size = (out.len() - frame_start) as u64;
        if actual_size != expected_size {
            return Err(Error::ContentSizeMismatch {
                expected: expected_size,
                actual: actual_size,
            });
        }
    }

    Ok(())
}

fn profile_first_zstandard_block(
    cursor: &mut Cursor<'_>,
    header: ZstandardFrameHeader,
    options: DecoderOptions,
    dictionary: Option<&Dictionary<'_>>,
    literals_scratch: &mut Vec<u8>,
    sequences_scratch: &mut Vec<SequenceCommand>,
    profiler: &mut DecodeStageProfiler,
) -> Result<()> {
    cursor.read_exact(header.header_size)?;

    let dictionary = match (dictionary, header.dictionary_id) {
        (None, Some(dictionary_id)) => {
            return Err(Error::DictionaryRequired(Some(dictionary_id)));
        }
        (Some(dictionary), Some(dictionary_id)) if dictionary.id() != dictionary_id => {
            return Err(Error::DictionaryMismatch {
                expected: dictionary_id,
                actual: dictionary.id(),
            });
        }
        _ => dictionary,
    };
    if let Some(max_window_size) = options.max_window_size {
        if header.window_size > max_window_size {
            return Err(Error::WindowSizeTooLarge {
                window_size: header.window_size,
                max_window_size,
            });
        }
    }

    let mut out = Vec::new();
    let mut out = DecodeOut::growable(&mut out);
    let mut literals_state =
        dictionary.map_or_else(LiteralsState::default, Dictionary::literals_state);
    let mut sequence_tables =
        dictionary.map_or_else(SequenceTablesState::default, Dictionary::sequence_tables);
    let mut repeat_offsets =
        dictionary.map_or_else(RepeatOffsets::default, Dictionary::repeat_offsets);
    if let Some(content_size) = header.content_size {
        ensure_total_size_limit(out.len(), content_size, options.max_output_size)?;
        let reserve =
            usize::try_from(content_size).map_err(|_| Error::ContentSizeTooLarge(content_size))?;
        out.try_reserve(
            reserve
                .min(header.block_size_max as usize)
                .saturating_add(32),
        )?;
    }

    let header_bytes = cursor.read_exact(BlockHeader::SIZE)?;
    let block_header = BlockHeader::parse(header_bytes)?;
    profiler.profile.blocks = 1;

    match block_header.block_type {
        BlockType::Raw => {
            profiler.profile.raw_blocks = 1;
            if block_header.block_size > header.block_size_max {
                return Err(Error::Corruption(
                    "block size exceeds frame block size limit",
                ));
            }
            let data = cursor.read_exact(block_header.payload_size())?;
            append_bytes(
                &mut out,
                data,
                OutputLimit::whole_output(options.max_output_size),
            )?;
        }
        BlockType::Rle => {
            profiler.profile.rle_blocks = 1;
            if block_header.block_size > header.block_size_max {
                return Err(Error::Corruption(
                    "block size exceeds frame block size limit",
                ));
            }
            let byte = cursor.read_u8()?;
            append_repeated(
                &mut out,
                byte,
                block_header.block_size as usize,
                OutputLimit::whole_output(options.max_output_size),
            )?;
        }
        BlockType::Compressed => {
            profiler.profile.compressed_blocks = 1;
            let data = cursor.read_exact(block_header.payload_size())?;
            decode_compressed_block_profiled(
                data,
                header.block_size_max as usize,
                0,
                usize::try_from(header.window_size).unwrap_or(usize::MAX),
                dictionary.map(Dictionary::content),
                &mut literals_state,
                &mut sequence_tables,
                &mut repeat_offsets,
                literals_scratch,
                sequences_scratch,
                &mut out,
                options.max_output_size,
                profiler,
            )?;
        }
    }

    Ok(())
}

fn decode_compressed_block_profiled(
    src: &[u8],
    block_size_max: usize,
    frame_start: usize,
    window_size: usize,
    dictionary: Option<&[u8]>,
    literals_state: &mut LiteralsState,
    sequence_tables: &mut SequenceTablesState,
    repeat_offsets: &mut RepeatOffsets,
    literals_scratch: &mut Vec<u8>,
    sequences_scratch: &mut Vec<SequenceCommand>,
    out: &mut DecodeOut<'_>,
    max_output_size: Option<usize>,
    profiler: &mut DecodeStageProfiler,
) -> Result<()> {
    decode_compressed_block_inner(
        src,
        block_size_max,
        frame_start,
        window_size,
        dictionary,
        literals_state,
        sequence_tables,
        repeat_offsets,
        literals_scratch,
        sequences_scratch,
        out,
        max_output_size,
        Some(profiler),
    )
}

/// Decode one compressed block, appending its output to `out`.
///
/// Shared by the one-shot and the streaming decoder, which differ only in what
/// `out` holds. Both require the frame's history from `frame_start` onward to
/// be present in `out`, because that is where a match reads from; the streaming
/// decoder keeps that much and releases the rest, and `limit` is what tells the
/// output-size cap about the bytes it released.
pub(crate) fn decode_compressed_block_into(
    src: &[u8],
    block_size_max: usize,
    frame_start: usize,
    window_size: usize,
    dictionary: Option<&[u8]>,
    literals_state: &mut LiteralsState,
    sequence_tables: &mut SequenceTablesState,
    repeat_offsets: &mut RepeatOffsets,
    literals_scratch: &mut Vec<u8>,
    out: &mut DecodeOut<'_>,
    limit: OutputLimit,
) -> Result<()> {
    let (literals_size, literals) = crate::literals::decode_literals_section_into(
        src,
        literals_state,
        block_size_max,
        literals_scratch,
    )?;
    // Over-read headroom is ensured by decode_literals_section_into:
    // - Raw literals with headroom: slice into src (sequence data follows)
    // - Raw literals without headroom / RLE / Huffman: scratch with reserve(16)
    if literals.len() > block_size_max {
        return Err(Error::Corruption(
            "compressed block literals exceed the frame block size limit",
        ));
    }

    let sequence_section = src.get(literals_size..).ok_or(Error::UnexpectedEof)?;
    // decode_and_execute_sequences reads only the SequenceDTables, so the
    // plain DTables would be a second table build nothing consumes.
    let parsed_sequences =
        parse_sequence_section(sequence_section, sequence_tables, TableTarget::SequenceOnly)?;
    if parsed_sequences.number_of_sequences == 0
        && parsed_sequences.header_size != sequence_section.len()
    {
        return Err(Error::Corruption(
            "zero-sequence blocks must not contain additional sequence payload",
        ));
    }

    decode_and_execute_sequences(
        &parsed_sequences,
        sequence_tables,
        out,
        frame_start,
        window_size,
        block_size_max,
        dictionary,
        literals,
        repeat_offsets,
        limit,
    )
}

fn decode_compressed_block_inner(
    src: &[u8],
    block_size_max: usize,
    frame_start: usize,
    window_size: usize,
    dictionary: Option<&[u8]>,
    literals_state: &mut LiteralsState,
    sequence_tables: &mut SequenceTablesState,
    repeat_offsets: &mut RepeatOffsets,
    literals_scratch: &mut Vec<u8>,
    sequences_scratch: &mut Vec<SequenceCommand>,
    out: &mut DecodeOut<'_>,
    max_output_size: Option<usize>,
    profiler: Option<&mut DecodeStageProfiler>,
) -> Result<()> {
    let Some(profiler) = profiler else {
        return decode_compressed_block_into(
            src,
            block_size_max,
            frame_start,
            window_size,
            dictionary,
            literals_state,
            sequence_tables,
            repeat_offsets,
            literals_scratch,
            out,
            // The one-shot decoder keeps every byte it produces, so `out.len()`
            // is the whole output and the cap needs no offset.
            OutputLimit::whole_output(max_output_size),
        );
    };

    let literals_start = Instant::now();
    let (literals_size, literals) = crate::literals::decode_literals_section_into(
        src,
        literals_state,
        block_size_max,
        literals_scratch,
    )?;
    if literals.len() > block_size_max {
        return Err(Error::Corruption(
            "compressed block literals exceed the frame block size limit",
        ));
    }
    profiler.profile.literals += literals_start.elapsed();

    {
        let sequence_start = Instant::now();

        let sequence_section = src.get(literals_size..).ok_or(Error::UnexpectedEof)?;
        // The profiled path decodes commands into a buffer first, and that
        // decoder reads the plain DTables.
        let parsed_sequences =
            parse_sequence_section(sequence_section, sequence_tables, TableTarget::Both)?;
        profiler.profile.sequence_tables += sequence_start.elapsed();
        if parsed_sequences.number_of_sequences == 0
            && parsed_sequences.header_size != sequence_section.len()
        {
            return Err(Error::Corruption(
                "zero-sequence blocks must not contain additional sequence payload",
            ));
        }

        let command_start = Instant::now();
        let decoded_stats = decode_sequence_commands_into_stats(
            &parsed_sequences,
            sequence_tables,
            sequences_scratch,
        )?;
        profiler.profile.sequence_commands += command_start.elapsed();
        let execute_start = Instant::now();
        // The profiled path consumes literals by value and does no fixed-width
        // over-read, so it takes the literals alone rather than the padded slice.
        let result = execute_sequences_with_total_match_bytes_profiled(
            out,
            frame_start,
            window_size,
            block_size_max,
            dictionary,
            literals.as_slice(),
            sequences_scratch,
            repeat_offsets,
            max_output_size,
            decoded_stats.total_match_bytes,
        );
        profiler.profile.sequence_execute += execute_start.elapsed();
        if let Ok(execute_profile) = &result {
            profiler.profile.sequence_execute_literal_copy += execute_profile.literal_copy;
            profiler.profile.sequence_execute_prefix_match_copy +=
                execute_profile.prefix_match_copy;
            profiler.profile.sequence_execute_dictionary_match_copy +=
                execute_profile.dictionary_match_copy;
        }
        result.map(|_| ())
    }
}

/// Append a literal run, rejecting it if it would break the output-size cap.
///
/// Shared with the streaming decoder, which is why the cap arrives as an
/// [`OutputLimit`] rather than a bare size: there `out.len()` is only the part
/// of the output still held.
pub(crate) fn append_bytes(
    out: &mut DecodeOut<'_>,
    bytes: &[u8],
    limit: OutputLimit,
) -> Result<()> {
    // Checked ahead of the cap so an uncapped decoder still reports the
    // overflow rather than letting `Vec` abort on the allocation.
    let new_len = checked_output_len(out.len(), bytes.len())?;
    limit.check_total(new_len)?;
    out.append(bytes)
}

pub(crate) fn append_repeated(
    out: &mut DecodeOut<'_>,
    byte: u8,
    count: usize,
    limit: OutputLimit,
) -> Result<()> {
    let new_len = checked_output_len(out.len(), count)?;
    limit.check_total(new_len)?;
    out.append_repeated(byte, count)
}

fn ensure_total_size_limit(
    current_output_size: usize,
    content_size: u64,
    max_output_size: Option<usize>,
) -> Result<()> {
    if let Some(limit) = max_output_size {
        let total = (current_output_size as u64)
            .checked_add(content_size)
            .ok_or(Error::OutputSizeOverflow)?;
        if total > limit as u64 {
            return Err(Error::OutputSizeTooLarge {
                output_size: total,
                max_output_size: limit,
            });
        }
    }
    Ok(())
}

fn checked_output_len(current: usize, add: usize) -> Result<usize> {
    current.checked_add(add).ok_or(Error::OutputSizeOverflow)
}

struct Cursor<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }

    fn is_empty(&self) -> bool {
        self.pos == self.src.len()
    }

    /// Offset into the original input, for errors that need to name a place.
    fn position(&self) -> usize {
        self.pos
    }

    fn remaining_slice(&self) -> &'a [u8] {
        &self.src[self.pos..]
    }

    fn read_u8(&mut self) -> Result<u8> {
        let byte = *self.src.get(self.pos).ok_or(Error::UnexpectedEof)?;
        self.pos += 1;
        Ok(byte)
    }

    fn read_u32_le(&mut self) -> Result<u32> {
        let bytes = self.read_exact(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    fn read_exact(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self.pos.checked_add(len).ok_or(Error::UnexpectedEof)?;
        let bytes = self.src.get(self.pos..end).ok_or(Error::UnexpectedEof)?;
        self.pos = end;
        Ok(bytes)
    }
}

impl std::fmt::Debug for Decoder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The scratch buffers are an implementation detail; report only their
        // retained capacity, which is the part a caller can act on.
        f.debug_struct("Decoder")
            .field("literals_capacity", &self.scratch.literals.capacity())
            .field("sequences_capacity", &self.scratch.sequences.capacity())
            .finish()
    }
}
