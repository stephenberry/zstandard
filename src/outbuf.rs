//! Output sink for the encoder.
//!
//! The encoder used to write into a `&mut Vec<u8>` everywhere, which made the
//! whole public API allocation-owning: a caller with an arena, a fixed FFI
//! buffer, or a `#[no_std]`-adjacent allocation budget had no entry point that
//! wrote where they asked. [`OutBuf`] is the indirection that fixes that
//! without duplicating the encoder — one code path, two destinations.
//!
//! # Why writes are infallible
//!
//! Every write here returns `()`, not `Result`. A fallible sink would put a
//! `?` on several hundred `extend_from_slice` calls in the block, literals, and
//! sequence writers, and each of those would be a new early-return path through
//! code that is currently straight-line.
//!
//! Instead, [`OutBuf::Fixed`] keeps the encoder's *arithmetic* correct past the
//! end of the buffer without storing the bytes. [`OutBuf::len`] counts
//! everything the encoder wrote, not everything that fit, so the block sizes,
//! header positions, and savings figures it computes are the same ones it would
//! compute with room to spare. Two consequences, both wanted:
//!
//! - The encoder makes identical decisions either way, so a fixed-slice encode
//!   into a large enough buffer is byte-for-byte the frame the growable path
//!   produces. `encode_into_slice_matches_encode_all_byte_for_byte` checks it.
//! - Overflow is detectable after the fact rather than mid-write.
//!   [`OutBuf::overflowed`] reports it and the caller gets
//!   [`Error::DstSizeTooSmall`](crate::Error::DstSizeTooSmall) instead of a
//!   length. Nothing partial is ever handed back.
//!
//! Truncating output would be the alternative, and it is worse in the way that
//! matters — a short frame is still a syntactically plausible frame, so a
//! caller who ignored the error would get silent corruption instead of a loud
//! failure.

/// Where the encoder writes its frame.
///
/// `Growable` appends to a caller's `Vec`, which is what every allocating
/// entry point uses. `Fixed` writes into a borrowed slice and never allocates.
pub(crate) enum OutBuf<'a> {
    /// A caller-owned `Vec`, grown as needed.
    Growable(&'a mut Vec<u8>),
    /// A caller-owned slice of fixed capacity.
    Fixed(FixedOut<'a>),
}

/// The fixed-capacity half of [`OutBuf`].
pub(crate) struct FixedOut<'a> {
    buf: &'a mut [u8],
    /// Bytes actually held in `buf`. Never exceeds `buf.len()`.
    stored: usize,
    /// Bytes written past the end of `buf` and discarded. Non-zero means the
    /// encode did not fit and its output must not be used.
    dropped: usize,
}

impl<'a> OutBuf<'a> {
    /// Wrap a growable buffer. The encoder appends; existing contents are kept.
    pub(crate) fn growable(buf: &'a mut Vec<u8>) -> Self {
        OutBuf::Growable(buf)
    }

    /// Wrap a fixed-capacity buffer, writing from its start.
    pub(crate) fn fixed(buf: &'a mut [u8]) -> Self {
        OutBuf::Fixed(FixedOut {
            buf,
            stored: 0,
            dropped: 0,
        })
    }

    /// Bytes the encoder has written, including any that did not fit.
    ///
    /// This is deliberately the *written* count and not the *stored* count.
    /// The encoder subtracts two of these to size a block and records one to
    /// backfill a header, so a value that stopped advancing at the buffer's
    /// end would corrupt that arithmetic and could desynchronize the encode
    /// rather than simply overflow it.
    pub(crate) fn len(&self) -> usize {
        match self {
            OutBuf::Growable(buf) => buf.len(),
            OutBuf::Fixed(fixed) => fixed.stored + fixed.dropped,
        }
    }

    /// Whether any write did not fit. When true the contents are incomplete
    /// and the caller must be given an error rather than a length.
    pub(crate) fn overflowed(&self) -> bool {
        match self {
            OutBuf::Growable(_) => false,
            OutBuf::Fixed(fixed) => fixed.dropped > 0,
        }
    }

    /// Hint that `additional` more bytes are coming. A no-op for `Fixed`,
    /// whose capacity is the caller's to choose.
    pub(crate) fn reserve(&mut self, additional: usize) {
        if let OutBuf::Growable(buf) = self {
            buf.reserve(additional);
        }
    }

    /// Append one byte.
    pub(crate) fn push(&mut self, byte: u8) {
        match self {
            OutBuf::Growable(buf) => buf.push(byte),
            OutBuf::Fixed(fixed) => {
                if fixed.stored < fixed.buf.len() {
                    fixed.buf[fixed.stored] = byte;
                    fixed.stored += 1;
                } else {
                    fixed.dropped += 1;
                }
            }
        }
    }

    /// Append a slice, storing as much of it as fits.
    pub(crate) fn extend_from_slice(&mut self, bytes: &[u8]) {
        match self {
            OutBuf::Growable(buf) => buf.extend_from_slice(bytes),
            OutBuf::Fixed(fixed) => {
                let room = fixed.buf.len() - fixed.stored;
                let fits = room.min(bytes.len());
                fixed.buf[fixed.stored..fixed.stored + fits].copy_from_slice(&bytes[..fits]);
                fixed.stored += fits;
                fixed.dropped += bytes.len() - fits;
            }
        }
    }

