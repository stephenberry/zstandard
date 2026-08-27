use crate::{
    block::{BLOCK_SIZE_MAX, BlockHeader},
    error::{Error, Result},
    outbuf::OutBuf,
};

/// Magic number that introduces every Zstandard frame (little-endian on the wire).
pub const ZSTD_MAGIC_NUMBER: u32 = 0xFD2F_B528;
/// Base value of the 16 reserved skippable-frame magic numbers (`0x184D2A5?`).
pub const SKIPPABLE_MAGIC_BASE: u32 = 0x184D_2A50;
const RESERVED_BIT: u8 = 1 << 3;

/// Frame envelope, upstream's `ZSTD_c_format` / `ZSTD_d_format`.
///
/// Only the four-byte magic number differs. Everything after it — the frame
/// header descriptor, the blocks, the optional checksum — is identical, so a
/// magicless frame is a standard frame with its first four bytes removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Format {
    /// A standard Zstandard frame, beginning with [`ZSTD_MAGIC_NUMBER`].
    #[default]
    Zstd1,
    /// A frame with the magic number omitted, upstream's `ZSTD_f_zstd1_magicless`.
    ///
    /// For callers whose own framing already identifies the payload and who
    /// would rather not pay four bytes per frame to say it twice. Such a frame
    /// cannot be recognised, only asserted: a decoder has to be told to expect
    /// one, so [`DecoderOptions::format`](crate::DecoderOptions::format) has to
    /// be set to match. Skippable frames are not magicless in either
    /// direction — they are identified by a magic number and nothing else — so
    /// a magicless stream cannot carry one.
    Zstd1Magicless,
}

impl Format {
    /// Bytes this format spends on the magic number: four, or none.
    pub(crate) const fn magic_size(self) -> usize {
        match self {
            Self::Zstd1 => 4,
            Self::Zstd1Magicless => 0,
        }
    }
}

fn write_frame_magic(dst: &mut OutBuf<'_>, format: Format) {
    if format.magic_size() != 0 {
        dst.extend_from_slice(&ZSTD_MAGIC_NUMBER.to_le_bytes());
    }
}

/// Result of [`parse_frame_header`]: either a Zstandard frame or a skippable frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameHeader {
    /// A standard Zstandard frame whose payload contains compressed blocks.
    Zstandard(ZstandardFrameHeader),
    /// A skippable frame carrying opaque user data the decoder must pass over.
    Skippable(SkippableFrame),
}

/// Parsed contents of a Zstandard frame header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ZstandardFrameHeader {
    /// Size in bytes of the parsed header (use this to advance past the header).
    pub header_size: usize,
    /// Effective window size in bytes the decoder must support for this frame.
    pub window_size: u64,
    /// Maximum block payload size the frame can carry, capped at [`crate::BLOCK_SIZE_MAX`].
    pub block_size_max: u32,
    /// Decoded content size declared by the header, if the encoder wrote one.
    pub content_size: Option<u64>,
    /// Dictionary id referenced by this frame, if any.
    pub dictionary_id: Option<u32>,
    /// `true` if a four-byte XXH64-truncated content checksum trails the last block.
    pub checksum: bool,
    /// `true` if the frame is encoded in single-segment mode (one block, no window descriptor).
    pub single_segment: bool,
}

/// Parsed eight-byte header of a skippable frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SkippableFrame {
    /// Size in bytes of the header (always 8 for skippable frames).
    pub header_size: usize,
    /// Low four bits of the magic number (`0..16`), distinguishing skippable variants.
    pub magic_variant: u8,
    /// Payload size in bytes that follows the header.
    pub size: u32,
}

/// Identify and parse the frame header at the start of `src`. Returns
/// [`Error::BadMagic`] when the leading magic is neither Zstandard nor a
/// skippable variant.
pub fn parse_frame_header(src: &[u8]) -> Result<FrameHeader> {
    parse_frame_header_with_format(src, Format::Zstd1)
}

