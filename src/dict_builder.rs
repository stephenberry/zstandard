//! Dictionary training: build a Zstandard dictionary from sample data.
//!
//! This is the equivalent of upstream's `ZDICT_trainFromBuffer`, which is the
//! `fastCover` algorithm driven by a parameter search. Training has two halves,
//! and they fail in different ways, so it is worth keeping them apart:
//!
//! 1. **Content selection.** The samples are hashed into a frequency table of
//!    `d`-byte substrings, split into epochs, and the highest-scoring segment of
//!    each epoch is appended to the dictionary content, back to front, so the
//!    best material ends up nearest the end where offsets are smallest. This
//!    half is pure integer arithmetic over the sample bytes. For a given `(k,
//!    d)` it is byte-identical to upstream, which the interop suite pins
//!    directly. The one exception is a `k` above 65535, where upstream's
//!    `u16` window counter wraps and ours does not; see `segment_freqs`.
//!
//! 2. **Measurement and entropy finalization.** Each candidate content is scored
//!    by actually compressing the held-back samples against it, and the winner
//!    is wrapped in a header carrying Huffman and FSE tables trained on the
//!    samples. Both steps run through *this crate's* encoder.
//!
//! The second half is where we and upstream can part company, and it reaches
//! further than it first appears. The obvious effect is on the header: our
//! parse of a sample yields slightly different sequence statistics, so the
//! tables built from them differ. The less obvious effect is on the content,
//! because the search keeps whichever candidate *measured* smallest — so a
//! disagreement about compressed size can make the two implementations settle
//! on different segment sizes and therefore select different bytes, with both
//! behaving correctly.
//!
//! So: dictionaries from this module are valid and, measured against upstream's
//! on held-out samples, land within about a percent either way. They are not
//! byte-identical to upstream's, and that is a property of encoder parity rather
//! than something the trainer can promise on its own.
//!
//! Upstream can spread the parameter search across a thread pool.
//! `ZDICT_trainFromBuffer` does not ask for one, so neither does this.

use crate::{
    dictionary::EncoderDictionary,
    encode::{
        CompressionLevel, CompressionParameters, EncoderOptions, EntropyEncodeScratch,
        compression_parameters_for_dictionary_training, count_dictionary_entropy_stats,
        encode_all_with_prepared_dict_and_options,
    },
    entropy::{fse, huff0, mem::highbit32},
    error::{Error, Result},
    xxhash::xxh64,
};

/// Smallest dictionary a caller may ask for, matching `ZDICT_DICTSIZE_MIN`.
pub const DICTIONARY_SIZE_MIN: usize = 256;

/// Largest offset code the first block of a frame can use.
const OFFCODE_MAX: usize = 30;
const MAX_MATCH_LENGTH_CODE: usize = 52;
const MAX_LITERAL_LENGTH_CODE: usize = 35;
const OFFSET_FSE_LOG: u32 = 8;
const MATCH_LENGTH_FSE_LOG: u32 = 9;
const LITERAL_LENGTH_FSE_LOG: u32 = 9;
const HUFFMAN_LOG: u32 = 11;
/// The repeat offsets a fresh frame starts from, and the ones every trained
/// dictionary records.
///
/// Upstream accumulates a histogram of the offsets its samples actually opened
/// with, sorts it, and then deliberately does not use the result — the code is
/// `#if 0`-ed out with a note that the effect on the rest of the statistics was
/// never evaluated. Since the histogram cannot reach the output, it is not
/// gathered here at all.
const REPEAT_OFFSET_START: [u32; 3] = [1, 4, 8];
/// Upper bound on the entropy header. Upstream uses the same figure and calls
/// it "large enough for all entropy headers"; the writes below are all checked
/// against it regardless.
const HEADER_BUFFER_SIZE: usize = 256;

const FASTCOVER_MAX_F: u32 = 31;
const FASTCOVER_MAX_ACCEL: u32 = 10;
const FASTCOVER_DEFAULT_SPLITPOINT: f64 = 0.75;
const DEFAULT_F: u32 = 20;
const DEFAULT_ACCEL: u32 = 1;
/// Number of `d`-mers to skip between counted ones, and the percentage of
/// training samples to finalize on, per acceleration level. Index 0 is unused:
/// an acceleration of zero means "unset" and resolves to 1.
const ACCEL_PARAMETERS: [(u32, u32); (FASTCOVER_MAX_ACCEL + 1) as usize] = [
    (100, 0),
    (100, 0),
    (50, 1),
    (34, 2),
    (25, 3),
    (20, 4),
    (17, 5),
    (14, 6),
    (13, 7),
    (11, 8),
    (10, 9),
];

