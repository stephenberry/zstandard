//! Output destination for the decoder.
//!
//! The decoder used to write into a `&mut Vec<u8>` everywhere, which left it
//! one shape short of upstream: `ZSTD_decompress` writes into a caller's
//! buffer, and a caller whose destination is an arena, an FFI buffer, an mmap
//! or a stack array had no entry point here. [`DecodeOut`] is the indirection
//! that fixes that.
//!
//! # Why this is not `OutBuf`
//!
//! `OutBuf`, the encoder's sink, can throw writes away. It counts what it
//! wrote, keeps what fits, and reports the overflow afterwards, because the
//! encoder never reads its own output back.
//!
//! The decoder does. Its output buffer *is* its match history: a sequence with
//! offset 40,000 copies from 40,000 bytes back in this very buffer. A sink that
//! dropped a byte would not merely truncate the output, it would corrupt every
//! later match that read through the hole. So every write here either lands or
//! fails, and [`DecodeOut::Fixed`] reports
//! [`Error::DstSizeTooSmall`](crate::Error::DstSizeTooSmall) the moment one
//! would not fit.
//!
//! # Why the enum is not in the hot path's way
//!
//! The sequence executor does not write through this type. It takes the base
//! pointer and the length once per block, runs its wildcopy loop on raw
//! pointers, and hands the length back at the end. The variant is matched a
//! handful of times per block, never per sequence and never per byte.
//!
//! What the fixed variant *does* change in that loop is the trailing slack.
//! `copy_match_inline` overshoots a match by up to `WILDCOPY_OVERLENGTH` bytes,
//! which a `Vec` absorbs as spare capacity and a caller's exact-sized slice
//! does not. [`capacity`](DecodeOut::capacity) is what the executor measures
//! that slack against, once per block; `execute_block_sequences` is
//! monomorphized on the answer and `execute_sequence_exact` is what runs when
//! it has run out.

use crate::error::{Error, Result};

/// Where the decoder writes the bytes it produces.
///
/// `Growable` appends to a caller's `Vec`, which is what every allocating entry
/// point and the streaming decoder use. `Fixed` writes into a borrowed slice
/// and never allocates.
pub(crate) enum DecodeOut<'a> {
    /// A caller-owned `Vec`, grown as needed.
    Growable(&'a mut Vec<u8>),
    /// A caller-owned slice, which cannot grow.
    Fixed(FixedDst<'a>),
}

/// The fixed-capacity half of [`DecodeOut`].
pub(crate) struct FixedDst<'a> {
    buf: &'a mut [u8],
    /// Bytes decoded into `buf`. Never exceeds `buf.len()`, because a write
    /// that would exceed it fails instead.
    len: usize,
}

impl<'a> DecodeOut<'a> {
    /// Wrap a growable buffer. The decoder appends; existing contents are kept
    /// unless the caller clears it.
    pub(crate) fn growable(buf: &'a mut Vec<u8>) -> Self {
        DecodeOut::Growable(buf)
    }

    /// Wrap a fixed-capacity buffer, writing from its start.
    pub(crate) fn fixed(buf: &'a mut [u8]) -> Self {
        DecodeOut::Fixed(FixedDst { buf, len: 0 })
    }

    /// Bytes decoded so far. Every one of them is stored.
    #[inline(always)]
    pub(crate) fn len(&self) -> usize {
        match self {
            DecodeOut::Growable(buf) => buf.len(),
            DecodeOut::Fixed(fixed) => fixed.len,
        }
    }

    /// Bytes that are physically writable through
    /// [`as_mut_ptr`](Self::as_mut_ptr), stored and spare together.
    ///
    /// This is the bound the sequence executor's wildcopy slack is measured
    /// against, which is why it is the physical figure and not
    /// [`len`](Self::len).
    #[inline(always)]
    pub(crate) fn capacity(&self) -> usize {
        match self {
            DecodeOut::Growable(buf) => buf.capacity(),
            DecodeOut::Fixed(fixed) => fixed.buf.len(),
        }
    }

    /// The decoded bytes. Match sources and the frame checksum read through
    /// this.
    #[inline(always)]
    pub(crate) fn as_slice(&self) -> &[u8] {
        match self {
            DecodeOut::Growable(buf) => buf.as_slice(),
            DecodeOut::Fixed(fixed) => &fixed.buf[..fixed.len],
        }
    }

