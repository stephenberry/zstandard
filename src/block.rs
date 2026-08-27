use crate::{
    error::{Error, Result},
    frame::read_u24_le,
    outbuf::OutBuf,
};

/// Largest payload size, in bytes, that a single Zstandard block may carry.
pub const BLOCK_SIZE_MAX: usize = 128 * 1024;
/// Encoded size of a block header in bytes (three).
pub const BLOCK_HEADER_SIZE: usize = 3;

/// Encoding type of a Zstandard block payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockType {
    /// Block payload is stored uncompressed.
    Raw,
    /// Block payload is a single byte that expands to `block_size` repetitions.
    Rle,
    /// Block payload is a Huff0/FSE-compressed sequence of literals and matches.
    Compressed,
}

/// Parsed three-byte header that introduces every block inside a Zstandard frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockHeader {
    /// `true` if this is the last block of the current frame.
    pub last_block: bool,
    /// Encoding type of the following payload.
    pub block_type: BlockType,
    /// For `Raw` and `Compressed` blocks the payload byte count; for `Rle` the run length.
    pub block_size: u32,
}

impl BlockHeader {
    /// Encoded size of a block header in bytes.
    pub const SIZE: usize = BLOCK_HEADER_SIZE;

    /// Parse a block header from the start of `src`. Returns
    /// [`Error::UnexpectedEof`](crate::Error::UnexpectedEof) if fewer than
    /// [`Self::SIZE`] bytes are available, or [`Error::Corruption`](crate::Error::Corruption)
    /// for the reserved block-type code.
    pub fn parse(src: &[u8]) -> Result<Self> {
        if src.len() < BLOCK_HEADER_SIZE {
            return Err(Error::UnexpectedEof);
        }

        let value = read_u24_le(&src[..BLOCK_HEADER_SIZE]);
        let last_block = (value & 1) != 0;
        let block_type = match (value >> 1) & 0x3 {
            0 => BlockType::Raw,
            1 => BlockType::Rle,
            2 => BlockType::Compressed,
            _ => return Err(Error::Corruption("reserved block type")),
        };
        let block_size = value >> 3;

        Ok(Self {
            last_block,
            block_type,
            block_size,
        })
    }

    fn encode(self) -> [u8; 3] {
        let block_type_bits = match self.block_type {
            BlockType::Raw => 0u32,
            BlockType::Rle => 1u32,
            BlockType::Compressed => 2u32,
        };
        let value = u32::from(self.last_block) | (block_type_bits << 1) | (self.block_size << 3);
        [
            (value & 0xFF) as u8,
            ((value >> 8) & 0xFF) as u8,
            ((value >> 16) & 0xFF) as u8,
        ]
    }

    pub(crate) fn write_to(self, dst: &mut OutBuf<'_>) {
        dst.extend_from_slice(&self.encode());
    }

    /// Write the block header at a specific position (for backfilling placeholders).
    ///
    /// The encoder reserves three bytes, encodes the payload, then comes back
    /// here with the size it turned out to be.
    pub(crate) fn write_at(self, dst: &mut OutBuf<'_>, pos: usize) {
        dst.write_at(pos, &self.encode());
    }

    pub(crate) fn payload_size(self) -> usize {
        match self.block_type {
            BlockType::Raw | BlockType::Compressed => self.block_size as usize,
            BlockType::Rle => 1,
        }
    }
}

/// Convenience wrapper for [`BlockHeader::parse`].
pub fn parse_block_header(src: &[u8]) -> Result<BlockHeader> {
    BlockHeader::parse(src)
}
