//! Write the seed corpus the fuzz targets start from, into `fuzz/seeds`.
//!
//! Run it before `cargo fuzz run`, and pass the directory it fills as an extra
//! read-only corpus:
//!
//! ```text
//! cargo run --release --example fuzz_seeds
//! cargo fuzz run full_decode fuzz/corpus/full_decode fuzz/seeds/frames
//! ```
//!
//! The seeds are generated rather than checked in so they stay in step with
//! whatever this crate currently emits, and so the repository does not carry
//! several hundred small binary files.
//!
//! Two things decide what goes in here. The decode targets need *valid frames*:
//! a fuzzer starting from random bytes will not assemble a legal header, let
//! alone legal entropy tables, so without seeds it spends its whole run at the
//! parser's front door. The encode targets need bodies with different match
//! structure, since that is what steers the parsers.
//!
//! Every seed stays small. libFuzzer takes its `-max_len` from the longest input
//! it has seen, so one large seed slows every later iteration — and the encode
//! targets can grow a body themselves when the fuzzer sets the amplify bit.

use std::{fs, path::Path};

use zstandard::{
    CompressionLevel, EncoderOptions, encode_all_with_dict_and_options, encode_all_with_options,
};

/// Bodies with distinct match structure. Between them they reach the raw, RLE,
/// and compressed block decisions, and every literal encoding.
fn bodies() -> Vec<(&'static str, Vec<u8>)> {
    let mut out: Vec<(&'static str, Vec<u8>)> = vec![
        ("empty", Vec::new()),
        ("short", b"hello hello hello".to_vec()),
        // One repeated byte: the RLE block decision.
        ("zeros", vec![0u8; 1500]),
        ("ascending", (0..2048u32).map(|i| i as u8).collect()),
    ];

    let mut json = Vec::new();
    let mut index = 0u64;
    while json.len() < 1_400 {
        json.extend_from_slice(
            format!(
                "{{\"ts\":{index},\"svc\":\"{}\",\"ok\":{},\"id\":\"{:08x}\"}}\n",
                ["api", "billing", "search"][(index % 3) as usize],
                !index.is_multiple_of(7),
                index.wrapping_mul(2_654_435_761) as u32
            )
            .as_bytes(),
        );
        index += 1;
    }
    out.push(("json", json));

    let mut csv = Vec::new();
    let mut row = 0u64;
    while csv.len() < 1_200 {
        csv.extend_from_slice(
            format!("{row},{},{},{}\n", row * 3 % 977, row % 5, row * 7 % 61).as_bytes(),
        );
        row += 1;
    }
    out.push(("csv", csv));

    // Incompressible: the raw block decision, and high-entropy literals.
    let mut state = 0x2545_F491_4F6C_DD1Du64;
    let random: Vec<u8> = (0..1024)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state as u8
        })
        .collect();
    out.push(("random", random));

    // A three-byte period broken every few bytes, so matches are everywhere but
    // none of them runs long. This is the shape that fills a sequence plan to
    // its per-block bound; evenly repeating data settles on one repeat offset
    // and emits almost nothing.
    let mut dense = Vec::new();
    let mut tile = 0u32;
    while dense.len() < 1_500 {
        let start = dense.len();
        dense.extend_from_slice(b"\x23\x91\xb0");
        dense[start + (tile as usize * 7) % 3] ^= (tile as u8) | 1;
        tile = tile.wrapping_add(1);
    }
    out.push(("dense", dense));

    // An unbroken five-byte period: one long match at an offset under 8, which
    // the sequence executor expands from a tiled stack buffer rather than by
    // copying history. Nothing else here reaches that expander with a match
    // long enough to stamp the buffer more than once -- `zeros` is offset 1,
    // `short` is too short, and `dense` breaks its period before any match
    // runs. Five neither divides the buffer nor the vector width, so a stamp
    // that loses the period's phase shows up in the output.
    out.push(("period5", b"abcde".repeat(120)));

    out
}

fn write(dir: &Path, name: &str, bytes: &[u8]) {
    fs::create_dir_all(dir).expect("seed directory");
    fs::write(dir.join(name), bytes).expect("seed file");
}

/// The frame-flags byte with long-distance matching switched on and its four
/// parameters left to derive. Bits 0 and 1 are the two frame-format switches, so
/// the long-distance field starts at bit 2.
const LDM_ON: u8 = 1 << 2;

/// The same, with all four long-distance parameters overridden as well.
const LDM_ALL: u8 = 0b11111 << 2;

