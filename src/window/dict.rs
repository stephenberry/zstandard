use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct PreparedDictionaryMatchStateKey {
    pub(crate) parser_strategy: ParserStrategy,
    pub(crate) hash_bits: u32,
    pub(crate) chain_log: u32,
    pub(crate) window_log: u32,
    pub(crate) search_log: u32,
    pub(crate) search_depth: usize,
    pub(crate) min_match: u32,
}

impl PreparedDictionaryMatchStateKey {
    /// Keyed on the geometry the tables are actually built with, which for
    /// every field the dictionary has its own of is the dictionary's. Keying
    /// on `applied` instead would hand back a cached state built for a
    /// different table size the moment two sources of different lengths share
    /// a dictionary.
    ///
    /// So the fields here are exactly the ones
    /// [`build_prepared_dictionary_match_state`] reads, and no others. Both
    /// directions matter. A field the builder reads and the key omits hands
    /// back tables of the wrong shape; a field the key carries and the builder
    /// ignores splits the cache on a distinction that cannot change what is
    /// stored, so the same tables are built again under a second key.
    ///
    /// `secondary_hash_bits` used to be here and was the second kind. It sizes
    /// the *source* side of DoubleFast's two tables
    /// (`PrefixedBlockMatchState::new_with_prepared_match_state`) and reaches
    /// no dictionary table on any strategy — the dictionary's short table comes
    /// from `dictionary_chain_log`. Because it is derived from the *adjusted*
    /// chain log, it moves with the source size below the attach cutoff, so one
    /// dictionary at one level keyed three different ways across a spread of
    /// small inputs and rebuilt identical tables for each. Small inputs are
    /// where a prepared dictionary earns the most, so the split was worst
    /// exactly where the cache mattered.
    pub(crate) fn new(params: MatchFinderParameters) -> Self {
        Self {
            parser_strategy: params.parser_strategy,
            hash_bits: params.dictionary_hash_bits(),
            chain_log: params.dictionary_chain_log(),
            window_log: params.dictionary_window_log(),
            search_log: params.search_log,
            search_depth: params.search_depth,
            min_match: params.min_match,
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum PreparedDictionaryMatchState {
    Chain(PreparedChainDictionaryTables),
    Fast(Arc<PreparedFastDictionaryTables>),
    DoubleFast(Arc<PreparedDoubleFastDictionaryTables>),
    Row(PreparedRowDictionaryTables),
    BinaryTree(PreparedBinaryTreeDictionaryTables),
}

impl PreparedDictionaryMatchState {
    pub(crate) fn chain_table_allocated(&self) -> bool {
        matches!(
            self,
            Self::Chain(_) | Self::DoubleFast(_) | Self::BinaryTree(_)
        )
    }

    pub(crate) fn row_hash_log(&self) -> Option<u32> {
        match self {
            Self::Row(finder) => Some(finder.row_hash_log()),
            _ => None,
        }
    }
}

pub(crate) fn build_prepared_dictionary_match_state(
    prefix: &[u8],
    params: MatchFinderParameters,
) -> Option<PreparedDictionaryMatchState> {
    if prefix.is_empty() {
        return None;
    }

    Some(match params.parser_strategy {
        strategy if strategy.is_row_hash() => {
            let mut finder = RowHashFinder::new(
                params.dictionary_hash_bits(),
                params.search_log,
                params.min_match,
            );
            finder.hash_salt = 0;
            finder.insert_prefix(prefix);
            PreparedDictionaryMatchState::Row(PreparedRowDictionaryTables {
                prefix_finder: Arc::new(finder),
            })
        }
        strategy if strategy.is_hash_chain() => {
            let mut finder = MatchFinder::with_chain_log(
                prefix.len(),
                params.dictionary_hash_bits(),
                params.dictionary_chain_log(),
                params.min_match,
            );
            let prefix_refs = [prefix];
            let prefix_chain = PrefixChain::new(&prefix_refs)
                .expect("single prefix must not overflow")
                .expect("single non-empty prefix expected");
            finder.insert_prefix_chain_for_cdict(prefix_chain);
            PreparedDictionaryMatchState::Chain(PreparedChainDictionaryTables {
                prefix_finder: Arc::new(finder),
            })
        }
        ParserStrategy::Fast => {
            PreparedDictionaryMatchState::Fast(Arc::new(PreparedFastDictionaryTables::build(
                prefix,
                params.dictionary_hash_bits(),
                params.min_match,
            )))
        }
        // C reads both of DoubleFast's dictionary tables off the dictionary's
        // own parameters: the long table from `dictCParams->hashLog` and the
        // short one from `dictCParams->chainLog`
        // (`zstd_double_fast.c:414`). `secondary_hash_bits` *is* `chain_log`
        // on this strategy — see its derivation in `encode.rs` — so the
        // dictionary's chain log is the right value to pass, not a second
        // dictionary-specific field.
        ParserStrategy::DoubleFast => PreparedDictionaryMatchState::DoubleFast(Arc::new(
            PreparedDoubleFastDictionaryTables::build(
                prefix,
                params.dictionary_hash_bits(),
                params.dictionary_chain_log(),
                params.min_match,
            ),
        )),
        strategy if strategy.is_binary_tree() => {
            let mut finder = BinaryTreeFinder::new(
                params.dictionary_hash_bits(),
                params.dictionary_chain_log(),
                params.min_match,
            )
            .with_window_log(params.dictionary_window_log());
            let prefix_refs = [prefix];
            let prefix_chain = PrefixChain::new(&prefix_refs)
                .expect("single prefix must not overflow")
                .expect("single non-empty prefix expected");
            finder.insert_prefix_chain(prefix_chain, &[], params.search_depth);
            PreparedDictionaryMatchState::BinaryTree(PreparedBinaryTreeDictionaryTables {
                prefix_finder: Arc::new(finder),
            })
        }
        _ => return None,
    })
}
