use crate::{
    entropy::huff0::{
        HuffmanDTable, decompress_1x_using_dtable, decompress_4x_using_dtable,
        prefers_double_symbol_decoder, read_dtable_x1, read_dtable_x2,
    },
    error::{Error, Result},
    frame::{read_u24_le, read_u32_le},
};

#[derive(Default)]
pub(crate) struct LiteralsState {
    huffman_table: Option<HuffmanDTable>,
}

impl LiteralsState {
    pub(crate) fn with_huffman_table(huffman_table: Option<HuffmanDTable>) -> Self {
        Self { huffman_table }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CompressedLiteralsHeader {
    header_size: usize,
    regenerated_size: usize,
    compressed_size: usize,
    four_streams: bool,
}

/// Decoded literals, followed by the headroom the sequence executor over-reads.
///
/// The executor copies literals in fixed 16-byte units to match C's
/// `ZSTD_execSequence`, so a run starting within 16 bytes of the last literal
/// reads past it. C makes that legal by giving its literals buffer
/// `WILDCOPY_OVERLENGTH` bytes of slack.
///
/// Reserving spare `Vec` capacity is not the same thing in Rust, which is what
/// this code did before: a slice's provenance ends at its length even when the
/// allocation continues, and spare capacity is uninitialized, so reading it is
/// undefined behavior on both counts despite the bytes being inside the
/// allocation. Miri reports it as a read whose tag is not in the borrow stack.
/// Here the headroom is inside the slice and initialized, and `len` records how
/// much of it is actually literals.
#[derive(Clone, Copy)]
pub(crate) struct DecodedLiterals<'a> {
    padded: &'a [u8],
    len: usize,
}

impl<'a> DecodedLiterals<'a> {
    fn new(padded: &'a [u8], len: usize) -> Self {
        debug_assert!(
            padded.len() >= len + LITERAL_OVERREAD_HEADROOM,
            "literals must carry {LITERAL_OVERREAD_HEADROOM} bytes of over-read headroom"
        );
        Self { padded, len }
    }

    /// The literal bytes alone, for callers that consume them by value.
    pub(crate) fn as_slice(&self) -> &'a [u8] {
        &self.padded[..self.len]
    }

    pub(crate) fn len(&self) -> usize {
        self.len
    }

    /// Start of the literals. Readable for `len() + LITERAL_OVERREAD_HEADROOM`
    /// bytes, which is what lets the executor copy in fixed-width units without
    /// checking the tail.
    pub(crate) fn padded_ptr(&self) -> *const u8 {
        self.padded.as_ptr()
    }
}

/// Decode a literals section into a fresh buffer.
///
/// The decoders proper use [`decode_literals_section_into`] with a scratch
/// buffer they keep across blocks; this allocating form is for the tests and
/// round-trip checks that want the literals on their own.
#[cfg(any(test, feature = "internal-fuzz"))]
pub(crate) fn decode_literals_section(
    src: &[u8],
    state: &mut LiteralsState,
    block_size_max: usize,
) -> Result<(Vec<u8>, usize)> {
    let mut scratch = Vec::new();
    let (consumed, literals) =
        decode_literals_section_into(src, state, block_size_max, &mut scratch)?;
    // The returned slice may borrow from src (zero-copy raw) or from scratch.
    // Callers of this function need an owned Vec, so copy if needed.
    Ok((literals.as_slice().to_vec(), consumed))
}

/// Decode a literals section, returning `(consumed_bytes, literals_slice)`.
///
/// For **raw literals** with enough trailing headroom in `src` for SIMD
/// over-read (16 bytes), the returned slice points directly into `src` —
/// **zero copy**, matching C zstd's `litPtr = istart + lhSize` approach.
/// This avoids a full memcpy of the literal data into the scratch buffer.
///
/// For RLE, Huffman, or raw literals near the end of `src` (insufficient
/// headroom), the data is decoded/copied into `scratch` and the returned
/// slice borrows from it.
pub(crate) fn decode_literals_section_into<'a>(
    src: &'a [u8],
    state: &mut LiteralsState,
    block_size_max: usize,
    scratch: &'a mut Vec<u8>,
) -> Result<(usize, DecodedLiterals<'a>)> {
    let header0 = *src.first().ok_or(Error::UnexpectedEof)?;
    let block_type = header0 & 0x3;
    let size_format = (header0 >> 2) & 0x3;

    match block_type {
        0 => decode_raw_literals_into(src, size_format, block_size_max, scratch),
        1 => decode_rle_literals_into(src, size_format, block_size_max, scratch),
        2 | 3 => decode_huffman_literals_into(
            src,
            block_type,
            size_format,
            state,
            block_size_max,
            scratch,
        ),
        _ => unreachable!(),
    }
}