    /// The base of the region as a const pointer, for the match prefetch, which
    /// forms addresses from an offset it has not yet validated.
    #[inline(always)]
    pub(crate) fn as_ptr(&self) -> *const u8 {
        match self {
            DecodeOut::Growable(buf) => buf.as_ptr(),
            DecodeOut::Fixed(fixed) => fixed.buf.as_ptr(),
        }
    }

    /// The base of the writable region, valid for [`capacity`](Self::capacity)
    /// bytes.
    ///
    /// Everything below [`len`](Self::len) is initialized. For `Growable` the
    /// bytes above it are `Vec` spare capacity and are not; for `Fixed` they
    /// are the caller's, and are. Either way they may be written.
    #[inline(always)]
    pub(crate) fn as_mut_ptr(&mut self) -> *mut u8 {
        match self {
            DecodeOut::Growable(buf) => buf.as_mut_ptr(),
            DecodeOut::Fixed(fixed) => fixed.buf.as_mut_ptr(),
        }
    }

    /// Declare `len` bytes decoded.
    ///
    /// # Safety
    ///
    /// `len` must not exceed [`capacity`](Self::capacity), and every byte below
    /// it must have been written.
    #[allow(unsafe_code)]
    #[inline(always)]
    pub(crate) unsafe fn set_len(&mut self, len: usize) {
        match self {
            // SAFETY: the caller's contract is `Vec::set_len`'s contract, with
            // `capacity` forwarded from the same buffer.
            DecodeOut::Growable(buf) => unsafe { buf.set_len(len) },
            DecodeOut::Fixed(fixed) => {
                debug_assert!(len <= fixed.buf.len());
                fixed.len = len;
            }
        }
    }

    /// Whether this destination can grow to fit what it is given.
    ///
    /// Only the decoder's wildcopy invariant asks: a growable destination
    /// reserves the trailing slack per block, so the byte-exact tail path is
    /// unreachable for it, and that is worth asserting rather than believing.
    #[cfg_attr(not(debug_assertions), allow(dead_code))]
    pub(crate) fn is_growable(&self) -> bool {
        matches!(self, DecodeOut::Growable(_))
    }

    /// Discard everything decoded so far, keeping the buffer.
    pub(crate) fn clear(&mut self) {
        match self {
            DecodeOut::Growable(buf) => buf.clear(),
            DecodeOut::Fixed(fixed) => fixed.len = 0,
        }
    }

    /// Hint that `additional` more bytes are coming.
    ///
    /// Advisory: a `Fixed` destination has the capacity the caller gave it and
    /// nothing here can change that, so this is a no-op for it rather than an
    /// error. What a write actually needs is [`ensure_room`](Self::ensure_room).
    /// Reported rather than aborting, because the sizes reaching this are
    /// derived from frame headers the decoder has not yet verified.
    pub(crate) fn try_reserve(&mut self, additional: usize) -> Result<()> {
        match self {
            DecodeOut::Growable(buf) => buf
                .try_reserve(additional)
                .map_err(|_| Error::OutputSizeOverflow),
            DecodeOut::Fixed(_) => Ok(()),
        }
    }

    /// Make `additional` bytes past [`len`](Self::len) writable.
    ///
    /// The growable path allocates, and aborts on allocation failure the way
    /// every other `Vec` write in the decoder does; sizes reaching here are
    /// already bounded by the block size limit rather than by the frame header.
    /// The fixed path cannot grow, so it reports
    /// [`Error::DstSizeTooSmall`].
    #[inline(always)]
    pub(crate) fn ensure_room(&mut self, additional: usize) -> Result<()> {
        match self {
            DecodeOut::Growable(buf) => {
                if buf.capacity() - buf.len() < additional {
                    buf.reserve(additional);
                }
                Ok(())
            }
            DecodeOut::Fixed(fixed) => {
                if fixed.buf.len() - fixed.len < additional {
                    return Err(Error::DstSizeTooSmall);
                }
                Ok(())
            }
        }
    }