/// Parameters for [`train_dictionary_with_parameters`], mirroring upstream's
/// `ZDICT_fastCover_params_t`.
///
/// Every field treats `0` as "choose for me", so [`Default`] is the same thing
/// as passing an all-zero struct to upstream.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DictionaryTrainingParameters {
    /// Segment size. `0` searches `50..=2000`.
    pub k: u32,
    /// Substring length used for frequency counting; must be 6 or 8. `0` tries
    /// both.
    pub d: u32,
    /// Log2 of the frequency table size, `1..=31`. `0` means 20.
    pub f: u32,
    /// Acceleration, `1..=10`. Higher is faster and less thorough. `0` means 1.
    pub accel: u32,
    /// Granularity of the `k` search: the range is divided into this many
    /// steps, so `steps + 1` values are tried. `0` means 40. No effect when `k`
    /// is fixed.
    pub steps: u32,
    /// Fraction of samples used for training rather than measurement. `0.0`
    /// means 0.75.
    pub split_point: f64,
    /// Level the candidate dictionaries are measured at, and whose statistics
    /// the entropy tables are trained on.
    pub compression_level: CompressionLevel,
    /// Dictionary id to record. `0` derives one from the content, as upstream
    /// does, landing in the range reserved for non-registered dictionaries.
    pub dictionary_id: u32,
}

impl Default for DictionaryTrainingParameters {
    fn default() -> Self {
        Self {
            k: 0,
            d: 0,
            f: 0,
            accel: 0,
            steps: 0,
            split_point: 0.0,
            compression_level: CompressionLevel::DEFAULT,
            dictionary_id: 0,
        }
    }
}

/// A trained dictionary and the parameters that produced it.
#[derive(Debug, Clone)]
pub struct TrainedDictionary {
    dictionary: Vec<u8>,
    k: u32,
    d: u32,
    total_compressed_size: usize,
}

impl TrainedDictionary {
    /// The dictionary, ready to hand to
    /// [`EncoderDictionary::new`](crate::EncoderDictionary::new).
    pub fn as_bytes(&self) -> &[u8] {
        &self.dictionary
    }

    /// Take ownership of the dictionary bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.dictionary
    }

    /// The segment size that won the parameter search.
    pub fn k(&self) -> u32 {
        self.k
    }

    /// The substring length that won the parameter search.
    pub fn d(&self) -> u32 {
        self.d
    }

    /// Total compressed size of the measurement samples under this dictionary,
    /// including the dictionary itself. This is the quantity the search
    /// minimizes; it is comparable only between runs over the same samples.
    pub fn total_compressed_size(&self) -> usize {
        self.total_compressed_size
    }
}

/// Train a dictionary of at most `capacity` bytes from `samples`.
///
/// The equivalent of `ZDICT_trainFromBuffer`: `d = 8` with a four-step search
/// over `k`. Returns the dictionary bytes.
///
/// Upstream advises training on at least 10x, preferably 100x, the dictionary
/// size in samples; far less than that produces a dictionary that will
/// disappoint.
///
/// Five samples must fall in the training split and at least one in the
/// measurement split, so with the default split of 0.75 the floor is **seven**
/// samples. Fewer is [`Error::InvalidParameter`], not a weak dictionary.
///
/// ```
/// # fn main() -> zstandard::Result<()> {
/// let records: Vec<Vec<u8>> = (0..64)
///     .map(|i| format!("{{\"id\":{i},\"status\":\"open\",\"path\":\"/v2/objects\"}}").into_bytes())
///     .collect();
/// let samples: Vec<&[u8]> = records.iter().map(Vec::as_slice).collect();
///
/// let dictionary = zstandard::train_dictionary(&samples, 1024)?;
/// let prepared = zstandard::EncoderDictionary::new(&dictionary)?;
/// let compressed = zstandard::encode_all_with_prepared_dict(&records[0], &prepared)?;
/// assert!(compressed.len() < records[0].len());
/// # Ok(())
/// # }
/// ```
pub fn train_dictionary(samples: &[&[u8]], capacity: usize) -> Result<Vec<u8>> {
    let parameters = DictionaryTrainingParameters {
        d: 8,
        steps: 4,
        ..DictionaryTrainingParameters::default()
    };
    Ok(train_dictionary_with_parameters(samples, capacity, parameters)?.into_bytes())
}