/// [`parse_frame_header`], for input written in `format`.
///
/// Under [`Format::Zstd1Magicless`] there is no magic number to read, so the
/// bytes are taken to be a Zstandard frame header and nothing else can be
/// recognised — including a skippable frame, whose magic number is the only
/// thing that identifies it.
pub fn parse_frame_header_with_format(src: &[u8], format: Format) -> Result<FrameHeader> {
    if format == Format::Zstd1Magicless {
        return parse_zstandard_frame_header(src, format);
    }
    if src.len() < 4 {
        return Err(Error::UnexpectedEof);
    }

    let magic = read_u32_le(&src[..4]);
    if magic == ZSTD_MAGIC_NUMBER {
        parse_zstandard_frame_header(src, format)
    } else if is_skippable_magic(magic) {
        parse_skippable_frame_header(src, magic)
    } else {
        Err(Error::BadMagic(magic))
    }
}

/// Encode a skippable frame carrying `payload`. `magic_variant` selects one of
/// the 16 skippable magic numbers and must lie in `0..16`; the output is a
/// fresh `Vec<u8>` containing the eight-byte header followed by the payload.
pub fn write_skippable_frame(magic_variant: u8, payload: &[u8]) -> Result<Vec<u8>> {
    if magic_variant >= 16 {
        return Err(Error::InvalidParameter(
            "skippable magic variant must be in 0..16",
        ));
    }

    let payload_size = u32::try_from(payload.len())
        .map_err(|_| Error::ContentSizeTooLarge(payload.len() as u64))?;

    let mut out = Vec::with_capacity(8 + payload.len());
    out.extend_from_slice(&(SKIPPABLE_MAGIC_BASE + u32::from(magic_variant)).to_le_bytes());
    out.extend_from_slice(&payload_size.to_le_bytes());
    out.extend_from_slice(payload);
    Ok(out)
}

/// What one frame costs on the wire, and the most it could produce.
struct FrameSizeInfo {
    /// Bytes the frame occupies, header through checksum.
    compressed_size: usize,
    /// Ceiling on what the frame decodes to. Equal to `declared_size` when
    /// there is one; otherwise derived from the block count.
    decompressed_bound: u64,
    /// What the frame's header says it decodes to.
    ///
    /// Separate from the bound because the two answer different questions and
    /// only this one can say "the encoder did not tell us". A skippable frame
    /// declares `Some(0)`: it produces nothing, and that is known rather than
    /// missing.
    declared_size: Option<u64>,
}

/// Measure the frame at the start of `src` without decoding it.
///
/// The walk reads the frame header, then each block header in turn, skipping
/// the payload each names. That is enough to find the frame's end and, for a
/// frame that did not declare its content size, to bound its output at one
/// maximal block per block header.
///
/// It deliberately validates no more than it must. A block wider than the
/// frame's declared maximum, a checksum that will not match, literals that
/// will not parse: all of that is the decoder's to reject, and rejecting it
/// here would mean this function and the decoder each holding half of one
/// rule. What it does reject is what stops the walk itself: a reserved block
/// type, and input that runs out.
fn frame_size_info(src: &[u8], format: Format) -> Result<FrameSizeInfo> {
    let header = match parse_frame_header_with_format(src, format)? {
        FrameHeader::Skippable(skippable) => {
            let compressed_size = skippable
                .header_size
                .checked_add(skippable.size as usize)
                .ok_or(Error::OutputSizeOverflow)?;
            if src.len() < compressed_size {
                return Err(Error::UnexpectedEof);
            }
            // A skippable frame decodes to nothing at all, which is what makes
            // it skippable.
            return Ok(FrameSizeInfo {
                compressed_size,
                decompressed_bound: 0,
                declared_size: Some(0),
            });
        }
        FrameHeader::Zstandard(header) => header,
    };

    let mut pos = header.header_size;
    let mut blocks = 0u64;
    loop {
        let block_header = BlockHeader::parse(src.get(pos..).ok_or(Error::UnexpectedEof)?)?;
        let advance = BlockHeader::SIZE
            .checked_add(block_header.payload_size())
            .ok_or(Error::OutputSizeOverflow)?;
        pos = pos.checked_add(advance).ok_or(Error::OutputSizeOverflow)?;
        if pos > src.len() {
            return Err(Error::UnexpectedEof);
        }
        blocks += 1;
        if block_header.last_block {
            break;
        }
    }

    if header.checksum {
        pos = pos.checked_add(4).ok_or(Error::OutputSizeOverflow)?;
        if pos > src.len() {
            return Err(Error::UnexpectedEof);
        }
    }

    // An undeclared content size is bounded rather than known: every block
    // decodes to at most `block_size_max`, so the block count is the ceiling.
    // Saturating, because a hostile frame can name far more blocks than any
    // honest one and the answer to that is a very large number, not a wrap.
    let decompressed_bound = header
        .content_size
        .unwrap_or_else(|| blocks.saturating_mul(u64::from(header.block_size_max)));

    Ok(FrameSizeInfo {
        compressed_size: pos,
        decompressed_bound,
        declared_size: header.content_size,
    })
}