    /// Append `bytes`, failing if a `Fixed` destination has no room for them.
    #[allow(unsafe_code)]
    #[inline(always)]
    pub(crate) fn append(&mut self, bytes: &[u8]) -> Result<()> {
        let len = bytes.len();
        if len == 0 {
            return Ok(());
        }
        self.ensure_room(len)?;
        let out_len = self.len();
        // Call platform memcpy directly. LLVM converts ptr::copy_nonoverlapping
        // into an inline 16-byte loop (7 insns/16 bytes), but platform memcpy
        // uses 32-byte stp pairs, prefetching, and non-temporal stores --
        // roughly 2x faster for large copies (e.g. 65KB trailing literals in
        // mixed-entropy). C zstd uses ZSTD_memcpy (== memcpy) here too.
        //
        // SAFETY: `ensure_room` has just guaranteed `len` writable bytes at
        // `out_len`, and `bytes` is a live slice of that length. Neither the
        // literals buffer nor the dictionary a caller can reach here overlaps
        // the output, so the regions are disjoint.
        unsafe {
            unsafe extern "C" {
                // This must be C's `memcpy` exactly, `void*` and all. The
                // standard library relies on this symbol, so the compiler
                // checks any declaration of it against the real one and
                // rejects a mismatch rather than letting the two disagree at
                // link time.
                fn memcpy(
                    dst: *mut core::ffi::c_void,
                    src: *const core::ffi::c_void,
                    len: usize,
                ) -> *mut core::ffi::c_void;
            }
            memcpy(
                self.as_mut_ptr().add(out_len).cast(),
                bytes.as_ptr().cast(),
                len,
            );
            self.set_len(out_len + len);
        }
        Ok(())
    }

    /// Append `count` copies of `byte`, failing if a `Fixed` destination has no
    /// room for them.
    pub(crate) fn append_repeated(&mut self, byte: u8, count: usize) -> Result<()> {
        if count == 0 {
            return Ok(());
        }
        self.ensure_room(count)?;
        let out_len = self.len();
        match self {
            DecodeOut::Growable(buf) => buf.resize(out_len + count, byte),
            DecodeOut::Fixed(fixed) => {
                fixed.buf[out_len..out_len + count].fill(byte);
                fixed.len = out_len + count;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_reports_the_write_that_does_not_fit() {
        let mut backing = [0u8; 4];
        let mut out = DecodeOut::fixed(&mut backing);
        out.append(b"ab").expect("two bytes fit in four");
        assert_eq!(out.len(), 2);

        // Unlike the encoder's sink, nothing is stored partially: a write that
        // straddles the end is refused whole, because a hole in the output is
        // a hole in the match history.
        assert!(matches!(out.append(b"cdef"), Err(Error::DstSizeTooSmall)));
        assert_eq!(out.len(), 2);
        assert_eq!(out.as_slice(), b"ab");

        out.append(b"cd").expect("the two that do fit still fit");
        assert_eq!(out.as_slice(), b"abcd");
    }

    #[test]
    fn a_hint_never_fails_on_a_fixed_destination() {
        // Frame headers declare their content size and the decoder passes that
        // through as a hint. It is unverified at that point, so a fixed
        // destination must not turn a large declaration into an error before
        // the frame has had a chance to contradict it.
        let mut backing = [0u8; 4];
        let mut out = DecodeOut::fixed(&mut backing);
        out.try_reserve(1 << 20).expect("a hint is advisory");
        assert_eq!(out.capacity(), 4);
    }

    #[test]
    fn repeated_bytes_fill_and_then_refuse() {
        let mut backing = [0u8; 4];
        let mut out = DecodeOut::fixed(&mut backing);
        out.append_repeated(b'x', 3).expect("three fit");
        assert_eq!(out.as_slice(), b"xxx");
        assert!(matches!(
            out.append_repeated(b'y', 2),
            Err(Error::DstSizeTooSmall)
        ));
        assert_eq!(out.as_slice(), b"xxx");
    }

    #[test]
    fn growable_tracks_the_vec_it_wraps() {
        let mut backing = Vec::new();
        let mut out = DecodeOut::growable(&mut backing);
        out.append(b"hello").expect("a Vec grows");
        out.append_repeated(b'!', 3).expect("a Vec grows");
        assert_eq!(out.as_slice(), b"hello!!!");
        assert!(out.capacity() >= 8);
        out.clear();
        assert_eq!(out.len(), 0);
        assert_eq!(backing, b"");
    }
}