/// Train a dictionary with explicit parameters, the equivalent of
/// `ZDICT_optimizeTrainFromBuffer_fastCover`.
///
/// Every zero field in `parameters` is resolved to its default, and the search
/// runs over whatever remains unfixed. Fixing both `k` and `d` reduces the
/// search to a single candidate, which is still measured and finalized the same
/// way.
pub fn train_dictionary_with_parameters(
    samples: &[&[u8]],
    capacity: usize,
    parameters: DictionaryTrainingParameters,
) -> Result<TrainedDictionary> {
    let resolved = ResolvedParameters::resolve(parameters, capacity, samples.len())?;
    let corpus = Corpus::new(samples);

    let mut best: Option<Candidate> = None;
    let mut scratch = TrainingScratch::default();
    for d in (resolved.min_d..=resolved.max_d).step_by(2) {
        let context =
            FrequencyContext::build(&corpus, d, resolved.f, resolved.split_point, resolved.skip)?;
        let mut k = resolved.min_k;
        while k <= resolved.max_k {
            if let Some(candidate) =
                context.try_segment_size(k, capacity, &resolved, &mut scratch)?
            {
                // Strictly better only, so that the smallest `k` wins a tie.
                // Upstream compares the same way, and a tie is common when a
                // range of segment sizes selects the same content.
                if best
                    .as_ref()
                    .is_none_or(|b| candidate.total_compressed_size < b.total_compressed_size)
                {
                    best = Some(candidate);
                }
            }
            // Stepping past the end must not be expressible as a wrap. With
            // `k = u32::MAX` the loop condition can never go false, so a naive
            // increment overflows: a panic in debug, and in release a wrap to
            // zero that restarts the sweep and never terminates.
            match k.checked_add(resolved.k_step) {
                Some(next) => k = next,
                None => break,
            }
        }
    }

    let best = best.ok_or(Error::InvalidParameter(
        "no dictionary could be trained from these samples",
    ))?;
    Ok(TrainedDictionary {
        dictionary: best.dictionary,
        k: best.k,
        d: best.d,
        total_compressed_size: best.total_compressed_size,
    })
}

/// A dictionary produced by one point of the parameter search.
struct Candidate {
    dictionary: Vec<u8>,
    k: u32,
    d: u32,
    total_compressed_size: usize,
}

/// Buffers reused across every point of the parameter search, so that a search
/// over dozens of `k` values allocates its working set once.
#[derive(Default)]
struct TrainingScratch {
    freqs: Vec<u32>,
    /// Occurrences of each `d`-mer within the active window.
    ///
    /// Upstream counts these in a `u16`, which is enough for the segment sizes
    /// its own search uses but not for an arbitrary caller-supplied `k`: a
    /// window of more than 65535 positions over repetitive data overflows it.
    /// In C that wraps silently and corrupts the score; here it would panic.
    /// A `u32` costs one more word per table entry and agrees with C everywhere
    /// C is not already overflowing.
    segment_freqs: Vec<u32>,
    content: Vec<u8>,
    encode: EntropyEncodeScratch,
    block: Vec<u8>,
}

/// Parameters after every "choose for me" zero has been resolved.
struct ResolvedParameters {
    min_d: u32,
    max_d: u32,
    min_k: u32,
    max_k: u32,
    k_step: u32,
    f: u32,
    /// `d`-mers skipped between counted ones.
    skip: u32,
    /// Percentage of training samples used to build the entropy tables.
    finalize_percent: u32,
    split_point: f64,
    compression_level: CompressionLevel,
    dictionary_id: u32,
}

impl ResolvedParameters {
    fn resolve(
        parameters: DictionaryTrainingParameters,
        capacity: usize,
        sample_count: usize,
    ) -> Result<Self> {
        if sample_count == 0 {
            return Err(Error::InvalidParameter(
                "training needs at least one sample",
            ));
        }
        if capacity < DICTIONARY_SIZE_MIN {
            return Err(Error::DstSizeTooSmall);
        }
        let split_point = if parameters.split_point <= 0.0 {
            FASTCOVER_DEFAULT_SPLITPOINT
        } else {
            parameters.split_point
        };
        if split_point > 1.0 {
            return Err(Error::InvalidParameter("split_point must be in (0, 1]"));
        }
        let accel = if parameters.accel == 0 {
            DEFAULT_ACCEL
        } else {
            parameters.accel
        };
        if accel > FASTCOVER_MAX_ACCEL {
            return Err(Error::InvalidParameter("accel must be in 1..=10"));
        }
        let f = if parameters.f == 0 {
            DEFAULT_F
        } else {
            parameters.f
        };
        if f > FASTCOVER_MAX_F {
            return Err(Error::InvalidParameter("f must be in 1..=31"));
        }
        if parameters.d != 0 && parameters.d != 6 && parameters.d != 8 {
            return Err(Error::InvalidParameter("d must be 6 or 8"));
        }
        let (min_d, max_d) = if parameters.d == 0 {
            (6, 8)
        } else {
            (parameters.d, parameters.d)
        };
        let (min_k, max_k) = if parameters.k == 0 {
            (50, 2000)
        } else {
            (parameters.k, parameters.k)
        };
        if min_k < max_d || max_k < min_k {
            return Err(Error::InvalidParameter("k must be at least d"));
        }
        // A `k` larger than the dictionary itself is not an error, it is simply
        // not a candidate. The default search runs up to 2000, so asking for a
        // dictionary smaller than that is ordinary and must not fail; the
        // oversized points are skipped and the rest still run.
        let steps = if parameters.steps == 0 {
            40
        } else {
            parameters.steps
        };
        let (finalize_percent, skip) = ACCEL_PARAMETERS[accel as usize];
        Ok(Self {
            min_d,
            max_d,
            min_k,
            max_k,
            k_step: ((max_k - min_k) / steps).max(1),
            f,
            skip,
            finalize_percent,
            split_point,
            compression_level: parameters.compression_level,
            dictionary_id: parameters.dictionary_id,
        })
    }
}

