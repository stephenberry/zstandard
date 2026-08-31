//! How large are the public types when held by value?
//!
//! Entropy coding needs tables, and a table written as an inline array costs
//! nothing to reach but has to be carried by whatever owns it. That is invisible
//! until a caller tries to *embed* one of these types: put a 25 KB struct in an
//! enum and `clippy::large_enum_variant` fires, hold one across an `await` and
//! the future grows by 25 KB. A C implementation hands back a pointer to a
//! context and none of this arises.
//!
//! So the sizes are documented on the types themselves, and pinned here. The
//! bounds are loose because an exact equality would fail on any unrelated field
//! addition and on 32-bit targets, where every one of these types shrinks by
//! some amount that depends on how many pointers it holds. What each bound is
//! really asserting is a *claim made in a doc comment*, so a failure here means
//! documentation now lies and needs editing, not that a number drifted.

use zstandard::io::{Reader, Writer};
use zstandard::{Decoder, Encoder, StreamingDecoder, StreamingEncoder};

/// The decode side is documented as small enough to hold by value anywhere.
///
/// `StreamingDecoder` was 35,520 bytes until `FrameDecodeState` was boxed, which
/// made every `io::Reader` wrapping one just as large. If that boxing were
/// undone this would fail by two orders of magnitude.
#[test]
fn the_decode_types_are_small_enough_to_embed_freely() {
    const SMALL: usize = 1024;

    for (name, size) in [
        ("StreamingDecoder", size_of::<StreamingDecoder<'static>>()),
        ("io::Reader", size_of::<Reader<'static, &[u8]>>()),
        ("Decoder", size_of::<Decoder>()),
    ] {
        assert!(
            size <= SMALL,
            "{name} is {size} bytes; it is documented as small enough to hold by \
             value, which for the streaming types depends on the frame's decode \
             tables staying boxed",
        );
    }
}

/// The encode side is documented as large, and callers are told to box it.
///
/// The weight is the Huffman compression workspace and the literals encoding
/// state. Unlike the decoder's per-frame tables this is long-lived reuse state
/// read on the per-block path, so boxing it would trade against the hot path
/// rather than being close to free, and it has not been done.
///
/// Two-sided on purpose. The upper bound catches growth. The lower bound catches
/// the encoder becoming *small*, which would be good news and would also mean
/// every doc comment telling callers to box it is now wrong.
#[test]
fn the_encode_types_are_large_and_documented_as_such() {
    // Each row carries the figure its own documentation quotes, in KB; `Encoder`
    // is the scratch alone and the streaming types add a frame's worth of state
    // on top, so one shared pair of bounds would have to be loose enough to be
    // worthless. Generous either side of the documented figure, because the
    // point is to catch a claim going stale, not to track the exact number.
    for (name, size, documented_kb) in [
        (
            "StreamingEncoder",
            size_of::<StreamingEncoder<'static>>(),
            25,
        ),
        ("io::Writer", size_of::<Writer<'static, Vec<u8>>>(), 25),
        ("Encoder", size_of::<Encoder>(), 19),
    ] {
        let documented = documented_kb * 1024;
        assert!(
            size < documented * 2,
            "{name} is {size} bytes, well past the ~{documented_kb} KB its \
             documentation quotes; update the doc comment or find out what was \
             added",
        );
        assert!(
            size > documented / 2,
            "{name} is down to {size} bytes from the ~{documented_kb} KB its \
             documentation quotes; if it no longer needs boxing to embed, delete \
             the footprint notes telling callers otherwise",
        );
    }
}