#[cfg(any(feature = "internal-trace", test))]
// Compiled into test builds so unit tests can reach the trace API without
// turning the feature on. Not every entry point has a default-feature test,
// so under `cfg(test)` alone some are legitimately unused.
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
/// Where a block's literals section ends, without decoding it.
///
/// Trace tooling has to find the sequences section, which begins wherever the
/// literals section stops. That boundary is the header plus the section's
/// *on-wire* payload, and for a compressed section the on-wire payload is
/// `Compressed_Size`, not `Regenerated_Size`. Reaching for the wrong one is
/// silent rather than loud: it yields a plausible section size that lands in
/// the middle of the Huffman payload, so the sequences header is then read out
/// of arbitrary bytes and reports a sequence count that looks like data. This
/// exists so tools consume the decoder's parser instead of re-deriving the bit
/// widths and drifting from it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LiteralsSectionLayout {
    /// Bytes of literals section header.
    pub header_size: usize,
    /// Bytes the section occupies in the block, header included.
    pub section_size: usize,
    /// Bytes the section decodes to.
    pub regenerated_size: usize,
    /// Whether the literals are Huffman-coded rather than stored raw or RLE.
    pub compressed: bool,
}

#[cfg(any(feature = "internal-trace", test))]
// Compiled into test builds so unit tests can reach the trace API without
// turning the feature on. Not every entry point has a default-feature test,
// so under `cfg(test)` alone some are legitimately unused.
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
/// Measure a block's literals section without decoding it.
///
/// `src` must start at the block payload. The section is only parsed far enough
/// to size it, so this accepts a section whose payload is truncated or whose
/// Huffman table is malformed; only the header itself has to be well formed.
pub fn parse_literals_section_layout(src: &[u8]) -> Result<LiteralsSectionLayout> {
    let header0 = *src.first().ok_or(Error::UnexpectedEof)?;
    let block_type = header0 & 0x3;
    let size_format = (header0 >> 2) & 0x3;

    match block_type {
        // Raw stores its literals verbatim, so the on-wire payload is the
        // regenerated size. RLE stores a single byte that repeats.
        0 | 1 => {
            let (header_size, regenerated_size) = decode_raw_or_rle_size(src, size_format)?;
            let payload = if block_type == 0 { regenerated_size } else { 1 };
            Ok(LiteralsSectionLayout {
                header_size,
                section_size: header_size
                    .checked_add(payload)
                    .ok_or(Error::OutputSizeOverflow)?,
                regenerated_size,
                compressed: false,
            })
        }
        2 | 3 => {
            let header = decode_compressed_literals_header(src, size_format)?;
            Ok(LiteralsSectionLayout {
                header_size: header.header_size,
                section_size: header
                    .header_size
                    .checked_add(header.compressed_size)
                    .ok_or(Error::OutputSizeOverflow)?,
                regenerated_size: header.regenerated_size,
                compressed: true,
            })
        }
        _ => unreachable!(),
    }
}

/// 16 bytes of SIMD over-read headroom required after the last literal.
const LITERAL_OVERREAD_HEADROOM: usize = 16;

fn decode_raw_literals_into<'a>(
    src: &'a [u8],
    size_format: u8,
    block_size_max: usize,
    scratch: &'a mut Vec<u8>,
) -> Result<(usize, DecodedLiterals<'a>)> {
    let (header_size, regenerated_size) = decode_raw_or_rle_size(src, size_format)?;
    ensure_regenerated_size(regenerated_size, block_size_max)?;
    let end = header_size
        .checked_add(regenerated_size)
        .ok_or(Error::OutputSizeOverflow)?;
    if end > src.len() {
        return Err(Error::UnexpectedEof);
    }

    // Zero-copy path: if there are enough bytes after the literals for the
    // SIMD over-read, point directly into the compressed input buffer.
    // This matches C zstd's `litPtr = istart + lhSize` optimization. The slice
    // deliberately extends past the literals: the over-read has to be inside
    // it, not merely inside `src`, because that is where the provenance stops.
    if end + LITERAL_OVERREAD_HEADROOM <= src.len() {
        return Ok((
            end,
            DecodedLiterals::new(
                &src[header_size..end + LITERAL_OVERREAD_HEADROOM],
                regenerated_size,
            ),
        ));
    }

    // Fallback: near the end of src, copy into scratch and materialize the
    // headroom as real zeroed bytes.
    let raw_literals = &src[header_size..end];
    scratch.clear();
    scratch.extend_from_slice(raw_literals);
    scratch.resize(regenerated_size + LITERAL_OVERREAD_HEADROOM, 0);
    Ok((end, DecodedLiterals::new(scratch, regenerated_size)))
}