/// The samples, concatenated, with the boundaries kept alongside.
///
/// Concatenation is not a convenience: segment selection slides a window across
/// the whole corpus and may pick material spanning two samples, so the algorithm
/// is defined over the joined bytes.
struct Corpus {
    bytes: Vec<u8>,
    /// `offsets[i]..offsets[i + 1]` is sample `i`. Length is `count + 1`.
    offsets: Vec<usize>,
}

impl Corpus {
    fn new(samples: &[&[u8]]) -> Self {
        let total = samples.iter().map(|sample| sample.len()).sum();
        let mut bytes = Vec::with_capacity(total);
        let mut offsets = Vec::with_capacity(samples.len() + 1);
        offsets.push(0);
        for sample in samples {
            bytes.extend_from_slice(sample);
            offsets.push(bytes.len());
        }
        Self { bytes, offsets }
    }

    fn count(&self) -> usize {
        self.offsets.len() - 1
    }

    fn sample(&self, index: usize) -> &[u8] {
        &self.bytes[self.offsets[index]..self.offsets[index + 1]]
    }
}

/// Hash the `d` bytes at `pos` into an index of a `2^f`-entry table.
///
/// These are `ZSTD_hash6Ptr` and `ZSTD_hash8Ptr`. Both read eight bytes, so the
/// caller must keep `pos + 8` within the corpus; every call site below is bounded
/// by the `d`-mer count, which already subtracts eight.
#[inline]
fn hash_to_index(bytes: &[u8], pos: usize, f: u32, d: u32) -> usize {
    const PRIME_6_BYTES: u64 = 227_718_039_650_203;
    const PRIME_8_BYTES: u64 = 0xCF1B_BCDC_B7A5_6463;
    let value = u64::from_le_bytes(bytes[pos..pos + 8].try_into().expect("eight bytes"));
    let hashed = if d == 6 {
        (value << 16).wrapping_mul(PRIME_6_BYTES)
    } else {
        value.wrapping_mul(PRIME_8_BYTES)
    };
    (hashed >> (64 - f)) as usize
}

/// The `d`-mer frequency table for one value of `d`, reused across every `k`.
struct FrequencyContext<'a> {
    corpus: &'a Corpus,
    /// Number of samples used to build content, as opposed to measure it.
    train_samples: usize,
    /// Positions at which a `d`-mer starts. Segment selection never looks past
    /// this, which is what keeps the eight-byte reads in bounds.
    dmers: usize,
    freqs: Vec<u32>,
    d: u32,
    f: u32,
}

impl<'a> FrequencyContext<'a> {
    fn build(corpus: &'a Corpus, d: u32, f: u32, split_point: f64, skip: u32) -> Result<Self> {
        let sample_count = corpus.count();
        let (train_samples, test_samples) = if split_point < 1.0 {
            let train = (sample_count as f64 * split_point) as usize;
            (train, sample_count - train)
        } else {
            (sample_count, sample_count)
        };
        let training_bytes = corpus.offsets[train_samples];

        // Both bounds are upstream's. Five is the floor for a frequency table
        // to mean anything, and the read width is why the corpus must exceed
        // eight bytes even when `d` is six.
        let read_length = (d as usize).max(8);
        if corpus.bytes.len() < read_length || corpus.bytes.len() >= u32::MAX as usize {
            return Err(Error::SrcSizeWrong);
        }
        if train_samples < 5 {
            return Err(Error::InvalidParameter(
                "training needs at least 5 training samples",
            ));
        }
        if test_samples < 1 {
            return Err(Error::InvalidParameter(
                "training needs at least one sample held back for measurement",
            ));
        }
        // The `d`-mer count is measured over the *training* portion, so a corpus
        // that is large overall can still leave nothing to count: five empty
        // samples ahead of one large one satisfies every check above. Upstream
        // guards only the total and lets the subtraction below wrap, which turns
        // an empty training set into an enormous `d`-mer count and a read far
        // past the samples.
        if training_bytes < read_length {
            return Err(Error::InvalidParameter(
                "the training samples are too small to count substrings in",
            ));
        }

        // `f` reaches 31, where each of the two tables this training run needs
        // is 8 GiB. `vec!` aborts the process on a failed allocation; a caller
        // parameter must not be able to do that, so ask for the memory in a way
        // that can be refused.
        let mut freqs = zeroed_u32_table(1usize << f)?;
        for index in 0..train_samples {
            let mut start = corpus.offsets[index];
            let end = corpus.offsets[index + 1];
            while start + read_length <= end {
                freqs[hash_to_index(&corpus.bytes, start, f, d)] += 1;
                start += skip as usize + 1;
            }
        }

        Ok(Self {
            corpus,
            train_samples,
            dmers: training_bytes - read_length + 1,
            freqs,
            d,
            f,
        })
    }
}

