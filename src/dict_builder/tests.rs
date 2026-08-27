use super::*;
use crate::{
    DecoderDictionary, decode_all_with_prepared_dict, encode_all, encode_all_with_prepared_dict,
};

/// Records that share structure without repeating verbatim.
///
/// A corpus of identical records would be compressed almost as well by any
/// dictionary, or none, and would not distinguish a working trainer from a
/// broken one. The varying field widths and interleaved values are what make
/// the shared skeleton worth learning while leaving real work for the parser.
fn sample_records(count: usize) -> Vec<Vec<u8>> {
    let hosts = ["alpha", "beta", "gamma-node", "delta", "epsilon-7"];
    let levels = ["INFO", "WARN", "ERROR", "DEBUG"];
    (0..count)
        .map(|index| {
            let mut record = Vec::new();
            for line in 0..12 {
                let seed = index * 31 + line * 7;
                record.extend_from_slice(
                    &format!(
                        "{{\"timestamp\":\"2026-08-{:02}T{:02}:{:02}:{:02}Z\",\
                         \"host\":\"{}\",\"level\":\"{}\",\"request_id\":{},\
                         \"duration_ms\":{},\"message\":\"request completed for tenant {}\"}}\n",
                        (seed % 28) + 1,
                        seed % 24,
                        (seed * 13) % 60,
                        (seed * 17) % 60,
                        hosts[seed % hosts.len()],
                        levels[(seed / 3) % levels.len()],
                        1_000_000 + seed * 991,
                        (seed * 7919) % 4096,
                        seed % 97,
                    )
                    .into_bytes(),
                );
            }
            record
        })
        .collect()
}

fn as_slices(samples: &[Vec<u8>]) -> Vec<&[u8]> {
    samples.iter().map(Vec::as_slice).collect()
}

#[test]
fn trains_a_dictionary_that_round_trips() {
    let samples = sample_records(40);
    let dictionary = train_dictionary(&as_slices(&samples), 4096).expect("training");

    let prepared = EncoderDictionary::new(&dictionary).expect("parse trained dictionary");
    let prepared_decoding =
        DecoderDictionary::new(&dictionary).expect("parse trained dictionary for decoding");
    assert!(
        !prepared.is_raw_content(),
        "a trained dictionary carries entropy tables and must parse as formatted"
    );

    for sample in &samples {
        let compressed = encode_all_with_prepared_dict(sample, &prepared).expect("encode");
        let decoded =
            decode_all_with_prepared_dict(&compressed, &prepared_decoding).expect("decode");
        assert_eq!(&decoded, sample);
    }
}

#[test]
fn trained_dictionary_beats_no_dictionary_on_short_records() {
    let samples = sample_records(40);
    let dictionary = train_dictionary(&as_slices(&samples), 4096).expect("training");
    let prepared = EncoderDictionary::new(&dictionary).expect("parse");

    // Measure on records the trainer did not see, so this reports transfer
    // rather than how well the dictionary memorized its own corpus.
    let held_out = sample_records(60);
    let held_out = &held_out[40..];

    let mut with_dictionary = 0usize;
    let mut without = 0usize;
    for sample in held_out {
        // One record at a time is the case dictionaries exist for: alone, a
        // short record has no history to match against.
        let record = sample.split(|&b| b == b'\n').next().expect("a record");
        with_dictionary += encode_all_with_prepared_dict(record, &prepared)
            .expect("encode")
            .len();
        without += encode_all(record).expect("encode").len();
    }

    assert!(
        with_dictionary * 2 < without,
        "a trained dictionary should less than halve short records: \
         {with_dictionary} with, {without} without"
    );
}

#[test]
fn the_dictionary_id_is_derived_from_the_content() {
    let samples = sample_records(40);
    let dictionary = train_dictionary(&as_slices(&samples), 4096).expect("training");
    let id = EncoderDictionary::new(&dictionary).expect("parse").id();
    // Upstream folds the content hash into the range reserved for dictionaries
    // that were never registered with a central authority.
    assert!(
        (32768..(1u32 << 31)).contains(&id),
        "derived id {id} is outside the non-registered range"
    );

    // The same samples must produce the same dictionary, id included. Training
    // has no clock and no randomness, and a caller who retrains on unchanged
    // input should not see the id move under them.
    let again = train_dictionary(&as_slices(&samples), 4096).expect("training");
    assert_eq!(dictionary, again, "training is not deterministic");
}

#[test]
fn an_explicit_dictionary_id_is_recorded_verbatim() {
    let samples = sample_records(40);
    let trained = train_dictionary_with_parameters(
        &as_slices(&samples),
        4096,
        DictionaryTrainingParameters {
            d: 8,
            steps: 4,
            dictionary_id: 12345,
            ..DictionaryTrainingParameters::default()
        },
    )
    .expect("training");
    assert_eq!(
        EncoderDictionary::new(trained.as_bytes())
            .expect("parse")
            .id(),
        12345
    );
}