fn decode_rle_literals_into<'a>(
    src: &[u8],
    size_format: u8,
    block_size_max: usize,
    scratch: &'a mut Vec<u8>,
) -> Result<(usize, DecodedLiterals<'a>)> {
    let (header_size, regenerated_size) = decode_raw_or_rle_size(src, size_format)?;
    ensure_regenerated_size(regenerated_size, block_size_max)?;
    let byte = *src.get(header_size).ok_or(Error::UnexpectedEof)?;
    scratch.clear();
    scratch.resize(regenerated_size, byte);
    scratch.resize(regenerated_size + LITERAL_OVERREAD_HEADROOM, 0);
    Ok((
        header_size + 1,
        DecodedLiterals::new(scratch, regenerated_size),
    ))
}

fn decode_huffman_literals_into<'a>(
    src: &[u8],
    block_type: u8,
    size_format: u8,
    state: &mut LiteralsState,
    block_size_max: usize,
    scratch: &'a mut Vec<u8>,
) -> Result<(usize, DecodedLiterals<'a>)> {
    let header = decode_compressed_literals_header(src, size_format)?;
    ensure_regenerated_size(header.regenerated_size, block_size_max)?;

    let payload_end = header
        .header_size
        .checked_add(header.compressed_size)
        .ok_or(Error::OutputSizeOverflow)?;
    let payload = src
        .get(header.header_size..payload_end)
        .ok_or(Error::UnexpectedEof)?;

    // A decode table is 8 or 16 KiB, and it is `Copy`, so routing it through a
    // local cost three passes over it per block that nothing needed:
    // `default()` zeroed a stack copy, moving it out of the `if` copied it
    // again, and storing it back into `state` copied it a third time. The
    // treeless branch paid one too, because reading the table out of the
    // `Option` by value copies it. On a 4 MiB frame at level 16 that was 24 KiB
    // of memmove and 8 KiB of memset per block, and it showed up as 3% of
    // decode. Build in place instead.
    let table_size = if block_type == 2 {
        // Which shape to build is a throughput choice, not a format one: both
        // decode the same bits. C runs this cost model only for four-stream
        // literals — a single-stream section always builds the single-symbol
        // table, `ZSTD_decodeLiteralsBlock` calling `HUF_decompress1X1_DCtx_wksp`
        // directly. That guard is mirrored rather than needed: the only size
        // format carrying a single stream tops out at 1023 regenerated bytes,
        // far below where the wider table starts paying for itself, so the
        // model would answer the same way. See the test named for it.
        //
        // The ratio is measured against the whole payload including this table
        // description, which is what C passes as `litCSize`.
        let result = if header.four_streams
            && prefers_double_symbol_decoder(header.regenerated_size, payload.len())
        {
            read_dtable_x2(
                payload,
                HuffmanDTable::double_slot(&mut state.huffman_table),
            )
        } else {
            read_dtable_x1(
                payload,
                HuffmanDTable::single_slot(&mut state.huffman_table),
            )
        };
        match result {
            Ok(table_size) => table_size,
            Err(error) => {
                // The slot now holds a half-written table. Callers are expected
                // to abandon the frame on error, but a later treeless block
                // reaching this one would be silent corruption rather than a
                // failure, so drop it rather than rely on that.
                state.huffman_table = None;
                return Err(error);
            }
        }
    } else {
        0
    };
    let dtable = state.huffman_table.as_ref().ok_or(Error::Corruption(
        "treeless literals require a prior Huff0 table",
    ))?;

    let streams = payload.get(table_size..).ok_or(Error::UnexpectedEof)?;
    if streams.is_empty() {
        return Err(Error::Corruption(
            "compressed literals payload is missing Huff0 streams",
        ));
    }

    // Grow to cover the literals and the over-read headroom, but do not clear
    // first: the Huffman decoders write every byte of the destination they are
    // given, so zeroing it beforehand is a second full pass over as much as
    // 128 KiB per block that the next instruction overwrites.
    //
    // That leaves the headroom holding whatever the previous block left there
    // rather than zeros, which is what the contract already allows: the raw
    // zero-copy path above points its headroom straight at the compressed input
    // that follows the literals. What the executor needs is for those bytes to
    // be *initialized*, because it over-copies them into spare output capacity
    // that is then never counted — not for them to hold any particular value.
    // Growing the buffer once initializes it for every block that follows.
    let padded_size = header
        .regenerated_size
        .checked_add(LITERAL_OVERREAD_HEADROOM)
        .ok_or(Error::OutputSizeOverflow)?;
    if scratch.len() < padded_size {
        scratch.resize(padded_size, 0);
    }
    // Sliced to exactly the regenerated length, so the decoders still see a
    // destination of the size they validate against.
    let destination = &mut scratch[..header.regenerated_size];
    if header.four_streams {
        decompress_4x_using_dtable(destination, streams, dtable)?;
    } else {
        decompress_1x_using_dtable(destination, streams, dtable)?;
    }

    Ok((
        payload_end,
        DecodedLiterals::new(scratch, header.regenerated_size),
    ))
}