/// How the corpus is divided for segment selection.
struct Epochs {
    count: u32,
    size: u32,
}

/// `COVER_computeEpochs`: enough epochs to fill the dictionary, but each at
/// least ten segments long so a segment has somewhere to be chosen from.
fn compute_epochs(max_dict_size: u32, dmers: u32, k: u32, passes: u32) -> Epochs {
    // `k` is bounded only by the dictionary capacity, so ten times it does not
    // fit in 32 bits for a large enough request. Saturating is the same answer
    // the arithmetic would give if it were wide: the value is only ever compared
    // against an epoch size, which cannot exceed the `d`-mer count.
    let min_epoch_size = k.saturating_mul(10);
    let count = (max_dict_size / k / passes).max(1);
    let size = dmers / count;
    if size >= min_epoch_size {
        return Epochs { count, size };
    }
    let size = min_epoch_size.min(dmers);
    Epochs {
        count: dmers / size,
        size,
    }
}

/// The best-scoring window of one epoch.
#[derive(Clone, Copy, Default)]
struct Segment {
    begin: u32,
    end: u32,
    score: u32,
}

impl FrequencyContext<'_> {
    /// `FASTCOVER_selectSegment`: slide a `k`-byte window across the epoch,
    /// scoring it by the summed frequency of the distinct `d`-mers it covers,
    /// and return the best. The chosen `d`-mers are then zeroed out of `freqs`
    /// so later epochs do not pay for material already in the dictionary.
    ///
    /// `segment_freqs` counts occurrences within the active window and is left
    /// zeroed on return, so one allocation serves every epoch.
    fn select_segment(
        &self,
        freqs: &mut [u32],
        segment_freqs: &mut [u32],
        begin: u32,
        end: u32,
        k: u32,
    ) -> Segment {
        let dmers_in_k = k - self.d + 1;
        let mut best = Segment::default();
        let mut active = Segment {
            begin,
            end: begin,
            score: 0,
        };

        while active.end < end {
            let index = hash_to_index(&self.corpus.bytes, active.end as usize, self.f, self.d);
            if segment_freqs[index] == 0 {
                active.score = active.score.wrapping_add(freqs[index]);
            }
            active.end += 1;
            segment_freqs[index] += 1;
            if active.end - active.begin == dmers_in_k + 1 {
                let removed =
                    hash_to_index(&self.corpus.bytes, active.begin as usize, self.f, self.d);
                segment_freqs[removed] -= 1;
                if segment_freqs[removed] == 0 {
                    active.score = active.score.wrapping_sub(freqs[removed]);
                }
                active.begin += 1;
            }
            if active.score > best.score {
                best = active;
            }
        }

        while active.begin < end {
            let removed = hash_to_index(&self.corpus.bytes, active.begin as usize, self.f, self.d);
            segment_freqs[removed] -= 1;
            active.begin += 1;
        }

        for pos in best.begin..best.end {
            let index = hash_to_index(&self.corpus.bytes, pos as usize, self.f, self.d);
            freqs[index] = 0;
        }
        best
    }

    /// `FASTCOVER_buildDictionary`: take one segment from each epoch, filling
    /// the content buffer from the back so the best material sits nearest the
    /// end, where a match against it costs the fewest offset bits.
    ///
    /// Returns the unused head of `content`; the selected material is
    /// everything after it.
    fn build_content(
        &self,
        freqs: &mut [u32],
        segment_freqs: &mut [u32],
        content: &mut [u8],
        k: u32,
    ) -> usize {
        let capacity = content.len();
        let mut tail = capacity;
        let epochs = compute_epochs(capacity as u32, self.dmers as u32, k, 1);
        // An epoch whose best segment scores zero has nothing left to give, but
        // a later epoch may still have material, so keep going for a while
        // before concluding the corpus is exhausted.
        const MAX_ZERO_SCORE_RUN: usize = 10;
        let mut zero_score_run = 0;
        let mut epoch = 0u32;
        while tail > 0 {
            let begin = epoch * epochs.size;
            let segment = self.select_segment(freqs, segment_freqs, begin, begin + epochs.size, k);
            epoch = (epoch + 1) % epochs.count;

            if segment.score == 0 {
                zero_score_run += 1;
                if zero_score_run >= MAX_ZERO_SCORE_RUN {
                    break;
                }
                continue;
            }
            zero_score_run = 0;

            let size = ((segment.end - segment.begin + self.d - 1) as usize).min(tail);
            if size < self.d as usize {
                break;
            }
            tail -= size;
            let from = segment.begin as usize;
            content[tail..tail + size].copy_from_slice(&self.corpus.bytes[from..from + size]);
        }
        tail
    }

    /// Build the content for one `k`, finalize it into a dictionary, and measure
    /// that dictionary against the held-back samples.
    ///
    /// `Ok(None)` means this `k` produced nothing usable, which is not an error:
    /// the search simply has no candidate here.
    fn try_segment_size(
        &self,
        k: u32,
        capacity: usize,
        resolved: &ResolvedParameters,
        scratch: &mut TrainingScratch,
    ) -> Result<Option<Candidate>> {
        if k < self.d || k as usize > capacity {
            return Ok(None);
        }

        // `select_segment` mutates frequencies as it consumes material, so each
        // `k` starts from a fresh copy of the corpus-wide counts.
        scratch.freqs.clear();
        scratch.freqs.extend_from_slice(&self.freqs);
        scratch.segment_freqs.clear();
        scratch
            .segment_freqs
            .try_reserve_exact(1usize << self.f)
            .map_err(|_| Error::Generic)?;
        scratch.segment_freqs.resize(1usize << self.f, 0);
        scratch.content.clear();
        scratch.content.resize(capacity, 0);

        let tail = self.build_content(
            &mut scratch.freqs,
            &mut scratch.segment_freqs,
            &mut scratch.content,
            k,
        );
        let content_size = capacity - tail;
        if content_size == 0 {
            return Ok(None);
        }

        let finalize_samples = (self.train_samples * resolved.finalize_percent as usize / 100)
            .min(self.corpus.count());
        let dictionary = finalize_dictionary(
            &scratch.content[tail..],
            self.corpus,
            finalize_samples,
            capacity,
            resolved,
            &mut scratch.encode,
            &mut scratch.block,
        )?;

        let total_compressed_size = self.measure(&dictionary, resolved)?;
        Ok(Some(Candidate {
            dictionary,
            k,
            d: self.d,
            total_compressed_size,
        }))
    }

    /// `COVER_checkTotalCompressedSize`: compress every measurement sample with
    /// the candidate and sum the results, counting the dictionary itself so a
    /// larger dictionary must earn its size back.
    fn measure(&self, dictionary: &[u8], resolved: &ResolvedParameters) -> Result<usize> {
        let prepared = EncoderDictionary::new(dictionary)?;
        let options = EncoderOptions::default().with_compression_level(resolved.compression_level);
        let mut total = dictionary.len();
        // With a split below 1.0 the measurement set is the samples training
        // did not see. With no split it is every sample, and the number is a
        // fit to the training data rather than an estimate of held-out gain.
        let first = if resolved.split_point < 1.0 {
            self.train_samples
        } else {
            0
        };
        for index in first..self.corpus.count() {
            // Empty samples are compressed rather than skipped, because upstream
            // compresses them: each contributes an empty frame's worth of bytes.
            // The ranking is a comparison between candidates and so is unmoved
            // either way, but `total_compressed_size` is reported to the caller
            // and should mean the same thing it means upstream.
            let sample = self.corpus.sample(index);
            total += encode_all_with_prepared_dict_and_options(sample, &prepared, options)?.len();
        }
        Ok(total)
    }
}