#[test]
fn the_dictionary_never_exceeds_the_requested_capacity() {
    let samples = sample_records(40);
    for capacity in [DICTIONARY_SIZE_MIN, 512, 1024, 4096, 16384] {
        let dictionary = train_dictionary(&as_slices(&samples), capacity)
            .unwrap_or_else(|error| panic!("training at capacity {capacity}: {error:?}"));
        assert!(
            dictionary.len() <= capacity,
            "capacity {capacity} produced {} bytes",
            dictionary.len()
        );
        EncoderDictionary::new(&dictionary)
            .unwrap_or_else(|error| panic!("capacity {capacity} produced junk: {error:?}"));
    }
}

#[test]
fn a_capacity_below_the_minimum_is_rejected() {
    let samples = sample_records(40);
    assert!(matches!(
        train_dictionary(&as_slices(&samples), DICTIONARY_SIZE_MIN - 1),
        Err(Error::DstSizeTooSmall)
    ));
}

#[test]
fn too_few_samples_are_rejected_rather_than_producing_a_weak_dictionary() {
    // Four training samples cannot support a frequency table, and upstream
    // refuses rather than training on them.
    let samples = sample_records(5);
    assert!(matches!(
        train_dictionary(&as_slices(&samples), 4096),
        Err(Error::InvalidParameter(_))
    ));

    assert!(matches!(
        train_dictionary(&[], 4096),
        Err(Error::InvalidParameter(_))
    ));
}

#[test]
fn out_of_range_parameters_are_rejected() {
    let samples = sample_records(40);
    let samples = as_slices(&samples);
    let cases = [
        DictionaryTrainingParameters {
            d: 7,
            ..Default::default()
        },
        DictionaryTrainingParameters {
            f: 32,
            ..Default::default()
        },
        DictionaryTrainingParameters {
            accel: 11,
            ..Default::default()
        },
        DictionaryTrainingParameters {
            split_point: 1.5,
            ..Default::default()
        },
    ];
    for parameters in cases {
        assert!(
            matches!(
                train_dictionary_with_parameters(&samples, 4096, parameters),
                Err(Error::InvalidParameter(_))
            ),
            "expected rejection for {parameters:?}"
        );
    }
}

#[test]
fn a_capacity_below_the_default_search_range_still_trains() {
    // The default search runs `k` up to 2000. A 256-byte dictionary cannot use
    // most of those, and upstream skips them rather than failing, so a caller
    // asking for a small dictionary must still get one.
    let samples = sample_records(40);
    let dictionary = train_dictionary(&as_slices(&samples), DICTIONARY_SIZE_MIN).expect("training");
    assert!(dictionary.len() <= DICTIONARY_SIZE_MIN);
    EncoderDictionary::new(&dictionary).expect("parse");
}

#[test]
fn both_substring_lengths_are_searched_when_d_is_unset() {
    let samples = sample_records(40);
    let trained = train_dictionary_with_parameters(
        &as_slices(&samples),
        4096,
        DictionaryTrainingParameters {
            steps: 2,
            ..Default::default()
        },
    )
    .expect("training");
    assert!(
        trained.d() == 6 || trained.d() == 8,
        "search settled on d={}",
        trained.d()
    );
    assert!(trained.total_compressed_size() > 0);
}

#[test]
fn incompressible_samples_still_produce_a_usable_dictionary() {
    // Noise has no shared structure to learn, and its literal histogram is the
    // pathological case that forces the flat-literal substitution. The trainer
    // must still emit a dictionary that parses and round-trips rather than
    // failing or emitting tables nothing can encode with.
    let mut state = 0x243f_6a88_85a3_08d3u64;
    let samples: Vec<Vec<u8>> = (0..16)
        .map(|_| {
            (0..2048)
                .map(|_| {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    (state >> 33) as u8
                })
                .collect()
        })
        .collect();

    let dictionary = train_dictionary(&as_slices(&samples), 1024).expect("training");
    let prepared = EncoderDictionary::new(&dictionary).expect("parse");
    let prepared_decoding = DecoderDictionary::new(&dictionary).expect("parse for decoding");
    let compressed = encode_all_with_prepared_dict(&samples[0], &prepared).expect("encode");
    let decoded = decode_all_with_prepared_dict(&compressed, &prepared_decoding).expect("decode");
    assert_eq!(decoded, samples[0]);
}