/// Fold `f` over every frame in `src`, left to right.
fn for_each_frame<T>(
    src: &[u8],
    format: Format,
    mut seed: T,
    mut fold: impl FnMut(T, &FrameSizeInfo) -> Result<T>,
) -> Result<T> {
    if src.is_empty() {
        return Err(Error::UnexpectedEof);
    }

    let mut pos = 0;
    while pos < src.len() {
        let info = frame_size_info(&src[pos..], format)?;
        seed = fold(seed, &info)?;
        // `frame_size_info` cannot report a zero-length frame — the shortest
        // thing it accepts is a header plus one block header — so this always
        // advances and the loop always terminates.
        debug_assert!(info.compressed_size > 0);
        pos += info.compressed_size;
    }

    Ok(seed)
}

/// Size in bytes of the first frame in `src`, upstream's
/// `ZSTD_findFrameCompressedSize`.
///
/// Answers "where does this frame end" for input that holds more than one, so
/// a caller carrying frames inside their own container can hand exactly one
/// frame to [`decode_all`](crate::decode_all) instead of the whole buffer.
/// Skippable frames are measured too, and report the size the reader must skip.
///
/// The frame is measured, not verified: this walks the block headers and does
/// not decode. A frame this accepts can still fail to decode.
///
/// ```
/// use zstandard::{encode_all, find_frame_compressed_size};
///
/// let mut stream = encode_all(b"first")?;
/// let first_len = stream.len();
/// stream.extend_from_slice(&encode_all(b"second")?);
///
/// assert_eq!(find_frame_compressed_size(&stream)?, first_len);
/// # Ok::<(), zstandard::Error>(())
/// ```
pub fn find_frame_compressed_size(src: &[u8]) -> Result<usize> {
    find_frame_compressed_size_with_format(src, Format::Zstd1)
}

/// [`find_frame_compressed_size`], for input written in `format`.
pub fn find_frame_compressed_size_with_format(src: &[u8], format: Format) -> Result<usize> {
    Ok(frame_size_info(src, format)?.compressed_size)
}

/// Upper bound on what every frame in `src` decodes to, upstream's
/// `ZSTD_decompressBound`.
///
/// Unlike [`decompressed_size`] this always produces a number, because a frame
/// that declared no content size is still bounded by its block count: no block
/// decodes to more than the frame's maximum block size.
///
/// # This is not a promise about the input
///
/// The bound comes from the frame's own headers, which for untrusted input are
/// the attacker's to choose. Four bytes of RLE block encode 128 KiB of output,
/// so a small hostile input can carry an enormous honest-looking bound.
/// **Do not allocate this many bytes without checking it against a budget** —
/// use [`DecoderOptions::max_output_size`](crate::DecoderOptions::max_output_size),
/// which the decoder enforces as it goes, to cap what a decode may produce.
///
/// ```
/// use zstandard::{decompress_bound, encode_all};
///
/// let payload = b"bounded before it is decoded";
/// let compressed = encode_all(payload)?;
///
/// // The default encoder declares the content size, so the bound is exact.
/// assert_eq!(decompress_bound(&compressed)?, payload.len() as u64);
/// # Ok::<(), zstandard::Error>(())
/// ```
pub fn decompress_bound(src: &[u8]) -> Result<u64> {
    decompress_bound_with_format(src, Format::Zstd1)
}

/// [`decompress_bound`], for input written in `format`.
pub fn decompress_bound_with_format(src: &[u8], format: Format) -> Result<u64> {
    for_each_frame(src, format, 0u64, |total: u64, info| {
        total
            .checked_add(info.decompressed_bound)
            .ok_or(Error::OutputSizeOverflow)
    })
}