/// Symbol histograms gathered across the training samples.
///
/// Every count starts at one rather than zero: a table that cannot describe a
/// symbol at all would make the first block that used it unencodable, so each
/// symbol keeps a floor of one occurrence whether it appeared or not.
pub(crate) struct EntropyStats {
    pub(crate) literals: [u32; 256],
    pub(crate) offset_codes: [u32; OFFCODE_MAX + 1],
    pub(crate) match_lengths: [u32; MAX_MATCH_LENGTH_CODE + 1],
    pub(crate) literal_lengths: [u32; MAX_LITERAL_LENGTH_CODE + 1],
}

impl EntropyStats {
    fn new(offcode_max: usize) -> Self {
        let mut offset_codes = [0u32; OFFCODE_MAX + 1];
        for slot in offset_codes.iter_mut().take(offcode_max + 1) {
            *slot = 1;
        }
        Self {
            literals: [1; 256],
            offset_codes,
            match_lengths: [1; MAX_MATCH_LENGTH_CODE + 1],
            literal_lengths: [1; MAX_LITERAL_LENGTH_CODE + 1],
        }
    }

    /// Replace the literal histogram with a flat but still compressible one.
    ///
    /// A distribution whose Huffman table needs a full eight bits per symbol
    /// cannot be serialized, so upstream substitutes a shape that can be. It
    /// costs ratio on the first block and only fires on samples that are noise
    /// or perfectly uniform.
    fn flatten_literals(&mut self) {
        self.literals = [2; 256];
        self.literals[0] = 4;
        self.literals[253] = 1;
        self.literals[254] = 1;
    }
}