fn decode_raw_or_rle_size(src: &[u8], size_format: u8) -> Result<(usize, usize)> {
    match size_format {
        0 | 2 => {
            let header0 = *src.first().ok_or(Error::UnexpectedEof)?;
            Ok((1, (header0 >> 3) as usize))
        }
        1 => {
            let header = src.get(..2).ok_or(Error::UnexpectedEof)?;
            let regenerated_size = ((header[0] as usize) >> 4) | ((header[1] as usize) << 4);
            Ok((2, regenerated_size))
        }
        3 => {
            let header = src.get(..3).ok_or(Error::UnexpectedEof)?;
            let regenerated_size = (read_u24_le(header) as usize) >> 4;
            Ok((3, regenerated_size))
        }
        _ => unreachable!(),
    }
}

fn decode_compressed_literals_header(
    src: &[u8],
    size_format: u8,
) -> Result<CompressedLiteralsHeader> {
    let (header_size, regenerated_size, compressed_size, four_streams) = match size_format {
        0 | 1 => {
            let header = src.get(..3).ok_or(Error::UnexpectedEof)?;
            let value = read_u24_le(header) as usize;
            (
                3,
                (value >> 4) & 0x03ff,
                (value >> 14) & 0x03ff,
                size_format == 1,
            )
        }
        2 => {
            let header = src.get(..4).ok_or(Error::UnexpectedEof)?;
            let value = read_u32_le(header) as usize;
            (4, (value >> 4) & 0x3fff, (value >> 18) & 0x3fff, true)
        }
        3 => {
            let header = src.get(..5).ok_or(Error::UnexpectedEof)?;
            let value = (header[0] as u64)
                | ((header[1] as u64) << 8)
                | ((header[2] as u64) << 16)
                | ((header[3] as u64) << 24)
                | ((header[4] as u64) << 32);
            (
                5,
                ((value >> 4) & 0x3ffff) as usize,
                ((value >> 22) & 0x3ffff) as usize,
                true,
            )
        }
        _ => unreachable!(),
    };

    if compressed_size == 0 {
        return Err(Error::Corruption(
            "compressed literals section has an empty payload",
        ));
    }
    if four_streams && compressed_size < 10 {
        return Err(Error::Corruption(
            "4-stream literals section is smaller than the jump-table minimum",
        ));
    }

    Ok(CompressedLiteralsHeader {
        header_size,
        regenerated_size,
        compressed_size,
        four_streams,
    })
}