/// Total decoded size every frame in `src` declares, upstream's
/// `ZSTD_findDecompressedSize`.
///
/// `Ok(None)` means at least one frame did not declare one, which is the
/// ordinary case for a frame written by a streaming encoder that was not told
/// its input length up front. Reach for [`decompress_bound`] then, which always
/// answers.
///
/// The size is the encoder's claim, and the decoder checks it against what the
/// frame actually produces ([`Error::ContentSizeMismatch`]). Sizing a buffer
/// from it is safe against a *truthful* encoder and no more; see the warning on
/// [`decompress_bound`], which applies here in full.
///
/// [`parse_frame_header`] answers the same question for a single frame and
/// hands back the rest of the header with it.
///
/// ```
/// use zstandard::{decompressed_size, encode_all};
///
/// let mut stream = encode_all(b"first")?;
/// stream.extend_from_slice(&encode_all(b"second")?);
///
/// assert_eq!(decompressed_size(&stream)?, Some(11));
/// # Ok::<(), zstandard::Error>(())
/// ```
pub fn decompressed_size(src: &[u8]) -> Result<Option<u64>> {
    decompressed_size_with_format(src, Format::Zstd1)
}

/// [`decompressed_size`], for input written in `format`.
pub fn decompressed_size_with_format(src: &[u8], format: Format) -> Result<Option<u64>> {
    for_each_frame(src, format, Some(0u64), |total, info| {
        let Some(total) = total else {
            return Ok(None);
        };
        let Some(declared) = info.declared_size else {
            return Ok(None);
        };
        total
            .checked_add(declared)
            .map(Some)
            .ok_or(Error::OutputSizeOverflow)
    })
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_single_segment_header(dst: &mut OutBuf<'_>, content_size: u64, checksum: bool) {
    write_single_segment_header_with_dict(dst, content_size, checksum, None, Format::Zstd1);
}

pub(crate) fn write_single_segment_header_with_dict(
    dst: &mut OutBuf<'_>,
    content_size: u64,
    checksum: bool,
    dictionary_id: Option<u32>,
    format: Format,
) {
    write_frame_magic(dst, format);

    let (fcs_flag, encoded_fcs) = encode_frame_content_size(content_size);
    let (dict_id_flag, encoded_dict_id) = encode_dictionary_id(dictionary_id);
    let mut descriptor = fcs_flag << 6;
    descriptor |= 1 << 5;
    if checksum {
        descriptor |= 1 << 2;
    }
    descriptor |= dict_id_flag;
    dst.push(descriptor);
    dst.extend_from_slice(&encoded_dict_id);
    dst.extend_from_slice(&encoded_fcs);
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn write_windowed_header(
    dst: &mut OutBuf<'_>,
    window_size: u64,
    checksum: bool,
) -> Result<()> {
    write_windowed_header_with_dict(dst, window_size, checksum, None, Format::Zstd1)
}

pub(crate) fn write_windowed_header_with_dict(
    dst: &mut OutBuf<'_>,
    window_size: u64,
    checksum: bool,
    dictionary_id: Option<u32>,
    format: Format,
) -> Result<()> {
    write_frame_magic(dst, format);

    let (dict_id_flag, encoded_dict_id) = encode_dictionary_id(dictionary_id);
    let mut descriptor = 0u8;
    if checksum {
        descriptor |= 1 << 2;
    }
    descriptor |= dict_id_flag;
    dst.push(descriptor);
    dst.push(encode_window_descriptor(window_size.max(1 << 10))?);
    dst.extend_from_slice(&encoded_dict_id);
    Ok(())
}

/// Largest `Window_Size` a decoder accepts without an explicit opt-in.
///
/// The window descriptor can encode far larger values, but upstream's
/// `ZSTD_WINDOWLOG_LIMIT_DEFAULT` is 27, and its CLI refuses any frame above
/// this with "Frame requires too much memory for decoding". Declaring more
/// than this makes a frame undecodable by the reference implementation at its
/// default settings, so the encoder never does.
pub(crate) const MAX_DECLARABLE_WINDOW_SIZE: usize = 1 << 27;

/// Write a frame header that declares both a window and a content size.
///
/// `Single_Segment_flag` and `Frame_Content_Size` are independent fields: a
/// frame may bound its window *and* state how much it decodes to. The one-shot
/// encoder needs both — the window so decoders size their buffers to what the
/// level actually needs rather than to the payload, and the content size
/// because APIs shaped like `ZSTD_decompress` and `ZSTD_getFrameContentSize`
/// require it up front and fail outright without it.
pub(crate) fn write_windowed_header_with_content_size(
    dst: &mut OutBuf<'_>,
    window_size: u64,
    content_size: u64,
    checksum: bool,
    dictionary_id: Option<u32>,
    format: Format,
) -> Result<()> {
    write_frame_magic(dst, format);

    let (mut fcs_flag, mut encoded_fcs) = encode_frame_content_size(content_size);
    if fcs_flag == 0 {
        // Flag 0 means "absent" outside single-segment mode, so promote the
        // smallest field that can actually carry a value. The two-byte form
        // stores `content_size - 256`, which is why flag 0 is only reachable
        // for sizes below that offset.
        fcs_flag = 1;
        encoded_fcs = ((content_size.saturating_sub(256)) as u16)
            .to_le_bytes()
            .to_vec();
    }

    let (dict_id_flag, encoded_dict_id) = encode_dictionary_id(dictionary_id);
    let mut descriptor = fcs_flag << 6;
    if checksum {
        descriptor |= 1 << 2;
    }
    descriptor |= dict_id_flag;
    dst.push(descriptor);
    dst.push(encode_window_descriptor(window_size.max(1 << 10))?);
    dst.extend_from_slice(&encoded_dict_id);
    dst.extend_from_slice(&encoded_fcs);
    Ok(())
}

pub(crate) fn read_u24_le(src: &[u8]) -> u32 {
    debug_assert!(src.len() >= 3);
    (src[0] as u32) | ((src[1] as u32) << 8) | ((src[2] as u32) << 16)
}

pub(crate) fn read_u32_le(src: &[u8]) -> u32 {
    debug_assert!(src.len() >= 4);
    u32::from_le_bytes([src[0], src[1], src[2], src[3]])
}

fn parse_zstandard_frame_header(src: &[u8], format: Format) -> Result<FrameHeader> {
    let magic_size = format.magic_size();
    let Some(&descriptor) = src.get(magic_size) else {
        return Err(Error::UnexpectedEof);
    };
    if descriptor & RESERVED_BIT != 0 {
        return Err(Error::Corruption("frame header reserved bit is set"));
    }

    let single_segment = descriptor & (1 << 5) != 0;
    let checksum = descriptor & (1 << 2) != 0;
    let dictionary_id_size = match descriptor & 0x3 {
        0 => 0usize,
        1 => 1,
        2 => 2,
        _ => 4,
    };
    let fcs_field_size = frame_content_size_field_size(descriptor >> 6, single_segment);

    let mut pos = magic_size + 1;
    let window_size = if single_segment {
        0
    } else {
        let window_descriptor = *src.get(pos).ok_or(Error::UnexpectedEof)?;
        pos += 1;
        decode_window_descriptor(window_descriptor)
    };

    let dictionary_id = if dictionary_id_size == 0 {
        None
    } else {
        let end = pos
            .checked_add(dictionary_id_size)
            .ok_or(Error::UnexpectedEof)?;
        let field = src.get(pos..end).ok_or(Error::UnexpectedEof)?;
        pos = end;
        let value = match field.len() {
            1 => field[0] as u32,
            2 => u16::from_le_bytes([field[0], field[1]]) as u32,
            4 => u32::from_le_bytes([field[0], field[1], field[2], field[3]]),
            _ => unreachable!(),
        };
        (value != 0).then_some(value)
    };

    let content_size = if fcs_field_size == 0 {
        None
    } else {
        let end = pos
            .checked_add(fcs_field_size)
            .ok_or(Error::UnexpectedEof)?;
        let field = src.get(pos..end).ok_or(Error::UnexpectedEof)?;
        pos = end;
        Some(decode_frame_content_size(field)?)
    };

    let window_size = if single_segment {
        content_size.ok_or(Error::Corruption(
            "single-segment frames must include a content size",
        ))?
    } else {
        window_size
    };

    let block_size_max = window_size.min(BLOCK_SIZE_MAX as u64) as u32;

    Ok(FrameHeader::Zstandard(ZstandardFrameHeader {
        header_size: pos,
        window_size,
        block_size_max,
        content_size,
        dictionary_id,
        checksum,
        single_segment,
    }))
}

fn parse_skippable_frame_header(src: &[u8], magic: u32) -> Result<FrameHeader> {
    if src.len() < 8 {
        return Err(Error::UnexpectedEof);
    }
    let size = read_u32_le(&src[4..8]);
    let magic_variant = (magic - SKIPPABLE_MAGIC_BASE) as u8;
    Ok(FrameHeader::Skippable(SkippableFrame {
        header_size: 8,
        magic_variant,
        size,
    }))
}

fn is_skippable_magic(magic: u32) -> bool {
    (magic & 0xFFFF_FFF0) == SKIPPABLE_MAGIC_BASE
}

fn frame_content_size_field_size(flag: u8, single_segment: bool) -> usize {
    match flag {
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        3 => 8,
        _ => unreachable!(),
    }
}

/// The smallest `Window_Size` a frame header can declare that is at least
/// `window_size`.
///
/// The window descriptor is a coarse `(exponent, mantissa)` pair, so a window
/// the encoder computed exactly is written as the next representable value up.
/// Anything checking a declared window against what the encoder meant has to
/// round the same way or it is off by up to an eighth.
#[cfg(feature = "internal-fuzz")]
pub(crate) fn representable_window_size(window_size: u64) -> Result<u64> {
    Ok(decode_window_descriptor(encode_window_descriptor(
        window_size,
    )?))
}

fn decode_window_descriptor(descriptor: u8) -> u64 {
    let exponent = descriptor >> 3;
    let mantissa = descriptor & 0x7;
    let window_log = 10u64 + u64::from(exponent);
    let window_base = 1u64 << window_log;
    let window_add = (window_base / 8) * u64::from(mantissa);
    window_base + window_add
}

fn decode_frame_content_size(field: &[u8]) -> Result<u64> {
    let value = match field.len() {
        1 => field[0] as u64,
        2 => u16::from_le_bytes([field[0], field[1]]) as u64 + 256,
        4 => u32::from_le_bytes([field[0], field[1], field[2], field[3]]) as u64,
        8 => u64::from_le_bytes([
            field[0], field[1], field[2], field[3], field[4], field[5], field[6], field[7],
        ]),
        _ => return Err(Error::Corruption("invalid frame content size field length")),
    };
    Ok(value)
}

fn encode_frame_content_size(content_size: u64) -> (u8, Vec<u8>) {
    if content_size <= 255 {
        (0, vec![content_size as u8])
    } else if content_size <= 65_791 {
        (1, ((content_size - 256) as u16).to_le_bytes().to_vec())
    } else if content_size <= u32::MAX as u64 {
        (2, (content_size as u32).to_le_bytes().to_vec())
    } else {
        (3, content_size.to_le_bytes().to_vec())
    }
}

fn encode_dictionary_id(dictionary_id: Option<u32>) -> (u8, Vec<u8>) {
    let Some(dictionary_id) = dictionary_id.filter(|&id| id != 0) else {
        return (0, Vec::new());
    };

    if dictionary_id <= u8::MAX as u32 {
        (1, vec![dictionary_id as u8])
    } else if dictionary_id <= u16::MAX as u32 {
        (2, (dictionary_id as u16).to_le_bytes().to_vec())
    } else {
        (3, dictionary_id.to_le_bytes().to_vec())
    }
}

fn encode_window_descriptor(window_size: u64) -> Result<u8> {
    let requested = window_size.max(1 << 10);

    for exponent in 0..=31u8 {
        let window_log = 10u32 + u32::from(exponent);
        let window_base = 1u64 << window_log;
        let window_step = window_base / 8;
        for mantissa in 0..=7u8 {
            let candidate = window_base + (window_step * u64::from(mantissa));
            if candidate >= requested {
                return Ok((exponent << 3) | mantissa);
            }
        }
    }

    Err(Error::InvalidParameter("window_size is too large"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windowed_headers_roundtrip() {
        for window_size in [1u64, 1_024, 32_768, 131_072] {
            let mut encoded = Vec::new();
            write_windowed_header(&mut OutBuf::growable(&mut encoded), window_size, true).unwrap();

            let FrameHeader::Zstandard(header) = parse_frame_header(&encoded).unwrap() else {
                panic!("unexpected skippable frame");
            };
            assert!(!header.single_segment);
            assert!(header.checksum);
            assert!(header.window_size >= window_size.max(1 << 10));
        }
    }
}