/// `ZDICT_finalizeDictionary`: wrap selected content in a header carrying a
/// dictionary id, entropy tables trained on the samples, and starting repeat
/// offsets.
fn finalize_dictionary(
    content: &[u8],
    corpus: &Corpus,
    finalize_samples: usize,
    capacity: usize,
    resolved: &ResolvedParameters,
    scratch: &mut EntropyEncodeScratch,
    block: &mut Vec<u8>,
) -> Result<Vec<u8>> {
    if capacity < DICTIONARY_SIZE_MIN {
        return Err(Error::DstSizeTooSmall);
    }

    let mut header = [0u8; HEADER_BUFFER_SIZE];
    header[..4].copy_from_slice(&crate::dictionary::ZSTD_DICTIONARY_MAGIC.to_le_bytes());
    let dictionary_id = if resolved.dictionary_id != 0 {
        resolved.dictionary_id
    } else {
        // Upstream's rule: hash the content and fold it into the range reserved
        // for dictionaries that were never registered.
        let hashed = xxh64(content, 0);
        ((hashed % ((1u64 << 31) - 32768)) + 32768) as u32
    };
    header[4..8].copy_from_slice(&dictionary_id.to_le_bytes());
    let mut header_size = 8;

    header_size += analyze_entropy(
        &mut header[header_size..],
        content,
        corpus,
        finalize_samples,
        resolved,
        scratch,
        block,
    )?;

    // Trim content that will not fit alongside the header.
    //
    // The bytes dropped are the ones at the *end*, which is where segment
    // selection put the highest-scoring material. That reads backwards, and it
    // is what upstream does: it takes a prefix of the content and lets the tail
    // go. Selecting the tail instead would be defensible on its own terms and
    // would make every trained dictionary differ from upstream's, since the
    // content normally fills the buffer and this trim normally fires.
    let content = &content[..content.len().min(capacity - header_size)];

    // The content must be at least as long as the largest starting repeat
    // offset, or that offset would reach past the front of the dictionary.
    let min_content_size = REPEAT_OFFSET_START.iter().copied().max().unwrap_or(0) as usize;
    let padding_size = if content.len() < min_content_size {
        if header_size + min_content_size > capacity {
            return Err(Error::DstSizeTooSmall);
        }
        min_content_size - content.len()
    } else {
        0
    };

    // Padding goes before the content, never after: the last byte of a
    // dictionary is its most valuable position.
    let mut dictionary = Vec::with_capacity(header_size + padding_size + content.len());
    dictionary.extend_from_slice(&header[..header_size]);
    dictionary.resize(header_size + padding_size, 0);
    dictionary.extend_from_slice(content);
    Ok(dictionary)
}

/// `ZDICT_analyzeEntropy`: compress the samples against the candidate content,
/// then build and serialize the Huffman and FSE tables their statistics imply.
fn analyze_entropy(
    dst: &mut [u8],
    content: &[u8],
    corpus: &Corpus,
    finalize_samples: usize,
    resolved: &ResolvedParameters,
    scratch: &mut EntropyEncodeScratch,
    block: &mut Vec<u8>,
) -> Result<usize> {
    // Offset codes are bounded by the furthest back a first block can reach,
    // which is the dictionary plus one block. A dictionary large enough to
    // exceed the first block's offset code space cannot be described here.
    let offcode_max = highbit32((content.len() + (128 << 10)) as u32) as usize;
    if offcode_max > OFFCODE_MAX {
        return Err(Error::InvalidParameter(
            "dictionary is too large to describe its offset codes",
        ));
    }

    let total_size: usize = (0..finalize_samples).map(|i| corpus.sample(i).len()).sum();
    let average_size = total_size / finalize_samples.max(1);

    let prepared = EncoderDictionary::raw_content(content);
    let params: CompressionParameters = compression_parameters_for_dictionary_training(
        resolved.compression_level,
        Some(average_size),
        prepared.as_inner(),
    );

    let mut stats = EntropyStats::new(offcode_max);
    for index in 0..finalize_samples {
        let sample = corpus.sample(index);
        if sample.is_empty() {
            continue;
        }
        count_dictionary_entropy_stats(sample, &prepared, params, scratch, block, &mut stats)?;
    }

    let mut written = 0usize;
    let mut huffman_workspace = huff0::CompressWorkspace::default();
    let mut huffman_log = HUFFMAN_LOG;
    {
        let mut max_bits =
            huffman_workspace.build_literal_ctable(&stats.literals, 255, huffman_log)?;
        if max_bits == 8 {
            stats.flatten_literals();
            max_bits = huffman_workspace.build_literal_ctable(&stats.literals, 255, huffman_log)?;
        }
        huffman_log = max_bits;
        written += huffman_workspace.write_literal_ctable(dst, 255, huffman_log)?;
    }

    written += write_normalized_table(
        &mut dst[written..],
        &stats.offset_codes[..=offcode_max],
        OFFSET_FSE_LOG,
        offcode_max as u32,
        // The offset table is written out to the full code space even when the
        // samples only exercised part of it, because a later block may reach
        // further back than any first block did.
        OFFCODE_MAX as u32,
    )?;
    written += write_normalized_table(
        &mut dst[written..],
        &stats.match_lengths,
        MATCH_LENGTH_FSE_LOG,
        MAX_MATCH_LENGTH_CODE as u32,
        MAX_MATCH_LENGTH_CODE as u32,
    )?;
    written += write_normalized_table(
        &mut dst[written..],
        &stats.literal_lengths,
        LITERAL_LENGTH_FSE_LOG,
        MAX_LITERAL_LENGTH_CODE as u32,
        MAX_LITERAL_LENGTH_CODE as u32,
    )?;

    if dst.len() < written + 12 {
        return Err(Error::DstSizeTooSmall);
    }
    // Upstream computes the most common leading offsets and then deliberately
    // does not use them, on the grounds that their effect on the rest of the
    // statistics has never been evaluated. The starting values go out instead.
    for (index, offset) in REPEAT_OFFSET_START.iter().enumerate() {
        let at = written + index * 4;
        dst[at..at + 4].copy_from_slice(&offset.to_le_bytes());
    }
    Ok(written + 12)
}