fn ensure_regenerated_size(regenerated_size: usize, block_size_max: usize) -> Result<()> {
    if regenerated_size > block_size_max {
        return Err(Error::Corruption(
            "literals section exceeds the frame block size limit",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entropy::huff0;

    /// Enough literals, compressed well enough, that the cost model prefers the
    /// double-symbol table.
    ///
    /// Below roughly 3.6 KiB of literals the wider table never repays its build
    /// cost, so a section built from a short body decodes through the
    /// single-symbol path no matter what else the test does. Anything asserting
    /// on double-symbol behavior has to clear that floor or it is asserting on
    /// the other branch.
    fn literal_heavy_bytes(len: usize) -> Vec<u8> {
        // Skewed enough to compress, unpredictable enough that the encoder
        // cannot turn it into matches and drop the literals entirely.
        (0..len as u32)
            .map(
                |index| match (index.wrapping_mul(2_654_435_761) >> 24) % 100 {
                    0..=59 => b' ',
                    60..=79 => b'e',
                    80..=89 => b't',
                    90..=95 => b'a',
                    96..=98 => b'n',
                    _ => (index % 251) as u8,
                },
            )
            .collect()
    }

    /// The shape a literals section decodes through is chosen by a cost model,
    /// and both shapes produce the same bytes, so a round-trip test cannot tell
    /// which one ran. Without this, the double-symbol decoder could stop being
    /// reached at all and every other test would still pass.
    #[test]
    fn a_large_well_compressed_section_decodes_through_the_double_symbol_table() {
        let literals = literal_heavy_bytes(24 * 1024);
        let compressed = huff0::compress(&literals)
            .unwrap()
            .expect("literal-heavy bytes should be Huff0-compressible");

        let mut section = encode_compressed_literals_header(2, 3, literals.len(), compressed.len());
        section.extend_from_slice(&compressed);

        let mut state = LiteralsState::default();
        let (decoded, _) =
            decode_literals_section(&section, &mut state, crate::BLOCK_SIZE_MAX).unwrap();

        assert_eq!(decoded, literals);
        assert!(
            matches!(state.huffman_table, Some(huff0::HuffmanDTable::Double(_))),
            "a {} byte section compressed to {} should select the double-symbol table",
            literals.len(),
            compressed.len()
        );
    }

    /// Why the single-stream branch of the shape choice cannot be observed.
    ///
    /// The decoder skips the cost model for single-stream sections because C
    /// does. That branch is unreachable in practice, and this records why
    /// rather than leaving the guard looking load-bearing: the only size format
    /// that encodes a single stream carries a 10-bit regenerated size, and a
    /// section that small never repays the wider table anyway.
    ///
    /// If the cost model is ever retuned far enough down, this is the test that
    /// says the guard has started to matter.
    #[test]
    fn the_format_caps_a_single_stream_section_below_the_double_symbol_threshold() {
        let largest_single_stream = 0x03ff;
        let header = encode_compressed_literals_header(2, 0, largest_single_stream, 1);
        let parsed = decode_compressed_literals_header(&header, 0).unwrap();
        assert!(!parsed.four_streams);
        assert_eq!(parsed.regenerated_size, largest_single_stream);

        // Best possible ratio, so nothing larger could tip the model either.
        assert!(!huff0::prefers_double_symbol_decoder(
            largest_single_stream,
            1
        ));
    }

    #[test]
    fn parses_compressed_literals_headers() {
        let header0 = encode_compressed_literals_header(2, 0, 511, 700);
        assert_eq!(
            decode_compressed_literals_header(&header0, 0).unwrap(),
            CompressedLiteralsHeader {
                header_size: 3,
                regenerated_size: 511,
                compressed_size: 700,
                four_streams: false,
            }
        );

        let header1 = encode_compressed_literals_header(2, 1, 700, 900);
        assert_eq!(
            decode_compressed_literals_header(&header1, 1).unwrap(),
            CompressedLiteralsHeader {
                header_size: 3,
                regenerated_size: 700,
                compressed_size: 900,
                four_streams: true,
            }
        );

        let header2 = encode_compressed_literals_header(2, 2, 9_000, 12_345);
        assert_eq!(
            decode_compressed_literals_header(&header2, 2).unwrap(),
            CompressedLiteralsHeader {
                header_size: 4,
                regenerated_size: 9_000,
                compressed_size: 12_345,
                four_streams: true,
            }
        );

        let header3 = encode_compressed_literals_header(2, 3, 200_000, 123_456);
        assert_eq!(
            decode_compressed_literals_header(&header3, 3).unwrap(),
            CompressedLiteralsHeader {
                header_size: 5,
                regenerated_size: 200_000,
                compressed_size: 123_456,
                four_streams: true,
            }
        );
    }

    #[test]
    fn treeless_literals_require_a_previous_table() {
        let src = {
            let mut bytes = encode_compressed_literals_header(3, 0, 0, 1);
            bytes.push(1);
            bytes
        };

        let err =
            decode_literals_section(&src, &mut LiteralsState::default(), 128 * 1024).unwrap_err();
        assert_eq!(
            err,
            Error::Corruption("treeless literals require a prior Huff0 table")
        );
    }

    fn encode_compressed_literals_header(
        block_type: u8,
        size_format: u8,
        regenerated_size: usize,
        compressed_size: usize,
    ) -> Vec<u8> {
        let low_bits = u64::from(block_type) | (u64::from(size_format) << 2);
        let (header_size, value) = match size_format {
            0 | 1 => (
                3,
                low_bits | ((regenerated_size as u64) << 4) | ((compressed_size as u64) << 14),
            ),
            2 => (
                4,
                low_bits | ((regenerated_size as u64) << 4) | ((compressed_size as u64) << 18),
            ),
            3 => (
                5,
                low_bits | ((regenerated_size as u64) << 4) | ((compressed_size as u64) << 22),
            ),
            _ => unreachable!(),
        };
        let mut out = vec![0u8; header_size];
        for (index, byte) in out.iter_mut().enumerate() {
            *byte = ((value >> (index * 8)) & 0xff) as u8;
        }
        out
    }
}