fn main() {
    let root = Path::new("fuzz/seeds");
    let _ = fs::remove_dir_all(root);
    let bodies = bodies();

    // Encode targets read configuration bytes before the body, so a seed is a
    // control prefix plus one of the bodies above. The prefixes below reach
    // every parser family, both amplified and not, several block sizes, both
    // frame formats, and long-distance matching; see `src/fuzz.rs` for how each
    // byte is read.
    //
    // The layout is `[level, block size, flags, override mask, value, value,
    // frame flags]`, with one more byte on the two targets that need it. An
    // override mask of `0` is deliberately the common case: a level's own
    // parameters are the ones the encoder is tuned for, and the interesting
    // mutations are a step or two off them rather than a uniformly random set.
    for (target, controls) in [
        (
            "encode_roundtrip",
            vec![
                vec![0u8, 0, 0, 0, 0, 0, 0],
                vec![0, 0, 4, 0, 0, 0, 0],
                vec![3, 5, 1, 0, 0, 0, 0],
                vec![6, 2, 3, 0, 0, 0, 0],
                vec![15, 7, 0, 0, 0, 0, 0],
                vec![21, 4, 1, 0, 0, 0, 0],
                // A narrow window, a chosen strategy, and every parameter at
                // once, plus the two frame-format switches.
                vec![6, 2, 3, 0x01, 0x05, 0x00, 0],
                vec![3, 5, 1, 0x40, 0x03, 0x07, 0],
                vec![15, 7, 0, 0x7f, 0x11, 0x2b, 0],
                vec![6, 2, 3, 0, 0, 0, 1],
                vec![6, 2, 3, 0, 0, 0, 2],
                vec![21, 4, 1, 0x7f, 0x40, 0x09, 3],
                // Long-distance matching, alone and with all four of its own
                // parameters overridden. The second pairs it with an amplified
                // body of 128 KiB: the matcher's minimum match is 64 bytes by
                // default, so on a body of a couple of kilobytes it resolves
                // its tables and finds nothing.
                vec![6, 2, 3, 0, 0, 0, LDM_ON],
                vec![15, 7, 0x2c, 0, 0x11, 0x2b, LDM_ALL],
            ],
        ),
        (
            "streaming_encode_roundtrip",
            vec![
                vec![0u8, 0, 0, 0, 0, 0, 0, 0x03],
                vec![0, 2, 1, 0, 0, 0, 0, 0x24],
                vec![6, 0, 0, 0, 0, 0, 0, 0x41],
                vec![15, 4, 1, 0, 0, 0, 0, 0x0f],
                vec![21, 1, 0, 0, 0, 0, 0, 0x1f],
                vec![6, 0, 0, 0x40, 0x05, 0x02, 0, 0x41],
                vec![15, 4, 1, 0x7f, 0x11, 0x2b, 2, 0x0f],
                // The long-distance table is the one piece of frame state with
                // no rebuild to fall back on when the history buffer compacts,
                // so the amplified body matters more here than anywhere: a body
                // under the window never compacts at all.
                vec![6, 0, 0x2c, 0, 0, 0, LDM_ON, 0x41],
                vec![15, 4, 0x2c, 0, 0x11, 0x2b, LDM_ALL, 0x0f],
            ],
        ),
        (
            "dictionary_encode_roundtrip",
            vec![
                vec![0u8, 0, 0, 0, 0, 0, 0, 32],
                vec![3, 2, 1, 0, 0, 0, 0, 96],
                vec![6, 4, 0, 0, 0, 0, 0, 8],
                vec![15, 0, 1, 0, 0, 0, 0, 200],
                vec![21, 3, 0, 0, 0, 0, 0, 64],
                vec![6, 4, 0, 0x40, 0x03, 0x06, 0, 8],
                vec![15, 0, 1, 0x7f, 0x11, 0x2b, 1, 200],
                // Long-distance matching against a dictionary is refused rather
                // than encoded without it, and the split byte decides which side
                // of that boundary the seed lands on: 0 leaves the dictionary
                // empty, which has nothing to conflict with and is accepted.
                vec![6, 4, 0, 0, 0, 0, LDM_ON, 96],
                vec![6, 4, 0, 0, 0, 0, LDM_ON, 0],
            ],
        ),
    ] {
        let dir = root.join(target);
        for (index, control) in controls.iter().enumerate() {
            for (name, body) in &bodies {
                let mut seed = control.clone();
                seed.extend_from_slice(body);
                write(&dir, &format!("{name}-{index}"), &seed);
            }
        }
    }

    // Seeds for `dictionary_encode_roundtrip` that reach the dictionary
    // boundary, which none of the bodies above does.
    //
    // The target splits its input into a dictionary and a body. A match that
    // starts inside the dictionary and runs on into the frame takes a split
    // path -- part copied from the dictionary, the rest expanded from the
    // frame's own output at an offset of everything produced so far -- and
    // reaching it needs the body to open on a match a few bytes back into the
    // dictionary's tail. So the dictionary ends in a short period and the body
    // opens with that same period. A 2026-08 defect in that expander survived
    // five days of fuzzing because nothing set up that pairing; it fails this
    // target's round trip immediately once something does.
    //
    // The split byte is 128 and the two halves are equal, so the dictionary
    // ends exactly where the period begins.
    {
        let dir = root.join("dictionary_encode_roundtrip");
        for period in 2..=7usize {
            let unit: Vec<u8> = (0..period).map(|index| b'a' + index as u8).collect();
            let half = 300;

            let mut dictionary: Vec<u8> = (0..half as u32).map(|i| (i * 37 % 251) as u8).collect();
            dictionary.truncate(half - period);
            dictionary.extend_from_slice(&unit);

            let body: Vec<u8> = unit.iter().copied().cycle().take(half).collect();

            for level in [3u8, 6, 12] {
                // Level, default block size, checksum on with the body taken as
                // a seed rather than tiled, no parameter overrides, and a
                // half-and-half dictionary split.
                let mut seed = vec![level, 0, 1, 0, 0, 0, 0, 128];
                seed.extend_from_slice(&dictionary);
                seed.extend_from_slice(&body);
                write(&dir, &format!("boundary-p{period}-l{level}"), &seed);
            }
        }
    }

    // Frames for the decode targets. One directory shared by all four that read
    // a whole frame, since libFuzzer accepts any number of corpus directories.
    let dictionary: Vec<u8> = (0..512u32).map(|i| (i * 37 % 251) as u8).collect();
    let mut frames: Vec<(String, Vec<u8>)> = Vec::new();
    for (name, body) in &bodies {
        for level in [1i32, 3, 6, 12, 19] {
            let options = EncoderOptions {
                // Small enough at the higher levels to give multi-block frames
                // without large bodies.
                block_size: if level > 9 { 4096 } else { 1 << 14 },
                checksum: level == 3,
                write_dict_id: true,
                compression_level: CompressionLevel::try_new(level).expect("level in range"),
                ..Default::default()
            };
            frames.push((
                format!("{name}-l{level}"),
                encode_all_with_options(body, options).expect("encode"),
            ));
            if level == 6 {
                frames.push((
                    format!("{name}-dict"),
                    encode_all_with_dict_and_options(body, &dictionary, options).expect("encode"),
                ));
            }
        }
    }

    let dir = root.join("frames");
    for (name, frame) in &frames {
        write(&dir, name, frame);
    }

    // The dictionary decode target reads a two-byte dictionary length, then
    // takes the dictionary and the frame from what follows. Only the frames
    // encoded against that dictionary are worth seeding: a frame with no
    // dictionary-reaching match leaves the boundary split unexercised, which is
    // the whole point of the target.
    let dir = root.join("dictionary_frames");
    for (name, frame) in &frames {
        if !name.ends_with("-dict") {
            continue;
        }
        let mut seed = (dictionary.len() as u16).to_le_bytes().to_vec();
        seed.extend_from_slice(&dictionary);
        seed.extend_from_slice(frame);
        write(&dir, name, &seed);
    }

    // The streaming decode target reads a chunk-size selector first.
    let dir = root.join("chunked_frames");
    for (name, frame) in &frames {
        for selector in [0u8, 4, 8] {
            let mut seed = vec![selector];
            seed.extend_from_slice(frame);
            write(&dir, &format!("{name}-c{selector}"), &seed);
        }
    }

    let (mut files, mut bytes, mut longest) = (0usize, 0u64, 0u64);
    for group in fs::read_dir(root).expect("seed root") {
        for seed in fs::read_dir(group.expect("group").path()).expect("group contents") {
            let len = seed.expect("seed").metadata().expect("metadata").len();
            files += 1;
            bytes += len;
            longest = longest.max(len);
        }
    }
    println!("wrote {files} seeds, {bytes} bytes, longest {longest}");
}