/// The four histograms training accumulates, in the order the entropy header
/// writes them: literals, offset codes, match lengths, literal lengths.
#[cfg(any(feature = "internal-trace", test))]
#[doc(hidden)]
pub type DictionaryEntropyHistograms = (Vec<u32>, Vec<u32>, Vec<u32>, Vec<u32>);

/// The histograms training would accumulate for `content` over `samples`,
/// before any table is built from them.
///
/// The parity harness compares these against upstream's directly. A finished
/// entropy table is a lossy function of its histogram, so comparing tables
/// cannot say whether a divergence came from the statistics or from the table
/// construction; comparing the counts can.
#[cfg(any(feature = "internal-trace", test))]
#[cfg_attr(not(feature = "internal-trace"), allow(dead_code))]
#[doc(hidden)]
pub fn trace_dictionary_entropy_stats(
    content: &[u8],
    samples: &[&[u8]],
    level: CompressionLevel,
) -> Result<DictionaryEntropyHistograms> {
    let total: usize = samples.iter().map(|sample| sample.len()).sum();
    let average = total / samples.len().max(1);
    let offcode_max = highbit32((content.len() + (128 << 10)) as u32) as usize;
    let prepared = EncoderDictionary::raw_content(content);
    let params =
        compression_parameters_for_dictionary_training(level, Some(average), prepared.as_inner());

    let mut stats = EntropyStats::new(offcode_max);
    let mut scratch = EntropyEncodeScratch::default();
    let mut block = Vec::new();
    for sample in samples {
        if sample.is_empty() {
            continue;
        }
        count_dictionary_entropy_stats(
            sample,
            &prepared,
            params,
            &mut scratch,
            &mut block,
            &mut stats,
        )?;
    }
    Ok((
        stats.literals.to_vec(),
        stats.offset_codes.to_vec(),
        stats.match_lengths.to_vec(),
        stats.literal_lengths.to_vec(),
    ))
}

/// A zeroed table of `len` `u32`s, or an error if the allocation is refused.
///
/// `vec![0; len]` aborts the process when the allocation fails. The frequency
/// tables are sized by a caller-supplied parameter, so that is the wrong
/// failure mode: it must be reportable.
fn zeroed_u32_table(len: usize) -> Result<Vec<u32>> {
    let mut table = Vec::new();
    table.try_reserve_exact(len).map_err(|_| Error::Generic)?;
    table.resize(len, 0);
    Ok(table)
}

/// Normalize one histogram and write its FSE header.
///
/// `normalize_max` is the largest symbol the counts describe; `write_max` is the
/// largest the header declares. They differ for offset codes, where the samples
/// may not reach as far as a later block might.
fn write_normalized_table(
    dst: &mut [u8],
    counts: &[u32],
    table_log: u32,
    normalize_max: u32,
    write_max: u32,
) -> Result<usize> {
    let total: u32 = counts.iter().sum();
    let mut normalized = vec![0i16; write_max as usize + 1];
    let table_log = fse::normalize_count(
        &mut normalized,
        table_log,
        counts,
        total as usize,
        normalize_max,
        true,
    )?;
    fse::write_ncount(dst, &normalized, write_max, table_log)
}

#[cfg(test)]
mod tests;