#[test]
fn content_selection_is_independent_of_the_entropy_half() {
    // The two halves of training fail differently, and this pins the first one
    // on its own: the same samples and parameters must select the same content
    // bytes, whatever the encoder does afterwards. A change here is a change in
    // the algorithm, not a change in our parser's statistics.
    let samples = sample_records(40);
    let corpus = Corpus::new(&as_slices(&samples));
    let context = FrequencyContext::build(&corpus, 8, 20, 0.75, 0).expect("frequencies");

    let select = |content: &mut Vec<u8>| {
        let mut freqs = context.freqs.clone();
        let mut segment_freqs = vec![0u32; 1 << 20];
        context.build_content(&mut freqs, &mut segment_freqs, content, 200)
    };

    let mut content = vec![0u8; 1024];
    let tail = select(&mut content);
    assert!(tail < content.len(), "no content was selected");

    // The content is a run of segments packed back to front, so a window taken
    // anywhere in it may straddle two of them. The last bytes of the buffer are
    // the exception: they hold the first segment selected, which the builder
    // guarantees is at least `d` bytes, so they are one contiguous run of
    // corpus bytes and must appear there verbatim. A hashing or bounds error
    // would show up here as content that came from nowhere.
    let probe = &content[content.len() - context.d as usize..];
    assert!(
        corpus
            .bytes
            .windows(probe.len())
            .any(|window| window == probe),
        "selected content does not appear in the corpus"
    );

    // Selection consumes frequencies as it goes, so it is only reproducible if
    // that state is rebuilt rather than carried over. Running it twice from the
    // same context is what catches a leak between candidates.
    let mut again = vec![0u8; 1024];
    assert_eq!(select(&mut again), tail);
    assert_eq!(content, again, "content selection is not reproducible");
}

#[test]
fn degenerate_sample_shapes_are_rejected_rather_than_read_past() {
    // The `d`-mer count is taken over the training portion only, so a corpus
    // that is large in total can still leave the training split with nothing to
    // count. Upstream checks only the total and lets the subtraction wrap.
    let mut samples: Vec<Vec<u8>> = vec![Vec::new(); 6];
    samples.push(vec![b'a'; 100_000]);
    samples.push(vec![b'b'; 100_000]);
    assert!(matches!(
        train_dictionary(&as_slices(&samples), 4096),
        Err(Error::InvalidParameter(_))
    ));

    // Samples shorter than the eight bytes each hash reads, in a corpus whose
    // total clears that bar.
    let tiny: Vec<Vec<u8>> = (0..12).map(|i| vec![b'x'; i % 7]).collect();
    match train_dictionary(&as_slices(&tiny), 4096) {
        Ok(dictionary) => {
            EncoderDictionary::new(&dictionary).expect("parse");
        }
        Err(Error::InvalidParameter(_) | Error::SrcSizeWrong | Error::DstSizeTooSmall) => {}
        Err(other) => panic!("unexpected error for tiny samples: {other:?}"),
    }

    // Interleaved empty samples inside an otherwise healthy corpus must not
    // shift any boundary: the offsets are what the epoch arithmetic indexes by.
    let mut mixed = Vec::new();
    for (index, record) in sample_records(40).into_iter().enumerate() {
        if index % 4 == 0 {
            mixed.push(Vec::new());
        }
        mixed.push(record);
    }
    let dictionary = train_dictionary(&as_slices(&mixed), 4096).expect("training");
    EncoderDictionary::new(&dictionary).expect("parse");
}

#[test]
fn a_segment_size_beyond_a_sixteen_bit_counter_does_not_overflow() {
    // Upstream counts window occurrences in a `u16`. Its own search never asks
    // for a segment longer than 2000, but this API takes `k` from the caller,
    // and a window of more than 65535 positions over repetitive data would
    // exceed that counter: silent corruption in C, a panic here.
    let samples: Vec<Vec<u8>> = (0..8).map(|_| vec![b'a'; 40_000]).collect();
    let trained = train_dictionary_with_parameters(
        &as_slices(&samples),
        200_000,
        DictionaryTrainingParameters {
            k: 100_000,
            d: 8,
            steps: 1,
            ..Default::default()
        },
    )
    .expect("training");
    EncoderDictionary::new(trained.as_bytes()).expect("parse");
}

#[test]
fn an_extreme_segment_size_terminates_instead_of_wrapping() {
    let samples = sample_records(40);

    // The search steps `k` until it passes `max_k`. At `u32::MAX` that
    // condition can never go false, so an unguarded increment overflows: a
    // panic in debug, and in release a wrap to zero that restarts the sweep and
    // never ends. No candidate is usable at this size, so the call should
    // simply report that.
    assert!(matches!(
        train_dictionary_with_parameters(
            &as_slices(&samples),
            4096,
            DictionaryTrainingParameters {
                k: u32::MAX,
                d: 8,
                steps: 1,
                ..Default::default()
            },
        ),
        Err(Error::InvalidParameter(_))
    ));

    // Epoch sizing multiplies `k` by ten, which also leaves 32 bits well before
    // `k` reaches its own maximum.
    assert!(
        train_dictionary_with_parameters(
            &as_slices(&samples),
            4096,
            DictionaryTrainingParameters {
                k: 500_000_000,
                d: 8,
                steps: 1,
                ..Default::default()
            },
        )
        .is_err()
    );
}