    /// Grow the output to `new_len` bytes, filling with `value`.
    ///
    /// Only ever used to open a header placeholder that a later
    /// [`write_at`](Self::write_at) fills in, so it is never asked to shrink.
    pub(crate) fn resize(&mut self, new_len: usize, value: u8) {
        let current = self.len();
        debug_assert!(new_len >= current, "resize is not used to shrink");
        match self {
            OutBuf::Growable(buf) => buf.resize(new_len, value),
            OutBuf::Fixed(_) => {
                for _ in current..new_len {
                    self.push(value);
                }
            }
        }
    }

    /// Discard everything past `new_len`.
    ///
    /// The encoder uses this to abandon a speculative block encode and fall
    /// back to a raw or RLE block. Rolling back below the buffer's capacity
    /// clears the overflow those discarded bytes caused, because they are no
    /// longer part of the output.
    pub(crate) fn truncate(&mut self, new_len: usize) {
        match self {
            OutBuf::Growable(buf) => buf.truncate(new_len),
            OutBuf::Fixed(fixed) => {
                if new_len >= fixed.stored + fixed.dropped {
                    return;
                }
                fixed.stored = new_len.min(fixed.buf.len());
                fixed.dropped = new_len - fixed.stored;
            }
        }
    }

    /// Overwrite `bytes` at `pos`, which must already have been written.
    ///
    /// Used to backfill a block header once the payload it describes has been
    /// encoded and its size is known. A target that landed in the discarded
    /// region is skipped: the encode has already failed and the bytes are
    /// going nowhere, but the write must not panic on the way out.
    pub(crate) fn write_at(&mut self, pos: usize, bytes: &[u8]) {
        let end = pos + bytes.len();
        match self {
            OutBuf::Growable(buf) => buf[pos..end].copy_from_slice(bytes),
            OutBuf::Fixed(fixed) => {
                if end <= fixed.stored {
                    fixed.buf[pos..end].copy_from_slice(bytes);
                }
            }
        }
    }

    /// The bytes actually stored, for the tests below to assert on.
    ///
    /// The encoder itself never reads its own output back, which is what lets
    /// the fixed variant discard overflow instead of having to keep it.
    #[cfg(test)]
    fn as_slice(&self) -> &[u8] {
        match self {
            OutBuf::Growable(buf) => buf.as_slice(),
            OutBuf::Fixed(fixed) => &fixed.buf[..fixed.stored],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_stores_what_fits_and_counts_what_does_not() {
        let mut backing = [0u8; 4];
        let mut out = OutBuf::fixed(&mut backing);
        out.extend_from_slice(b"ab");
        assert_eq!(out.len(), 2);
        assert!(!out.overflowed());

        // Straddles the end: two bytes land, two are counted and dropped.
        out.extend_from_slice(b"cdef");
        assert_eq!(
            out.len(),
            6,
            "len counts every byte written, not just stored"
        );
        assert!(out.overflowed());
        assert_eq!(out.as_slice(), b"abcd");
    }

    #[test]
    fn rolling_back_past_the_overflow_clears_it() {
        // The encoder speculatively encodes a compressed block and truncates
        // back to the header when a raw block would be smaller. A speculative
        // encode that overflowed must not condemn the frame that replaces it.
        let mut backing = [0u8; 4];
        let mut out = OutBuf::fixed(&mut backing);
        out.extend_from_slice(b"abcdefgh");
        assert!(out.overflowed());

        out.truncate(2);
        assert!(
            !out.overflowed(),
            "the discarded bytes are no longer output"
        );
        assert_eq!(out.len(), 2);
        assert_eq!(out.as_slice(), b"ab");

        out.extend_from_slice(b"XY");
        assert_eq!(out.as_slice(), b"abXY");
        assert!(!out.overflowed());
    }

    #[test]
    fn truncating_inside_the_dropped_region_keeps_the_deficit() {
        let mut backing = [0u8; 4];
        let mut out = OutBuf::fixed(&mut backing);
        out.extend_from_slice(b"abcdefgh");

        // Still two bytes over capacity, so still a failed encode.
        out.truncate(6);
        assert_eq!(out.len(), 6);
        assert!(out.overflowed());
        assert_eq!(out.as_slice(), b"abcd");
    }

    #[test]
    fn write_at_backfills_a_header_and_skips_a_dropped_one() {
        let mut backing = [0u8; 4];
        let mut out = OutBuf::fixed(&mut backing);
        out.resize(3, 0);
        out.extend_from_slice(b"z");
        out.write_at(0, b"hdr");
        assert_eq!(out.as_slice(), b"hdrz");

        // A header position past the stored region is skipped rather than
        // panicking; the encode has already overflowed.
        out.extend_from_slice(b"payload");
        let past_the_end = 6;
        out.write_at(past_the_end, b"hdr");
        assert!(out.overflowed());
    }

    #[test]
    fn growable_matches_vec_semantics() {
        let mut backing = Vec::new();
        let mut out = OutBuf::growable(&mut backing);
        out.resize(3, 0);
        out.extend_from_slice(b"payload");
        out.write_at(0, b"hdr");
        out.truncate(5);
        assert!(!out.overflowed());
        assert_eq!(out.as_slice(), b"hdrpa");
    }
}
