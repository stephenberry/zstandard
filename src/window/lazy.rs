use super::*;

/// Matches C's ZSTD_bitmix (based on XXH3_rrmxmx).
#[inline(always)]
fn bitmix(val: u64, len: u64) -> u64 {
    let mut v = val;
    v ^= v.rotate_right(49) ^ v.rotate_right(24);
    v = v.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    v ^= (v >> 35).wrapping_add(len);
    v = v.wrapping_mul(0x9FB2_1C65_1E98_DF25);
    v ^ (v >> 28)
}

#[allow(dead_code)]
pub(crate) fn plan_sequences_with_prefix_chain(
    src: &[u8],
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
) -> Result<SequencePlan> {
    let mut plan = SequencePlan::default();
    plan_sequences_with_prefix_chain_into(&mut plan, src, prefix_chain, repeat_offsets, params)?;
    Ok(plan)
}

pub(crate) fn plan_sequences_with_prefix_chain_into(
    plan: &mut SequencePlan,
    src: &[u8],
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
) -> Result<()> {
    if params.parser_strategy.is_row_hash() {
        let mut prefix_finder =
            RowHashFinder::new(params.hash_bits, params.search_log, params.min_match);
        prefix_finder.insert_prefix_chain(prefix_chain, src);
        let mut src_finder =
            RowHashFinder::new(params.hash_bits, params.search_log, params.min_match);
        return plan_sequences_row_ext_dict_from_into(
            plan,
            src,
            0,
            prefix_chain,
            repeat_offsets,
            params,
            PrefixedMatchFloor::fixed(0, 0),
            &prefix_finder,
            &mut src_finder,
        );
    }

    let mut prefix_finder = MatchFinder::with_chain_log(
        prefix_chain.len(),
        params.hash_bits,
        params.chain_log,
        params.min_match,
    );
    prefix_finder.insert_prefix_chain(prefix_chain, src);
    let mut src_finder = prefix_finder.clone();
    plan_sequences_chain_ext_dict_with_prefix_chain_from_into(
        plan,
        src,
        0,
        prefix_chain,
        repeat_offsets,
        params,
        PrefixedMatchFloor::fixed(0, 0),
        &mut src_finder,
    )
}

#[allow(dead_code)]
pub(crate) fn plan_sequences_with_prefix_chain_from(
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &impl LazySearchFinder,
    src_finder: &mut impl LazySearchFinder,
    mode: PrefixMatchMode,
) -> Result<SeqStore> {
    let mut plan = SequencePlan::default();
    plan_sequences_with_prefix_chain_from_into(
        &mut plan,
        src,
        block_start,
        prefix_chain,
        repeat_offsets,
        params,
        match_floor,
        prefix_finder,
        src_finder,
        mode,
    )?;
    Ok(plan)
}

pub(crate) fn plan_sequences_with_prefix_chain_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &impl LazySearchFinder,
    src_finder: &mut impl LazySearchFinder,
    mode: PrefixMatchMode,
) -> Result<()> {
    if matches!(
        params.parser_strategy,
        ParserStrategy::Greedy
            | ParserStrategy::Lazy
            | ParserStrategy::Lazy2
            | ParserStrategy::BinaryTreeLazy2
    ) {
        return plan_sequences_lazy_with_prefix_chain_from_into(
            plan,
            src,
            block_start,
            prefix_chain,
            repeat_offsets,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            mode,
        );
    }

    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let mut repeat_offsets = repeat_offsets;
    let mut anchor = block_start;
    let mut pos = block_start + usize::from(block_start == 0);

    while pos + MIN_MATCH <= src.len() {
        let (prefix_low, source_low) = match_floor.at(pos);
        let literal_length = pos - anchor;
        let Some(candidate) = best_match_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            repeat_offsets.values(),
            literal_length,
            params,
            prefix_low,
            source_low,
            mode,
            prefix_finder,
            src_finder,
        ) else {
            src_finder.insert(src, pos);
            pos += skip_after_no_match(anchor, pos, params);
            continue;
        };

        let found = extend_back_prefix_chain_match(
            prefix_chain,
            src,
            anchor,
            pos,
            candidate,
            prefix_low,
            source_low,
        );
        if !should_accept_match(
            MatchCandidate {
                offset: found.offset,
                length: found.length,
            },
            found.start.saturating_sub(anchor),
            params,
        ) {
            src_finder.insert(src, pos);
            pos += skip_after_no_match(anchor, pos, params);
            continue;
        }

        let lazy = find_lazy_match_skip_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            anchor,
            repeat_offsets.values(),
            candidate,
            params,
            match_floor,
            mode,
            prefix_finder,
            src_finder,
        );
        if lazy.skip != 0 {
            pos += lazy.skip;
            continue;
        }

        store_lazy_sequence_with_source(
            plan,
            src,
            &mut anchor,
            &mut repeat_offsets,
            found.start,
            found.offset,
            found.length,
            SequenceTraceMatchSource::Unknown,
        )?;

        let insert_start = pos + lazy.inserted;
        pos = anchor;
        src_finder.insert_range(src, insert_start, anchor);
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

fn chain_prefixed_store_match_and_chain_rep2_core<const PROFILE: bool>(
    plan: &mut SequencePlan,
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    offset_1: &mut usize,
    offset_2: &mut usize,
    best: LazyParserMatch,
    best_source: SequenceTraceMatchSource,
    match_floor: PrefixedMatchFloor,
    search_limit: usize,
    timings: Option<&mut PlanningIterationTimings>,
) -> Result<usize> {
    let mut timings = timings;
    let anchor_before = *anchor;
    let offset_1_before = *offset_1;
    let offset_2_before = *offset_2;
    let store_start = PROFILE.then(Instant::now);
    let store_snapshot =
        PlanningIterationCategorySnapshot::capture(PROFILE.then_some(timings.as_deref()).flatten());
    match best.kind {
        LazyMatchKind::Repeat1 => {
            let raw_offset = store_lazy_sequence_with_offset_value_and_source(
                plan,
                src,
                anchor,
                repeat_offsets,
                best.start,
                1,
                best.length,
                SequenceTraceMatchSource::Rep,
            )?;
            debug_assert_eq!(raw_offset as usize, *offset_1);
            trace_lazy_emission(
                plan,
                SequenceTraceEmissionKind::Rep,
                SequenceTraceMatchSource::Rep,
                anchor_before,
                best.start,
                best.length,
                1,
                raw_offset as usize,
                offset_1_before,
                offset_2_before,
                *offset_1,
                *offset_2,
            );
        }
        LazyMatchKind::Regular => {
            // Extending backwards from the match's own start, so the floor is
            // the one that position sees.
            let (prefix_low, source_low) = match_floor.at(best.start);
            let found = extend_back_prefix_chain_match(
                prefix_chain,
                src,
                *anchor,
                best.start,
                best.candidate(),
                prefix_low,
                source_low,
            );
            let off_base = explicit_offbase(found.offset);
            *offset_2 = *offset_1;
            *offset_1 = found.offset;
            let raw_offset = store_lazy_sequence_with_offset_value_and_source(
                plan,
                src,
                anchor,
                repeat_offsets,
                found.start,
                off_base,
                found.length,
                best_source,
            )?;
            debug_assert_eq!(raw_offset as usize, found.offset);
            trace_lazy_emission(
                plan,
                SequenceTraceEmissionKind::Regular,
                best_source,
                anchor_before,
                found.start,
                found.length,
                off_base,
                raw_offset as usize,
                offset_1_before,
                offset_2_before,
                *offset_1,
                *offset_2,
            );
        }
    }
    if PROFILE {
        record_lazy_parser_phase(
            timings.as_deref_mut(),
            LazyParserPhase::Store,
            store_start,
            store_snapshot,
        );
    }
    debug_assert!(row_dict_match_state_offsets_synced(
        *repeat_offsets,
        *offset_1,
        *offset_2
    ));

    let rep2_start = PROFILE.then(Instant::now);
    let rep2_snapshot =
        PlanningIterationCategorySnapshot::capture(PROFILE.then_some(timings.as_deref()).flatten());
    let mut pos = *anchor;
    while pos <= search_limit {
        // The chain advances `pos` itself, so the floor moves with it.
        let (prefix_low, source_low) = match_floor.at(pos);
        let anchor_before = *anchor;
        let offset_1_before = *offset_1;
        let offset_2_before = *offset_2;
        let rep2 = *offset_2;
        let rep_start = PROFILE.then(Instant::now);
        let Some(candidate) =
            repeat_match_with_prefix_chain_at(prefix_chain, src, pos, rep2, prefix_low, source_low)
        else {
            if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
                timings.rep_check += rep_start.elapsed();
            }
            break;
        };
        if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
            timings.rep_check += rep_start.elapsed();
        }
        *offset_2 = *offset_1;
        *offset_1 = rep2;
        let raw_offset = store_lazy_sequence_with_offset_value_and_source(
            plan,
            src,
            anchor,
            repeat_offsets,
            pos,
            1,
            candidate.length,
            SequenceTraceMatchSource::Rep,
        )?;
        debug_assert_eq!(raw_offset as usize, rep2);
        trace_lazy_emission(
            plan,
            SequenceTraceEmissionKind::Rep,
            SequenceTraceMatchSource::Rep,
            anchor_before,
            pos,
            candidate.length,
            1,
            raw_offset as usize,
            offset_1_before,
            offset_2_before,
            *offset_1,
            *offset_2,
        );
        debug_assert!(row_dict_match_state_offsets_synced(
            *repeat_offsets,
            *offset_1,
            *offset_2
        ));
        pos = *anchor;
    }
    if PROFILE {
        record_lazy_parser_phase(timings, LazyParserPhase::Rep2, rep2_start, rep2_snapshot);
    }

    Ok(pos)
}

pub(crate) fn chain_prefixed_store_match_and_chain_rep2(
    plan: &mut SequencePlan,
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    offset_1: &mut usize,
    offset_2: &mut usize,
    best: LazyParserMatch,
    best_source: SequenceTraceMatchSource,
    match_floor: PrefixedMatchFloor,
    search_limit: usize,
    timings: Option<&mut PlanningIterationTimings>,
) -> Result<usize> {
    if timings.is_some() {
        chain_prefixed_store_match_and_chain_rep2_core::<true>(
            plan,
            prefix_chain,
            src,
            anchor,
            repeat_offsets,
            offset_1,
            offset_2,
            best,
            best_source,
            match_floor,
            search_limit,
            timings,
        )
    } else {
        chain_prefixed_store_match_and_chain_rep2_core::<false>(
            plan,
            prefix_chain,
            src,
            anchor,
            repeat_offsets,
            offset_1,
            offset_2,
            best,
            best_source,
            match_floor,
            search_limit,
            timings,
        )
    }
}

#[derive(Debug, Clone, Copy)]
struct ChainLazyParserChoice {
    best: LazyParserMatch,
    source: SequenceTraceMatchSource,
}

impl ChainLazyParserChoice {
    #[inline]
    fn repeat(start: usize, candidate: MatchCandidate) -> Self {
        Self {
            best: LazyParserMatch {
                start,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Repeat1,
            },
            source: SequenceTraceMatchSource::Rep,
        }
    }

    #[inline]
    fn regular(start: usize, source: SequenceTraceMatchSource, candidate: MatchCandidate) -> Self {
        Self {
            best: LazyParserMatch {
                start,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Regular,
            },
            source,
        }
    }
}

pub(crate) fn lazy_search_limit(src_len: usize) -> usize {
    src_len.saturating_sub(8)
}

pub(crate) fn plan_sequences_chain_dict_match_state_with_prefix_chain_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &MatchFinder,
    src_finder: &mut MatchFinder,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let mut repeat_offsets = repeat_offsets;
    let (mut offset_1, mut offset_2) = repeat_offsets12(repeat_offsets);
    let depth = params.lazy_search_depth.min(2);
    let mut anchor = block_start;
    let mut pos = block_start + usize::from(block_start == 0);
    let mut lazy_skipping = false;
    let search_limit = lazy_search_limit(src.len());

    while pos < search_limit {
        let (prefix_low, source_low) = match_floor.at(pos);
        let profiling_enabled = plan.planning_profile.is_enabled();
        let iteration_start = profiling_enabled.then(Instant::now);
        let mut iteration_timings = PlanningIterationTimings::default();

        let rep_start = profiling_enabled.then(Instant::now);
        let rep = repeat_ahead_match_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            offset_1,
            prefix_low,
            source_low,
        );
        if let Some(rep_start) = rep_start {
            iteration_timings.rep_check += rep_start.elapsed();
        }
        let mut best = rep.map(|candidate| LazyParserMatch {
            start: pos + 1,
            offset: candidate.offset,
            length: candidate.length,
            kind: LazyMatchKind::Repeat1,
        });
        let mut best_source = best.map(|_| SequenceTraceMatchSource::Rep);
        let current_before_regular = best;
        let current_source_before_regular = best_source;
        // Greedy takes the depth-0 repeat immediately; see the ext-dict chain path below
        // for the reasoning and the C line. Worth 1.01x to 1.25x of upstream's size on
        // the attach path, where a prepared dictionary is searched alongside the source
        // and the extra reach makes a longer, more expensive match easier to find.
        let regular_search = if depth == 0 && best.is_some() {
            ChainRegularMatchSearch::default()
        } else {
            best_chain_dict_match_state_regular_match(
                prefix_chain,
                src,
                pos,
                params,
                prefix_low,
                source_low,
                prefix_finder,
                src_finder,
                lazy_skipping,
                profiling_enabled.then_some(&mut iteration_timings),
            )
        };
        if let Some(candidate) = regular_search.candidate {
            let regular = LazyParserMatch {
                start: pos,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Regular,
            };
            if best.is_none_or(|current| regular.length > current.length) {
                best = Some(regular);
                best_source = Some(regular_search.source);
            }
        }

        let Some(mut best) = best else {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = next_pos;
            if let Some(iteration_start) = iteration_start {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        };
        trace_chain_search(
            plan,
            anchor,
            pos,
            0,
            offset_1,
            offset_2,
            current_before_regular.unwrap_or(best),
            current_source_before_regular.unwrap_or(SequenceTraceMatchSource::Unknown),
            rep.map_or(0, |candidate| candidate.length),
            regular_search,
            best,
            best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
        );

        if depth >= 1 {
            let mut probe_pos = pos;
            loop {
                let depth1_pos = probe_pos + 1;
                if depth1_pos > search_limit {
                    break;
                }
                // The floor moves with the probe, as it does in C.
                let (prefix_low, source_low) = match_floor.at(depth1_pos);

                let rep_start = profiling_enabled.then(Instant::now);
                let rep = repeat_match_with_prefix_chain_at(
                    prefix_chain,
                    src,
                    depth1_pos,
                    offset_1,
                    prefix_low,
                    source_low,
                );
                if let Some(rep_start) = rep_start {
                    iteration_timings.rep_check += rep_start.elapsed();
                }
                if let Some(candidate) =
                    rep.filter(|candidate| lazy_repeat_match_improves(best, *candidate, 3))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Repeat1,
                    };
                    best_source = Some(SequenceTraceMatchSource::Rep);
                }

                let current_before_regular = best;
                let current_source_before_regular = best_source;
                let regular_search = best_chain_dict_match_state_regular_match(
                    prefix_chain,
                    src,
                    depth1_pos,
                    params,
                    prefix_low,
                    source_low,
                    prefix_finder,
                    src_finder,
                    lazy_skipping,
                    profiling_enabled.then_some(&mut iteration_timings),
                );
                if let Some(candidate) = regular_search
                    .candidate
                    .filter(|candidate| lazy_regular_match_improves(best, *candidate, 4))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Regular,
                    };
                    best_source = Some(regular_search.source);
                    trace_chain_search(
                        plan,
                        anchor,
                        depth1_pos,
                        1,
                        offset_1,
                        offset_2,
                        current_before_regular,
                        current_source_before_regular.unwrap_or(SequenceTraceMatchSource::Unknown),
                        rep.map_or(0, |candidate| candidate.length),
                        regular_search,
                        best,
                        best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
                    );
                    probe_pos = depth1_pos;
                    continue;
                }
                trace_chain_search(
                    plan,
                    anchor,
                    depth1_pos,
                    1,
                    offset_1,
                    offset_2,
                    current_before_regular,
                    current_source_before_regular.unwrap_or(SequenceTraceMatchSource::Unknown),
                    rep.map_or(0, |candidate| candidate.length),
                    regular_search,
                    best,
                    best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
                );

                if depth == 2 {
                    let depth2_pos = depth1_pos + 1;
                    if depth2_pos > search_limit {
                        break;
                    }
                    let (prefix_low, source_low) = match_floor.at(depth2_pos);

                    let rep_start = profiling_enabled.then(Instant::now);
                    let rep = repeat_match_with_prefix_chain_at(
                        prefix_chain,
                        src,
                        depth2_pos,
                        offset_1,
                        prefix_low,
                        source_low,
                    );
                    if let Some(rep_start) = rep_start {
                        iteration_timings.rep_check += rep_start.elapsed();
                    }
                    if let Some(candidate) =
                        rep.filter(|candidate| lazy_repeat_match_improves(best, *candidate, 4))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Repeat1,
                        };
                        best_source = Some(SequenceTraceMatchSource::Rep);
                    }

                    let current_before_regular = best;
                    let current_source_before_regular = best_source;
                    let regular_search = best_chain_dict_match_state_regular_match(
                        prefix_chain,
                        src,
                        depth2_pos,
                        params,
                        prefix_low,
                        source_low,
                        prefix_finder,
                        src_finder,
                        lazy_skipping,
                        profiling_enabled.then_some(&mut iteration_timings),
                    );
                    if let Some(candidate) = regular_search
                        .candidate
                        .filter(|candidate| lazy_regular_match_improves(best, *candidate, 7))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Regular,
                        };
                        best_source = Some(regular_search.source);
                        trace_chain_search(
                            plan,
                            anchor,
                            depth2_pos,
                            2,
                            offset_1,
                            offset_2,
                            current_before_regular,
                            current_source_before_regular
                                .unwrap_or(SequenceTraceMatchSource::Unknown),
                            rep.map_or(0, |candidate| candidate.length),
                            regular_search,
                            best,
                            best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
                        );
                        probe_pos = depth2_pos;
                        continue;
                    }
                    trace_chain_search(
                        plan,
                        anchor,
                        depth2_pos,
                        2,
                        offset_1,
                        offset_2,
                        current_before_regular,
                        current_source_before_regular.unwrap_or(SequenceTraceMatchSource::Unknown),
                        rep.map_or(0, |candidate| candidate.length),
                        regular_search,
                        best,
                        best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
                    );
                }

                break;
            }
        }

        let literal_length = best.start.saturating_sub(anchor);
        if !should_accept_match(best.candidate(), literal_length, params) {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = next_pos;
            if let Some(iteration_start) = iteration_start {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        }

        pos = chain_prefixed_store_match_and_chain_rep2(
            plan,
            prefix_chain,
            src,
            &mut anchor,
            &mut repeat_offsets,
            &mut offset_1,
            &mut offset_2,
            best,
            best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
            match_floor,
            search_limit,
            profiling_enabled.then_some(&mut iteration_timings),
        )?;
        lazy_skipping = false;
        if let Some(iteration_start) = iteration_start {
            plan.planning_profile
                .record_iteration(iteration_start.elapsed(), iteration_timings);
        }
    }

    debug_assert!(row_dict_match_state_offsets_synced(
        repeat_offsets,
        offset_1,
        offset_2
    ));
    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

fn plan_sequences_chain_ext_dict_with_prefix_chain_from_into_core<const PROFILE: bool>(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    finder: &mut MatchFinder,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let mut repeat_offsets = repeat_offsets;
    let (mut offset_1, mut offset_2) = repeat_offsets12(repeat_offsets);
    let depth = params.lazy_search_depth.min(2);
    let mut anchor = block_start;
    let mut pos = block_start + usize::from(block_start == 0);
    let mut lazy_skipping = false;
    let search_limit = lazy_search_limit(src.len());

    while pos < search_limit {
        let (prefix_low, source_low) = match_floor.at(pos);
        let iteration_start = PROFILE.then(Instant::now);
        let mut iteration_timings = PlanningIterationTimings::default();

        let baseline_rep_start = PROFILE.then(Instant::now);
        let baseline_rep_snapshot =
            PlanningIterationCategorySnapshot::capture(PROFILE.then_some(&iteration_timings));
        let rep_start = PROFILE.then(Instant::now);
        let rep = repeat_ahead_match_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            offset_1,
            prefix_low,
            source_low,
        );
        if let Some(rep_start) = rep_start {
            iteration_timings.rep_check += rep_start.elapsed();
        }
        let mut choice = rep.map(|candidate| ChainLazyParserChoice::repeat(pos + 1, candidate));
        if PROFILE {
            record_lazy_parser_phase(
                Some(&mut iteration_timings),
                LazyParserPhase::BaselineRep,
                baseline_rep_start,
                baseline_rep_snapshot,
            );
        }

        let current_before_regular = choice.map(|choice| choice.best);
        let current_source_before_regular = choice
            .map(|choice| choice.source)
            .unwrap_or(SequenceTraceMatchSource::Unknown);
        let baseline_regular_start = PROFILE.then(Instant::now);
        let baseline_regular_snapshot =
            PlanningIterationCategorySnapshot::capture(PROFILE.then_some(&iteration_timings));
        // C's Greedy (depth==0) takes the rep match at ip+1 immediately via
        // `goto _storeSequence`, skipping the chain search. Rust's chain walk
        // often finds a longer regular match at pos but with a larger offset,
        // which costs more bits than the repcode — the raw-dict L4 ratio
        // regression comes from exactly this. Skip the chain search when we
        // already have a rep candidate and depth==0, matching C.
        let regular_search = if depth == 0 && choice.is_some() {
            ChainRegularMatchSearch::default()
        } else {
            best_chain_ext_dict_regular_match_core::<PROFILE>(
                prefix_chain,
                src,
                pos,
                params,
                prefix_low,
                source_low,
                finder,
                lazy_skipping,
                PROFILE.then_some(&mut iteration_timings),
            )
        };
        if let Some(candidate) = regular_search
            .candidate
            .filter(|candidate| choice.is_none_or(|choice| candidate.length > choice.best.length))
        {
            choice = Some(ChainLazyParserChoice::regular(
                pos,
                regular_search.source,
                candidate,
            ));
        }
        if PROFILE {
            record_lazy_parser_phase(
                Some(&mut iteration_timings),
                LazyParserPhase::BaselineRegular,
                baseline_regular_start,
                baseline_regular_snapshot,
            );
        }

        let Some(mut choice) = choice else {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = next_pos;
            if let Some(iteration_start) = iteration_start {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        };
        trace_chain_search(
            plan,
            anchor,
            pos,
            0,
            offset_1,
            offset_2,
            current_before_regular.unwrap_or(choice.best),
            current_source_before_regular,
            rep.map_or(0, |candidate| candidate.length),
            regular_search,
            choice.best,
            choice.source,
        );

        if depth >= 1 {
            let mut probe_pos = pos;
            loop {
                let depth1_pos = probe_pos + 1;
                if depth1_pos > search_limit {
                    break;
                }
                // The floor moves with the probe, as it does in C.
                let (prefix_low, source_low) = match_floor.at(depth1_pos);

                let continue_start = PROFILE.then(Instant::now);
                let continue_snapshot = PlanningIterationCategorySnapshot::capture(
                    PROFILE.then_some(&iteration_timings),
                );
                let rep_start = PROFILE.then(Instant::now);
                let rep = repeat_match_with_prefix_chain_at(
                    prefix_chain,
                    src,
                    depth1_pos,
                    offset_1,
                    prefix_low,
                    source_low,
                );
                if let Some(rep_start) = rep_start {
                    iteration_timings.rep_check += rep_start.elapsed();
                }
                let current_before_regular = choice.best;
                let current_source_before_regular = choice.source;
                if let Some(candidate) =
                    rep.filter(|candidate| lazy_repeat_match_improves(choice.best, *candidate, 3))
                {
                    choice = ChainLazyParserChoice::repeat(depth1_pos, candidate);
                }
                let regular_search = best_chain_ext_dict_regular_match_core::<PROFILE>(
                    prefix_chain,
                    src,
                    depth1_pos,
                    params,
                    prefix_low,
                    source_low,
                    finder,
                    lazy_skipping,
                    PROFILE.then_some(&mut iteration_timings),
                );
                let regular_improved = if let Some(candidate) = regular_search
                    .candidate
                    .filter(|candidate| lazy_regular_match_improves(choice.best, *candidate, 4))
                {
                    choice = ChainLazyParserChoice::regular(
                        depth1_pos,
                        regular_search.source,
                        candidate,
                    );
                    true
                } else {
                    false
                };
                trace_chain_search(
                    plan,
                    anchor,
                    depth1_pos,
                    1,
                    offset_1,
                    offset_2,
                    current_before_regular,
                    current_source_before_regular,
                    rep.map_or(0, |candidate| candidate.length),
                    regular_search,
                    choice.best,
                    choice.source,
                );
                if PROFILE {
                    record_lazy_parser_phase(
                        Some(&mut iteration_timings),
                        LazyParserPhase::Continue,
                        continue_start,
                        continue_snapshot,
                    );
                }
                if regular_improved {
                    probe_pos = depth1_pos;
                    continue;
                }

                if depth == 2 {
                    let depth2_pos = depth1_pos + 1;
                    if depth2_pos > search_limit {
                        break;
                    }
                    let (prefix_low, source_low) = match_floor.at(depth2_pos);

                    let continue_start = PROFILE.then(Instant::now);
                    let continue_snapshot = PlanningIterationCategorySnapshot::capture(
                        PROFILE.then_some(&iteration_timings),
                    );
                    let rep_start = PROFILE.then(Instant::now);
                    let rep = repeat_match_with_prefix_chain_at(
                        prefix_chain,
                        src,
                        depth2_pos,
                        offset_1,
                        prefix_low,
                        source_low,
                    );
                    if let Some(rep_start) = rep_start {
                        iteration_timings.rep_check += rep_start.elapsed();
                    }
                    let current_before_regular = choice.best;
                    let current_source_before_regular = choice.source;
                    if let Some(candidate) = rep
                        .filter(|candidate| lazy_repeat_match_improves(choice.best, *candidate, 4))
                    {
                        choice = ChainLazyParserChoice::repeat(depth2_pos, candidate);
                    }
                    let regular_search = best_chain_ext_dict_regular_match_core::<PROFILE>(
                        prefix_chain,
                        src,
                        depth2_pos,
                        params,
                        prefix_low,
                        source_low,
                        finder,
                        lazy_skipping,
                        PROFILE.then_some(&mut iteration_timings),
                    );
                    let regular_improved = if let Some(candidate) = regular_search
                        .candidate
                        .filter(|candidate| lazy_regular_match_improves(choice.best, *candidate, 7))
                    {
                        choice = ChainLazyParserChoice::regular(
                            depth2_pos,
                            regular_search.source,
                            candidate,
                        );
                        true
                    } else {
                        false
                    };
                    trace_chain_search(
                        plan,
                        anchor,
                        depth2_pos,
                        2,
                        offset_1,
                        offset_2,
                        current_before_regular,
                        current_source_before_regular,
                        rep.map_or(0, |candidate| candidate.length),
                        regular_search,
                        choice.best,
                        choice.source,
                    );
                    if PROFILE {
                        record_lazy_parser_phase(
                            Some(&mut iteration_timings),
                            LazyParserPhase::Continue,
                            continue_start,
                            continue_snapshot,
                        );
                    }
                    if regular_improved {
                        probe_pos = depth2_pos;
                        continue;
                    }
                }

                break;
            }
        }

        let accepted_match = match choice.best.kind {
            LazyMatchKind::Repeat1 => ExtendedMatch {
                start: choice.best.start,
                offset: choice.best.offset,
                length: choice.best.length,
            },
            LazyMatchKind::Regular => extend_back_prefix_chain_match(
                prefix_chain,
                src,
                anchor,
                choice.best.start,
                choice.best.candidate(),
                prefix_low,
                source_low,
            ),
        };
        let literal_length = accepted_match.start.saturating_sub(anchor);
        if !should_accept_match(
            MatchCandidate {
                offset: accepted_match.offset,
                length: accepted_match.length,
            },
            literal_length,
            params,
        ) {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = next_pos;
            if let Some(iteration_start) = iteration_start {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        }

        pos = chain_prefixed_store_match_and_chain_rep2_core::<PROFILE>(
            plan,
            prefix_chain,
            src,
            &mut anchor,
            &mut repeat_offsets,
            &mut offset_1,
            &mut offset_2,
            choice.best,
            choice.source,
            match_floor,
            search_limit,
            PROFILE.then_some(&mut iteration_timings),
        )?;
        lazy_skipping = false;
        if let Some(iteration_start) = iteration_start {
            plan.planning_profile
                .record_iteration(iteration_start.elapsed(), iteration_timings);
        }
    }

    debug_assert!(row_dict_match_state_offsets_synced(
        repeat_offsets,
        offset_1,
        offset_2
    ));
    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

pub(crate) fn plan_sequences_chain_ext_dict_with_prefix_chain_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    finder: &mut MatchFinder,
) -> Result<()> {
    if plan.planning_profile.is_enabled() {
        plan_sequences_chain_ext_dict_with_prefix_chain_from_into_core::<true>(
            plan,
            src,
            block_start,
            prefix_chain,
            repeat_offsets,
            params,
            match_floor,
            finder,
        )
    } else {
        plan_sequences_chain_ext_dict_with_prefix_chain_from_into_core::<false>(
            plan,
            src,
            block_start,
            prefix_chain,
            repeat_offsets,
            params,
            match_floor,
            finder,
        )
    }
}

#[allow(dead_code)]
pub(crate) fn plan_sequences_without_prefix(
    src: &[u8],
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
) -> Result<SequencePlan> {
    let mut plan = SequencePlan::default();
    plan_sequences_without_prefix_into(&mut plan, src, repeat_offsets, params)?;
    Ok(plan)
}

pub(crate) fn plan_sequences_without_prefix_into(
    plan: &mut SequencePlan,
    src: &[u8],
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
) -> Result<()> {
    if params.parser_strategy.is_row_hash() {
        let mut finder = RowHashFinder::new(params.hash_bits, params.search_log, params.min_match);
        return plan_sequences_row_without_prefix_from_into(
            plan,
            src,
            0,
            repeat_offsets,
            params,
            MatchFloor::fixed(0),
            &mut finder,
        );
    }

    let mut finder = MatchFinder::with_chain_log(
        src.len(),
        params.hash_bits,
        params.chain_log,
        params.min_match,
    );
    plan_sequences_without_prefix_from_into(
        plan,
        src,
        0,
        repeat_offsets,
        params,
        MatchFloor::fixed(0),
        &mut finder,
    )
}

#[allow(dead_code)]
pub(crate) fn plan_sequences_without_prefix_from(
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut impl LazySearchFinder,
) -> Result<SeqStore> {
    let mut plan = SequencePlan::default();
    plan_sequences_without_prefix_from_into(
        &mut plan,
        src,
        block_start,
        repeat_offsets,
        params,
        match_floor,
        finder,
    )?;
    Ok(plan)
}

pub(crate) fn plan_sequences_without_prefix_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut impl LazySearchFinder,
) -> Result<()> {
    if matches!(
        params.parser_strategy,
        ParserStrategy::Greedy
            | ParserStrategy::Lazy
            | ParserStrategy::Lazy2
            | ParserStrategy::BinaryTreeLazy2
    ) {
        return plan_sequences_lazy_without_prefix_from_into(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
        );
    }

    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let mut repeat_offsets = repeat_offsets;
    let mut anchor = block_start;
    let mut pos = block_start;

    while pos + MIN_MATCH <= src.len() {
        let window_low = match_floor.at(pos);
        let literal_length = pos - anchor;
        let Some(candidate) = best_match_without_prefix(
            src,
            pos,
            repeat_offsets.values(),
            literal_length,
            params,
            finder,
            window_low,
        )
        .filter(|candidate| should_accept_match(*candidate, literal_length, params)) else {
            finder.insert(src, pos);
            pos += skip_after_no_match(anchor, pos, params);
            continue;
        };

        let lazy = find_lazy_match_skip_without_prefix(
            src,
            pos,
            anchor,
            repeat_offsets.values(),
            candidate,
            params,
            finder,
            match_floor,
        );
        if lazy.skip != 0 {
            pos += lazy.skip;
            continue;
        }

        let found = extend_back_source_candidate(src, anchor, pos, candidate, window_low);
        store_lazy_sequence_with_source(
            plan,
            src,
            &mut anchor,
            &mut repeat_offsets,
            found.start,
            found.offset,
            found.length,
            SequenceTraceMatchSource::Source,
        )?;

        let insert_start = pos + lazy.inserted;
        pos = anchor;
        finder.insert_range(src, insert_start, anchor);
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

#[derive(Debug, Clone)]
pub(crate) struct RowHashFinder {
    pub(crate) hash_table: Vec<u32>,
    pub(crate) tag_table: Vec<u8>,
    pub(crate) hash_cache: [u32; ROW_HASH_CACHE_SIZE],
    pub(crate) row_log: u32,
    pub(crate) row_hash_log: u32,
    pub(crate) min_match: u32,
    pub(crate) next_to_update: usize,
    pub(crate) hash_salt: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowHashContext {
    pub(crate) total_hash_bits: u32,
    pub(crate) row_entries: usize,
    pub(crate) row_mask: usize,
    pub(crate) group_width: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RowHashLocation {
    pub(crate) rel_row: usize,
    pub(crate) tag: u8,
    pub(crate) head_index: usize,
    pub(crate) head_grouped: usize,
    pub(crate) insert_index: usize,
}

impl RowHashFinder {
    #[inline]
    fn row_rel_row_and_tag(&self, hash: usize) -> (usize, u8) {
        let rel_row = (hash >> ROW_HASH_TAG_BITS) << self.row_log;
        let tag = (hash & ((1usize << ROW_HASH_TAG_BITS) - 1)) as u8;
        (rel_row, tag)
    }

    #[inline(always)]
    #[allow(unsafe_code)]
    fn insert_row_position(
        &mut self,
        rel_row: usize,
        tag: u8,
        pos: usize,
        context: RowHashContext,
    ) {
        debug_assert!(rel_row + context.row_entries <= self.tag_table.len());
        debug_assert!(rel_row + context.row_entries <= self.hash_table.len());
        let tag_row = &mut self.tag_table[rel_row..rel_row + context.row_entries];
        let next = row_next_index(tag_row, context.row_mask);
        // SAFETY: next < row_entries (guaranteed by row_next_index using row_mask),
        // and rel_row + row_entries is within bounds (checked above in debug).
        unsafe {
            *self.tag_table.as_mut_ptr().add(rel_row + next) = tag;
            *self.hash_table.as_mut_ptr().add(rel_row + next) = pos as u32;
        }
    }

    #[inline]
    fn insert_hash_with_context(&mut self, hash: usize, pos: usize, context: RowHashContext) {
        let (rel_row, tag) = self.row_rel_row_and_tag(hash);
        self.insert_row_position(rel_row, tag, pos, context);
    }

    pub(crate) fn new(hash_bits: u32, search_log: u32, min_match: u32) -> Self {
        let hash_bits = hash_bits.clamp(10, MAX_MATCH_HASH_BITS);
        let row_log = search_log.clamp(4, 6);
        let row_hash_log = hash_bits.saturating_sub(row_log);
        Self {
            hash_table: vec![0; 1usize << hash_bits],
            tag_table: vec![0; 1usize << hash_bits],
            hash_cache: [0; ROW_HASH_CACHE_SIZE],
            row_log,
            row_hash_log,
            min_match: min_match.clamp(4, 6),
            next_to_update: 0,
            // Matches the first CCtx row-hash salt produced by ZSTD_advanceHashSalt().
            hash_salt: DEFAULT_ROW_HASH_SALT,
        }
    }

    pub(crate) fn row_entries(&self) -> usize {
        1usize << self.row_log
    }

    pub(crate) fn row_mask(&self) -> usize {
        self.row_entries() - 1
    }

    pub(crate) fn total_hash_bits(&self) -> u32 {
        self.row_hash_log + ROW_HASH_TAG_BITS
    }

    #[inline(always)]
    pub(crate) fn row_context(&self) -> RowHashContext {
        let row_entries = self.row_entries();
        RowHashContext {
            total_hash_bits: self.total_hash_bits(),
            row_entries,
            row_mask: row_entries - 1,
            group_width: row_match_mask_group_width(row_entries),
        }
    }

    #[inline(always)]
    pub(crate) fn row_location_from_hash(
        &self,
        hash: usize,
        context: RowHashContext,
    ) -> RowHashLocation {
        let (rel_row, tag) = self.row_rel_row_and_tag(hash);
        let head_index = usize::from(self.tag_table[rel_row] & context.row_mask as u8);
        let head_grouped = head_index * context.group_width;
        RowHashLocation {
            rel_row,
            tag,
            head_index,
            head_grouped,
            insert_index: row_next_insert_index(head_index, context.row_mask),
        }
    }

    pub(crate) fn insert_prefix(&mut self, prefix: &[u8]) {
        let end = row_insert_end(prefix.len());
        let context = self.row_context();
        for pos in self.next_to_update.min(end)..end {
            let hash = row_hash_prefix_at(
                prefix,
                pos,
                context.total_hash_bits,
                self.min_match,
                self.hash_salt,
            );
            self.insert_hash_with_context(hash, pos, context);
        }
        self.next_to_update = end;
    }

    pub(crate) fn insert_prefix_chain(&mut self, prefix_chain: PrefixChain<'_>, src: &[u8]) {
        let end = prefix_chain.len().min(
            prefix_chain
                .len()
                .saturating_add(src.len())
                .saturating_sub(8)
                + 1,
        );
        let context = self.row_context();
        for pos in self.next_to_update.min(end)..end {
            let hash = row_hash_prefix_chain_at(
                prefix_chain,
                src,
                pos,
                context.total_hash_bits,
                self.min_match,
                self.hash_salt,
            );
            self.insert_hash_with_context(hash, pos, context);
        }
        self.next_to_update = end;
    }

    pub(crate) fn reset(&mut self) {
        // Rotate the hash salt instead of zeroing the tag table, matching
        // C's ZSTD_advanceHashSalt().  Old tags become invalid because they
        // were computed with the previous salt, so almost every stale entry
        // stops matching.  This avoids zeroing ~640KB of hash + tag tables
        // per frame.
        //
        // Almost, not all: tags are a few bits, so a stale entry can collide
        // with the new salt's tag and be returned as a candidate. The position
        // it carries belongs to the *previous* frame and can be anywhere,
        // including past the end of the current source.
        //
        // This used to say those entries were caught by the `window_low`
        // check in the search. They are not. `window_low` is a lower bound,
        // and the failure is an upper-bound one: a position from a longer
        // previous frame is too *large*, which no lower bound rejects. A 1 MiB
        // frame followed by a 128 KiB one on the same `Encoder` produced
        // candidate indices up to 995424 against a 131072-byte source, and
        // both the prefetch and `count_match_length_unchecked` take those
        // unchecked. `row_collect_match_indices_no_positions` now rejects any
        // entry at or beyond the position being searched, which is what makes
        // keeping the table across frames sound rather than merely lucky.
        self.hash_salt = bitmix(self.hash_salt, 8) ^ bitmix(self.hash_salt.wrapping_add(1), 4);
        self.hash_cache = [0; ROW_HASH_CACHE_SIZE];
        self.next_to_update = 0;
    }

    /// Rebase every filed position by `delta`, so the table keeps describing
    /// the same bytes after the first `delta` of them are dropped.
    ///
    /// `hash_table` is the only position-bearing table here: `tag_table` holds
    /// each row's insert cursor and the tags, and `hash_cache` holds hashes,
    /// none of which move with the buffer. Emptied entries take zero, the value
    /// the table is built with, which the search treats as a candidate at
    /// position zero and rejects on the byte comparison.
    pub(crate) fn shift_positions(&mut self, delta: usize) {
        shift_raw_positions(&mut self.hash_table, delta, 0);
        self.next_to_update = self.next_to_update.saturating_sub(delta);
    }

    pub(crate) fn insert_range(&mut self, src: &[u8], start: usize, end: usize) {
        let end = end.min(row_insert_end(src.len()));
        let start = start.max(self.next_to_update).min(end);
        let context = self.row_context();
        for pos in start..end {
            let hash = row_hash_src_at(
                src,
                pos,
                context.total_hash_bits,
                self.min_match,
                self.hash_salt,
            );
            self.insert_hash_with_context(hash, pos, context);
        }
        self.next_to_update = end;
    }

    /// Prefetch the hash-table and tag-table rows for a given row offset into
    /// L1 cache. Matches C zstd's ZSTD_row_prefetch which prefetches 1-2 cache
    /// lines depending on row size.
    #[allow(unsafe_code)]
    #[inline(always)]
    fn row_prefetch(&self, rel_row: usize) {
        unsafe {
            crate::entropy::mem::prefetch_l1(self.hash_table.as_ptr().add(rel_row) as *const u8);
            crate::entropy::mem::prefetch_l1(self.tag_table.as_ptr().add(rel_row));
            // For large rows (row_log >= 5, i.e. 32+ entries per row), the row
            // spans two cache lines; prefetch the second half.
            if self.row_log >= 5 {
                crate::entropy::mem::prefetch_l1(
                    self.hash_table.as_ptr().add(rel_row + 16) as *const u8
                );
            }
        }
    }

    pub(crate) fn refill_hash_cache<const MLS: u32>(&mut self, src: &[u8], search_limit: usize) {
        let context = self.row_context();
        self.fill_hash_cache_from::<MLS>(src, self.next_to_update, search_limit, context);
    }

    pub(crate) fn fill_hash_cache_from<const MLS: u32>(
        &mut self,
        src: &[u8],
        start: usize,
        search_limit: usize,
        context: RowHashContext,
    ) {
        if start > search_limit {
            return;
        }
        let limit = start + ROW_HASH_CACHE_SIZE.min(search_limit - start + 1);
        for idx in start..limit {
            let hash =
                row_hash_src_at_const::<MLS>(src, idx, context.total_hash_bits, self.hash_salt)
                    as u32;
            // Prefetch the hash/tag table row this hash maps to, so it is in
            // L1 cache by the time the search function accesses it.
            let rel_row = ((hash as usize) >> ROW_HASH_TAG_BITS) << self.row_log;
            self.row_prefetch(rel_row);
            self.hash_cache[idx & (ROW_HASH_CACHE_SIZE - 1)] = hash;
        }
    }

    #[inline(always)]
    pub(crate) fn update_internal_with_context<const MLS: u32>(
        &mut self,
        src: &[u8],
        target: usize,
        use_cache: bool,
        context: RowHashContext,
    ) {
        let mut idx = self.next_to_update.min(target);
        if idx == target {
            self.next_to_update = target;
            return;
        }
        if use_cache {
            const SKIP_THRESHOLD: usize = 384;
            const MAX_MATCH_START_POSITIONS_TO_UPDATE: usize = 96;
            const MAX_MATCH_END_POSITIONS_TO_UPDATE: usize = 32;
            if target.saturating_sub(idx) > SKIP_THRESHOLD {
                let bound = idx + MAX_MATCH_START_POSITIONS_TO_UPDATE;
                self.update_internal_cached_range::<MLS>(src, idx, bound.min(target), context);
                idx = target.saturating_sub(MAX_MATCH_END_POSITIONS_TO_UPDATE);
                self.fill_hash_cache_from::<MLS>(src, idx, target.saturating_add(1), context);
            }
            self.update_internal_cached_range::<MLS>(src, idx, target, context);
        } else {
            self.update_internal_uncached_range::<MLS>(src, idx, target, context);
        }
        self.next_to_update = target;
        debug_assert_eq!(context.row_mask, self.row_mask());
    }

    #[inline(always)]
    fn update_internal_cached_range<const MLS: u32>(
        &mut self,
        src: &[u8],
        start: usize,
        end: usize,
        context: RowHashContext,
    ) {
        let total_hash_bits = context.total_hash_bits;
        let row_mask = context.row_mask;
        let row_log = self.row_log;
        let hash_salt = self.hash_salt;
        let count = end.saturating_sub(start);
        let pairs = count / 2;
        let mut idx = start;

        // Process 2 positions per iteration to interleave memory latency
        // between hash lookups and row inserts.
        for _ in 0..pairs {
            let ci0 = idx & (ROW_HASH_CACHE_SIZE - 1);
            let ci1 = (idx + 1) & (ROW_HASH_CACHE_SIZE - 1);

            // Compute next hashes for both positions.
            let nh0 = row_hash_src_at_const::<MLS>(
                src,
                idx + ROW_HASH_CACHE_SIZE,
                total_hash_bits,
                hash_salt,
            ) as u32;
            let nh1 = row_hash_src_at_const::<MLS>(
                src,
                idx + 1 + ROW_HASH_CACHE_SIZE,
                total_hash_bits,
                hash_salt,
            ) as u32;

            // Prefetch both rows.
            let nr0 = ((nh0 as usize) >> ROW_HASH_TAG_BITS) << row_log;
            let nr1 = ((nh1 as usize) >> ROW_HASH_TAG_BITS) << row_log;
            self.row_prefetch(nr0);
            self.row_prefetch(nr1);

            // Insert first position — use raw pointers to avoid 6 bounds checks
            // per iteration. Safety: rr0/rr1 are masked to table size, n0/n1 < row_entries.
            let h0 = core::mem::replace(&mut self.hash_cache[ci0], nh0) as usize;
            let (rr0, tag0) = self.row_rel_row_and_tag(h0);
            #[allow(unsafe_code)]
            unsafe {
                let tag_ptr = self.tag_table.as_mut_ptr();
                let hash_ptr = self.hash_table.as_mut_ptr();
                let n0 = row_next_insert_index(usize::from(*tag_ptr.add(rr0)), row_mask);
                *tag_ptr.add(rr0) = n0 as u8;
                *tag_ptr.add(rr0 + n0) = tag0;
                *hash_ptr.add(rr0 + n0) = idx as u32;

                // Insert second position.
                let h1 = core::mem::replace(&mut self.hash_cache[ci1], nh1) as usize;
                let (rr1, tag1) = self.row_rel_row_and_tag(h1);
                let n1 = row_next_insert_index(usize::from(*tag_ptr.add(rr1)), row_mask);
                *tag_ptr.add(rr1) = n1 as u8;
                *tag_ptr.add(rr1 + n1) = tag1;
                *hash_ptr.add(rr1 + n1) = (idx + 1) as u32;
            }

            idx += 2;
        }

        // Handle odd remainder.
        if idx < end {
            let cache_index = idx & (ROW_HASH_CACHE_SIZE - 1);
            let next_hash = row_hash_src_at_const::<MLS>(
                src,
                idx + ROW_HASH_CACHE_SIZE,
                total_hash_bits,
                hash_salt,
            ) as u32;
            let next_rel_row = ((next_hash as usize) >> ROW_HASH_TAG_BITS) << row_log;
            self.row_prefetch(next_rel_row);
            let hash = core::mem::replace(&mut self.hash_cache[cache_index], next_hash) as usize;
            let (rel_row, tag) = self.row_rel_row_and_tag(hash);
            let next = row_next_insert_index(usize::from(self.tag_table[rel_row]), row_mask);
            self.tag_table[rel_row] = next as u8;
            self.tag_table[rel_row + next] = tag;
            self.hash_table[rel_row + next] = idx as u32;
        }
    }

    #[inline(always)]
    fn update_internal_uncached_range<const MLS: u32>(
        &mut self,
        src: &[u8],
        start: usize,
        end: usize,
        context: RowHashContext,
    ) {
        let total_hash_bits = context.total_hash_bits;
        let row_mask = context.row_mask;
        for idx in start..end {
            let hash = row_hash_src_at_const::<MLS>(src, idx, total_hash_bits, self.hash_salt);
            let (rel_row, tag) = self.row_rel_row_and_tag(hash);
            let next = row_next_insert_index(usize::from(self.tag_table[rel_row]), row_mask);
            self.tag_table[rel_row] = next as u8;
            self.tag_table[rel_row + next] = tag;
            self.hash_table[rel_row + next] = idx as u32;
        }
    }

    #[inline(always)]
    pub(crate) fn next_cached_hash<const MLS: u32>(
        &mut self,
        src: &[u8],
        idx: usize,
        context: RowHashContext,
    ) -> u32 {
        let next_hash = row_hash_src_at_const::<MLS>(
            src,
            idx + ROW_HASH_CACHE_SIZE,
            context.total_hash_bits,
            self.hash_salt,
        ) as u32;
        let cache_index = idx & (ROW_HASH_CACHE_SIZE - 1);
        let hash = self.hash_cache[cache_index];
        self.hash_cache[cache_index] = next_hash;
        hash
    }

    #[inline(always)]
    pub(crate) fn search_attempt_budget(&self, search_log: u32) -> usize {
        1usize << search_log.min(self.row_log)
    }

    /// Fast production match finder — no TRACE, no PROFILE overhead.
    /// Returns (best_match, remaining_attempts). Only generic on MLS.
    #[inline(always)]
    fn find_source_match_fast<const MLS: u32>(
        &mut self,
        src: &[u8],
        pos: usize,
        window_low: usize,
        lazy_skipping: bool,
        attempts: usize,
    ) -> (MatchCandidate, usize) {
        if pos + 8 > src.len() {
            return (
                MatchCandidate {
                    offset: 0,
                    length: 0,
                },
                attempts,
            );
        }

        let context = self.row_context();
        let hash = if lazy_skipping {
            self.next_to_update = pos;
            row_hash_src_at_const::<MLS>(src, pos, context.total_hash_bits, self.hash_salt)
        } else {
            self.update_internal_with_context::<MLS>(src, pos, true, context);
            self.next_cached_hash::<MLS>(src, pos, context) as usize
        };
        let location = self.row_location_from_hash(hash, context);
        let mut match_buffer = [NO_POS; ROW_HASH_MAX_ENTRIES];
        // SAFETY: rel_row + row_entries is within the hash/tag table bounds
        // (guaranteed by row_location_from_hash using the row mask).
        #[allow(unsafe_code)]
        let (hash_row, tag_row) = unsafe {
            let rr = location.rel_row;
            let re = context.row_entries;
            debug_assert!(rr + re <= self.hash_table.len());
            debug_assert!(rr + re <= self.tag_table.len());
            (
                core::slice::from_raw_parts(self.hash_table.as_ptr().add(rr), re),
                core::slice::from_raw_parts(self.tag_table.as_ptr().add(rr), re),
            )
        };
        let (num_matches, attempts_left) = row_collect_match_indices_no_positions(
            hash_row,
            tag_row,
            location,
            context,
            window_low,
            pos,
            attempts,
            &mut match_buffer,
            src.as_ptr(),
        );
        self.insert_hashed_position_with_location(location, pos, context);
        self.next_to_update = pos + 1;
        let mut ml = MIN_MATCH - 1;
        let mut best_off_base = 0usize;
        for &match_index_u32 in &match_buffer[..num_matches] {
            let match_index = match_index_u32 as usize;
            debug_assert!(match_index < pos, "hash table entry should be < search pos");
            let gate_offset = ml - (MIN_MATCH - 1);
            if !source_match_passes_gate_at_offset(src, match_index, pos, gate_offset) {
                continue;
            }
            // SAFETY: match_index < pos (guaranteed by hash table construction:
            // entries are populated at positions < current search position, and
            // low_limit filtering removes stale entries).
            // pos < src.len() (guaranteed by pos + 8 <= src.len() at function entry).
            #[allow(unsafe_code)]
            let current_ml = unsafe { count_match_length_unchecked(src, match_index, pos) };
            if current_ml > ml {
                ml = current_ml;
                best_off_base = explicit_offbase(pos - match_index) as usize;
                if pos + current_ml == src.len() {
                    break;
                }
            }
        }
        // ml < MIN_MATCH means no match found (C convention: matchLength < 4).
        let best = MatchCandidate {
            offset: if ml >= MIN_MATCH {
                best_off_base - (MIN_MATCH - 1)
            } else {
                0
            },
            length: ml,
        };
        (best, attempts_left)
    }

    fn find_source_match_with_budget_core<
        const TRACE: bool,
        const PROFILE: bool,
        const MLS: u32,
    >(
        &mut self,
        src: &[u8],
        pos: usize,
        window_low: usize,
        lazy_skipping: bool,
        attempts: usize,
        timings: Option<&mut PlanningIterationTimings>,
    ) -> (Option<MatchCandidate>, usize, RowMatchBufferTrace) {
        // Fast path: no tracing or profiling — delegate to lean function.
        if !TRACE && !PROFILE {
            let (best, attempts_left) =
                self.find_source_match_fast::<MLS>(src, pos, window_low, lazy_skipping, attempts);
            let opt = (best.length >= MIN_MATCH).then_some(best);
            return (opt, attempts_left, RowMatchBufferTrace::default());
        }

        if pos + 8 > src.len() {
            return (None, attempts, RowMatchBufferTrace::default());
        }

        let next_to_update_before_search = self.next_to_update;
        let context = self.row_context();
        let mut timings = timings;
        let insert_start = PROFILE.then(Instant::now);
        let hash = if lazy_skipping {
            self.next_to_update = pos;
            row_hash_src_at_const::<MLS>(src, pos, context.total_hash_bits, self.hash_salt)
        } else {
            self.update_internal_with_context::<MLS>(src, pos, true, context);
            self.next_cached_hash::<MLS>(src, pos, context) as usize
        };
        if PROFILE {
            timings.as_mut().unwrap().insert_update += insert_start.unwrap().elapsed();
        }
        let location = self.row_location_from_hash(hash, context);
        if !TRACE {
            let mut match_buffer = [NO_POS; ROW_HASH_MAX_ENTRIES];
            let row_search_start = PROFILE.then(Instant::now);
            let (num_matches, attempts_left) = row_collect_match_indices_no_positions(
                &self.hash_table[location.rel_row..location.rel_row + context.row_entries],
                &self.tag_table[location.rel_row..location.rel_row + context.row_entries],
                location,
                context,
                window_low,
                pos,
                attempts,
                &mut match_buffer,
                src.as_ptr(),
            );
            if PROFILE {
                timings.as_mut().unwrap().row_search += row_search_start.unwrap().elapsed();
            }
            let insert_start = PROFILE.then(Instant::now);
            self.insert_hashed_position_with_location(location, pos, context);
            self.next_to_update = pos + 1;
            if PROFILE {
                timings.as_mut().unwrap().insert_update += insert_start.unwrap().elapsed();
            }
            let mut ml = MIN_MATCH - 1;
            let mut best_off_base = 0usize;
            for &match_index_u32 in &match_buffer[..num_matches] {
                let match_index = match_index_u32 as usize;
                if match_index >= pos {
                    continue;
                }
                let gate_offset = ml - (MIN_MATCH - 1);
                if !source_match_passes_gate_at_offset(src, match_index, pos, gate_offset) {
                    continue;
                }
                let match_count_start = PROFILE.then(Instant::now);
                let current_ml = count_match_length(src, match_index, pos);
                if PROFILE {
                    timings.as_mut().unwrap().match_count += match_count_start.unwrap().elapsed();
                }
                if current_ml > ml {
                    ml = current_ml;
                    best_off_base = explicit_offbase(pos - match_index) as usize;
                    if pos + current_ml == src.len() {
                        break;
                    }
                }
            }
            let best = (ml >= MIN_MATCH).then_some(MatchCandidate {
                offset: best_off_base.saturating_sub(MIN_MATCH - 1),
                length: ml,
            });
            return (best, attempts_left, RowMatchBufferTrace::default());
        }

        let mut match_buffer = [NO_POS; ROW_HASH_MAX_ENTRIES];
        let row_search_start = PROFILE.then(Instant::now);
        let (num_matches, attempts_left, mut trace) = {
            let row = &self.hash_table[location.rel_row..location.rel_row + context.row_entries];
            let tag_row = &self.tag_table[location.rel_row..location.rel_row + context.row_entries];
            let mut match_positions = [0usize; ROW_HASH_MAX_ENTRIES];
            let (num_matches, attempts_left) = row_collect_match_indices_with_positions(
                row,
                tag_row,
                location,
                context,
                window_low,
                pos,
                attempts,
                &mut match_positions,
                &mut match_buffer,
                src.as_ptr(),
            );
            let mut trace = RowMatchBufferTrace {
                next_to_update_before_search,
                hash,
                rel_row: location.rel_row,
                tag: location.tag,
                low_limit: window_low,
                attempt_budget: attempts,
                head_index: location.head_index,
                insert_index: location.insert_index,
                group_width: context.group_width,
                num_matches,
                ..RowMatchBufferTrace::default()
            };
            for (index, &match_index) in match_buffer[..num_matches.min(trace.match_indices.len())]
                .iter()
                .enumerate()
            {
                trace.match_positions[index] = match_positions[index];
                trace.match_indices[index] = match_index as usize;
            }
            (num_matches, attempts_left, trace)
        };
        if PROFILE {
            timings.as_mut().unwrap().row_search += row_search_start.unwrap().elapsed();
        }

        let insert_start = PROFILE.then(Instant::now);
        self.insert_hashed_position_with_location(location, pos, context);
        self.next_to_update = pos + 1;
        if PROFILE {
            timings.as_mut().unwrap().insert_update += insert_start.unwrap().elapsed();
        }
        let mut ml = MIN_MATCH - 1;
        let mut best_off_base = 0usize;
        for (visit_index, &match_index_u32) in match_buffer[..num_matches].iter().enumerate() {
            let match_index = match_index_u32 as usize;
            let gate_offset = ml - (MIN_MATCH - 1);
            let gate_passed = match_index < pos
                && source_match_passes_gate_at_offset(src, match_index, pos, gate_offset);
            let mut current_ml = 0usize;
            if gate_passed {
                let match_count_start = PROFILE.then(Instant::now);
                current_ml = count_match_length(src, match_index, pos);
                if PROFILE {
                    timings.as_mut().unwrap().match_count += match_count_start.unwrap().elapsed();
                }
            }
            if current_ml > ml {
                ml = current_ml;
                best_off_base = explicit_offbase(pos - match_index) as usize;
                if pos + current_ml == src.len() {
                    if TRACE && trace.visit_count < trace.visit_indices.len() {
                        trace.visit_positions[trace.visit_count] =
                            trace.match_positions[visit_index];
                        trace.visit_indices[trace.visit_count] = match_index;
                        trace.visit_gate_passes[trace.visit_count] = gate_passed;
                        trace.visit_lengths[trace.visit_count] = current_ml;
                        trace.visit_winner_lengths[trace.visit_count] = ml;
                        trace.visit_winner_off_bases[trace.visit_count] = best_off_base;
                    }
                    if TRACE {
                        trace.visit_count += 1;
                    }
                    break;
                }
            }
            if TRACE {
                if trace.visit_count < trace.visit_indices.len() {
                    trace.visit_positions[trace.visit_count] = trace.match_positions[visit_index];
                    trace.visit_indices[trace.visit_count] = match_index;
                    trace.visit_gate_passes[trace.visit_count] = gate_passed;
                    trace.visit_lengths[trace.visit_count] = current_ml;
                    trace.visit_winner_lengths[trace.visit_count] = ml;
                    trace.visit_winner_off_bases[trace.visit_count] =
                        if ml >= MIN_MATCH { best_off_base } else { 0 };
                }
                trace.visit_count += 1;
            }
        }
        let best = (ml >= MIN_MATCH).then_some(MatchCandidate {
            offset: best_off_base.saturating_sub(MIN_MATCH - 1),
            length: ml,
        });
        (best, attempts_left, trace)
    }

    pub(crate) fn find_source_match_with_budget(
        &mut self,
        src: &[u8],
        pos: usize,
        window_low: usize,
        lazy_skipping: bool,
        attempts: usize,
        trace_enabled: bool,
        timings: Option<&mut PlanningIterationTimings>,
    ) -> (Option<MatchCandidate>, usize, RowMatchBufferTrace) {
        match (trace_enabled, timings, self.min_match) {
            (true, Some(timings), 4) => self.find_source_match_with_budget_core::<true, true, 4>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                Some(timings),
            ),
            (true, Some(timings), 5) => self.find_source_match_with_budget_core::<true, true, 5>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                Some(timings),
            ),
            (true, Some(timings), _) => self.find_source_match_with_budget_core::<true, true, 6>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                Some(timings),
            ),
            (true, None, 4) => self.find_source_match_with_budget_core::<true, false, 4>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                None,
            ),
            (true, None, 5) => self.find_source_match_with_budget_core::<true, false, 5>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                None,
            ),
            (true, None, _) => self.find_source_match_with_budget_core::<true, false, 6>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                None,
            ),
            (false, Some(timings), 4) => self.find_source_match_with_budget_core::<false, true, 4>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                Some(timings),
            ),
            (false, Some(timings), 5) => self.find_source_match_with_budget_core::<false, true, 5>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                Some(timings),
            ),
            (false, Some(timings), _) => self.find_source_match_with_budget_core::<false, true, 6>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                Some(timings),
            ),
            (false, None, 4) => self.find_source_match_with_budget_core::<false, false, 4>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                None,
            ),
            (false, None, 5) => self.find_source_match_with_budget_core::<false, false, 5>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                None,
            ),
            (false, None, _) => self.find_source_match_with_budget_core::<false, false, 6>(
                src,
                pos,
                window_low,
                lazy_skipping,
                attempts,
                None,
            ),
        }
    }

    pub(crate) fn find_ext_dict_match_with_budget(
        &self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
        prefix_low: usize,
        attempts: usize,
        min_length_to_beat: usize,
    ) -> Option<MatchCandidate> {
        if pos + 8 > src.len() {
            return None;
        }

        let current = prefix_chain.len() + pos;
        let context = self.row_context();
        let hash = row_hash_src_at(
            src,
            pos,
            context.total_hash_bits,
            self.min_match,
            self.hash_salt,
        );
        let location = self.row_location_from_hash(hash, context);
        let mut match_buffer = [NO_POS; ROW_HASH_MAX_ENTRIES];
        let (num_matches, _) = row_collect_match_indices_no_positions(
            &self.hash_table[location.rel_row..location.rel_row + context.row_entries],
            &self.tag_table[location.rel_row..location.rel_row + context.row_entries],
            location,
            context,
            prefix_low,
            current,
            attempts,
            &mut match_buffer,
            core::ptr::null(),
        );

        let mut best = None;
        let mut best_length = min_length_to_beat;
        for &match_index_u32 in &match_buffer[..num_matches] {
            let match_index = match_index_u32 as usize;
            if match_index >= prefix_chain.len()
                || !virtual_match_has_length(prefix_chain, src, match_index, current, MIN_MATCH)
            {
                continue;
            }
            let length = count_match_length_virtual(prefix_chain, src, match_index, current);
            if length > best_length {
                best_length = length;
                best = Some(MatchCandidate {
                    offset: current - match_index,
                    length,
                });
                if pos + length == src.len() {
                    break;
                }
            }
        }

        best
    }

    pub(crate) fn find_dict_match_state_match_with_budget(
        &self,
        prefix: &[u8],
        src: &[u8],
        pos: usize,
        prefix_low: usize,
        attempts: usize,
        min_length_to_beat: usize,
    ) -> Option<MatchCandidate> {
        if pos + 8 > src.len() {
            return None;
        }

        let current = prefix.len() + pos;
        let context = self.row_context();
        let hash = row_hash_src_at(
            src,
            pos,
            context.total_hash_bits,
            self.min_match,
            self.hash_salt,
        );
        let location = self.row_location_from_hash(hash, context);
        let mut match_buffer = [NO_POS; ROW_HASH_MAX_ENTRIES];
        let (num_matches, _) = row_collect_match_indices_no_positions(
            &self.hash_table[location.rel_row..location.rel_row + context.row_entries],
            &self.tag_table[location.rel_row..location.rel_row + context.row_entries],
            location,
            context,
            prefix_low,
            current,
            attempts,
            &mut match_buffer,
            core::ptr::null(),
        );

        let mut best = None;
        let mut best_length = min_length_to_beat;
        for &match_index_u32 in &match_buffer[..num_matches] {
            let match_index = match_index_u32 as usize;
            if match_index + MIN_MATCH > prefix.len() || pos + MIN_MATCH > src.len() {
                continue;
            }
            if prefix[match_index..match_index + MIN_MATCH] != src[pos..pos + MIN_MATCH] {
                continue;
            }
            let length = count_match_length_with_prefix(prefix, src, match_index, current);
            if length > best_length {
                best_length = length;
                best = Some(MatchCandidate {
                    offset: current - match_index,
                    length,
                });
                if pos + length == src.len() {
                    break;
                }
            }
        }

        best
    }

    #[inline(always)]
    pub(crate) fn insert_hashed_position_with_location(
        &mut self,
        location: RowHashLocation,
        pos: usize,
        context: RowHashContext,
    ) {
        self.insert_row_position(location.rel_row, location.tag, pos, context);
    }
}

#[allow(unsafe_code)]
#[inline(always)]
pub(crate) fn row_collect_match_indices_no_positions(
    row: &[u32],
    tag_row: &[u8],
    location: RowHashLocation,
    context: RowHashContext,
    low_limit: usize,
    search_pos: usize,
    mut attempts_left: usize,
    match_buffer: &mut [u32; ROW_HASH_MAX_ENTRIES],
    src_base: *const u8,
) -> (usize, usize) {
    let mut num_matches = 0usize;
    // Slot 0 is a sentinel — head_index == 0 means the bucket is empty.
    if location.head_index == 0 {
        return (0, attempts_left);
    }
    // Use SIMD-accelerated tag matching (SSE2/NEON) with SWAR fallback.
    let mut matches = row_get_match_mask_fast(
        tag_row,
        location.tag,
        location.head_grouped,
        context.row_entries,
    );
    // Use raw pointers for the inner loop to avoid per-iteration bounds
    // checks on row[] and match_buffer[]. match_pos is masked to row_mask
    // (< row_entries == row.len()), and num_matches < ROW_HASH_MAX_ENTRIES
    // is guaranteed by the attempts budget.
    let row_ptr = row.as_ptr();
    let buf_ptr = match_buffer.as_mut_ptr();
    while matches != 0 && attempts_left != 0 {
        let next = matches.trailing_zeros() as usize;
        matches &= matches - 1;
        let match_pos = (location.head_grouped + next) & context.row_mask;
        if match_pos == 0 {
            continue;
        }
        // SAFETY: match_pos = (... & row_mask) where row_mask = row_entries - 1,
        // so match_pos < row_entries == row.len().
        let match_index = unsafe { *row_ptr.add(match_pos) };
        if (match_index as usize) < low_limit {
            break;
        }
        if (match_index as usize) >= search_pos {
            // A position at or beyond the byte being searched is not a
            // candidate: an offset must point backwards. It can only be here
            // because `RowMatchFinder::reset` deliberately leaves the position
            // table populated between frames, and a tag collision let an entry
            // from the previous frame survive the salt rotation. Skipping
            // rather than breaking because bucket order says nothing about
            // where such an entry sits relative to the live ones.
            //
            // Everything below this point treats the index as in-bounds and
            // behind `search_pos`: the prefetch offsets a raw pointer by it,
            // and callers hand it to `count_match_length_unchecked`. Without
            // this filter a long frame followed by a shorter one on the same
            // `Encoder` read up to 864 KB past the end of the source. See
            // `reusing_an_encoder_across_a_long_then_short_frame_stays_in_bounds`.
            continue;
        }
        if !src_base.is_null() {
            unsafe { crate::entropy::mem::prefetch_l1(src_base.add(match_index as usize)) };
        }
        debug_assert!(num_matches < ROW_HASH_MAX_ENTRIES);
        // SAFETY: num_matches < ROW_HASH_MAX_ENTRIES because we decrement
        // attempts_left (initially <= row_entries <= ROW_HASH_MAX_ENTRIES).
        unsafe { *buf_ptr.add(num_matches) = match_index };
        num_matches += 1;
        attempts_left -= 1;
    }
    (num_matches, attempts_left)
}

#[allow(unsafe_code)]
#[inline(always)]
pub(crate) fn row_collect_match_indices_with_positions(
    row: &[u32],
    tag_row: &[u8],
    location: RowHashLocation,
    context: RowHashContext,
    low_limit: usize,
    search_pos: usize,
    mut attempts_left: usize,
    match_positions: &mut [usize; ROW_HASH_MAX_ENTRIES],
    match_buffer: &mut [u32; ROW_HASH_MAX_ENTRIES],
    src_base: *const u8,
) -> (usize, usize) {
    let mut num_matches = 0usize;
    // Slot 0 is a sentinel — head_index == 0 means the bucket is empty.
    if location.head_index == 0 {
        return (0, attempts_left);
    }
    // Use SIMD-accelerated tag matching (SSE2/NEON) with SWAR fallback.
    let mut matches = row_get_match_mask_fast(
        tag_row,
        location.tag,
        location.head_grouped,
        context.row_entries,
    );
    while matches != 0 && attempts_left != 0 {
        let next = matches.trailing_zeros() as usize;
        matches &= matches - 1;
        let match_pos = (location.head_grouped + next) & context.row_mask;
        if match_pos == 0 {
            continue;
        }
        let match_index = row[match_pos];
        if (match_index as usize) < low_limit {
            break;
        }
        if (match_index as usize) >= search_pos {
            // A position at or beyond the byte being searched is not a
            // candidate: an offset must point backwards. It can only be here
            // because `RowMatchFinder::reset` deliberately leaves the position
            // table populated between frames, and a tag collision let an entry
            // from the previous frame survive the salt rotation. Skipping
            // rather than breaking because bucket order says nothing about
            // where such an entry sits relative to the live ones.
            //
            // Everything below this point treats the index as in-bounds and
            // behind `search_pos`: the prefetch offsets a raw pointer by it,
            // and callers hand it to `count_match_length_unchecked`. Without
            // this filter a long frame followed by a shorter one on the same
            // `Encoder` read up to 864 KB past the end of the source. See
            // `reusing_an_encoder_across_a_long_then_short_frame_stays_in_bounds`.
            continue;
        }
        if !src_base.is_null() {
            unsafe { crate::entropy::mem::prefetch_l1(src_base.add(match_index as usize)) };
        }
        debug_assert!(num_matches < match_buffer.len());
        match_positions[num_matches] = match_pos;
        match_buffer[num_matches] = match_index;
        num_matches += 1;
        attempts_left -= 1;
    }
    (num_matches, attempts_left)
}

#[allow(dead_code)]
pub(crate) fn row_collect_match_indices(
    row: &[u32],
    tag_row: &[u8],
    location: RowHashLocation,
    context: RowHashContext,
    low_limit: usize,
    search_pos: usize,
    attempts_left: usize,
    match_positions: Option<&mut [usize; ROW_HASH_MAX_ENTRIES]>,
    match_buffer: &mut [u32; ROW_HASH_MAX_ENTRIES],
    src_base: *const u8,
) -> (usize, usize) {
    match match_positions {
        Some(match_positions) => row_collect_match_indices_with_positions(
            row,
            tag_row,
            location,
            context,
            low_limit,
            search_pos,
            attempts_left,
            match_positions,
            match_buffer,
            src_base,
        ),
        None => row_collect_match_indices_no_positions(
            row,
            tag_row,
            location,
            context,
            low_limit,
            search_pos,
            attempts_left,
            match_buffer,
            src_base,
        ),
    }
}
pub(crate) fn row_match_mask_group_width(_row_entries: usize) -> usize {
    // Always use group_width == 1 so all platforms take the SWAR bitmask
    // path in row_collect_match_indices, which is faster than the scalar
    // byte-by-byte loop that was used on aarch64 for group_width > 1.
    1
}

pub(crate) fn rotate_right_with_width(value: u64, shift: usize, width: usize) -> u64 {
    debug_assert!(width > 0 && width <= 64);
    if width == 64 {
        return value.rotate_right((shift & 63) as u32);
    }
    let mask = (1u64 << width) - 1;
    let shift = shift % width;
    let value = value & mask;
    if shift == 0 {
        value
    } else {
        ((value >> shift) | (value << (width - shift))) & mask
    }
}

/// SIMD-accelerated tag matching dispatcher.
/// Uses SSE2 on x86_64, NEON on aarch64, falls back to SWAR elsewhere.
#[allow(unsafe_code)]
#[inline(always)]
fn row_get_match_mask_fast(
    tag_row: &[u8],
    tag: u8,
    head_grouped: usize,
    row_entries: usize,
) -> u64 {
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is universal on x86_64. tag_row.len() >= row_entries.
        unsafe {
            return row_get_match_mask_sse2(tag_row, tag, head_grouped, row_entries);
        }
    }
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is universal on aarch64. tag_row.len() >= row_entries.
        unsafe {
            return row_get_match_mask_neon(tag_row, tag, head_grouped, row_entries);
        }
    }
    #[allow(unreachable_code)]
    row_get_match_mask_swar(tag_row, tag, head_grouped, row_entries)
}

#[cfg(target_arch = "x86_64")]
#[allow(unsafe_code)]
#[inline(always)]
unsafe fn row_get_match_mask_sse2(
    tag_row: &[u8],
    tag: u8,
    head_grouped: usize,
    row_entries: usize,
) -> u64 {
    use core::arch::x86_64::*;
    // SAFETY: `tag_row` holds at least `row_entries` bytes by contract, and the
    // loop reads whole 16-byte chunks strictly below `row_entries / 16 * 16`,
    // so no load passes the end. `_mm_loadu_si128` is an unaligned load, so
    // `tag_row` needs no alignment. The intrinsics themselves are unsafe only
    // because they require the `sse2` target feature, which is baseline on
    // `x86_64` — and which this function's own `#[target_feature]` does not
    // list, so the block is what carries the requirement.
    //
    // The aarch64 twin below has had this block since edition 2024 made
    // `unsafe_op_in_unsafe_fn` deny-by-default. This one did not, and nothing
    // built on an Apple Silicon machine compiles it: `cargo check --target
    // x86_64-unknown-linux-gnu` is what sees this arm at all.
    unsafe {
        let splat = _mm_set1_epi8(tag as i8);
        let ptr = tag_row.as_ptr();
        let mut result: u64 = 0;
        let chunks = row_entries / 16;
        for c in 0..chunks {
            let offset = c * 16;
            let data = _mm_loadu_si128(ptr.add(offset) as *const __m128i);
            let cmp = _mm_cmpeq_epi8(data, splat);
            let mask = _mm_movemask_epi8(cmp) as u16 as u64;
            result |= mask << offset;
        }
        if row_entries < 64 {
            result &= (1u64 << row_entries) - 1;
        }
        rotate_right_with_width(result, head_grouped, row_entries)
    }
}

#[cfg(target_arch = "aarch64")]
#[allow(unsafe_code)]
#[inline(always)]
unsafe fn row_get_match_mask_neon(
    tag_row: &[u8],
    tag: u8,
    head_grouped: usize,
    row_entries: usize,
) -> u64 {
    use core::arch::aarch64::*;
    // SAFETY: `tag_row` holds at least `row_entries` bytes by contract, and the
    // loop reads whole 16-byte chunks strictly below `row_entries / 16 * 16`,
    // so no load passes the end. `vld1q_u8` is an unaligned load, so neither
    // `tag_row` nor the local array needs alignment. The NEON intrinsics
    // themselves are unsafe only because they require the `neon` target
    // feature, which is baseline on aarch64.
    unsafe {
        let splat = vdupq_n_u8(tag);
        let bit_positions: [u8; 16] = [1, 2, 4, 8, 16, 32, 64, 128, 1, 2, 4, 8, 16, 32, 64, 128];
        let bit_mask = vld1q_u8(bit_positions.as_ptr());
        let ptr = tag_row.as_ptr();
        let mut result: u64 = 0;
        let chunks = row_entries / 16;
        for c in 0..chunks {
            let offset = c * 16;
            let data = vld1q_u8(ptr.add(offset));
            let cmp = vceqq_u8(data, splat);
            let masked = vandq_u8(cmp, bit_mask);
            let lo = vget_low_u8(masked);
            let hi = vget_high_u8(masked);
            let lo_byte = vaddv_u8(lo) as u64;
            let hi_byte = vaddv_u8(hi) as u64;
            let chunk_mask = lo_byte | (hi_byte << 8);
            result |= chunk_mask << offset;
        }
        if row_entries < 64 {
            result &= (1u64 << row_entries) - 1;
        }
        rotate_right_with_width(result, head_grouped, row_entries)
    }
}

#[inline(always)]
fn row_get_match_mask_swar(
    tag_row: &[u8],
    tag: u8,
    head_grouped: usize,
    row_entries: usize,
) -> u64 {
    const CHUNK: usize = 8;
    const X01: u64 = 0x0101_0101_0101_0101;
    const X80: u64 = 0x8080_8080_8080_8080;

    let splat_tag = (tag as u64).wrapping_mul(X01);
    let mut matches: u64 = 0;
    let mut i = row_entries.wrapping_sub(CHUNK);

    if cfg!(target_endian = "little") {
        // Magic constant: (0xFFFFFFFFFFFFFFFF / 0x7F) >> 8
        // Collects the high bit of each byte into adjacent bit positions.
        const EXTRACT: u64 = 0x0002_0408_1020_4081;
        while let Ok(bytes) = <[u8; 8]>::try_from(&tag_row[i..i + CHUNK]) {
            let mut chunk = u64::from_ne_bytes(bytes);
            chunk ^= splat_tag;
            // Set high bit for each non-zero byte (non-matching tags).
            chunk = (((chunk | X80).wrapping_sub(X01)) | chunk) & X80;
            matches <<= CHUNK;
            matches |= chunk.wrapping_mul(EXTRACT) >> 56;
            if i < CHUNK {
                break;
            }
            i -= CHUNK;
        }
    } else {
        const MSB: u64 = 1u64 << 63;
        let extract: u64 = (MSB / 0x1FF) | MSB;
        while let Ok(bytes) = <[u8; 8]>::try_from(&tag_row[i..i + CHUNK]) {
            let mut chunk = u64::from_ne_bytes(bytes);
            chunk ^= splat_tag;
            chunk = (((chunk | X80).wrapping_sub(X01)) | chunk) & X80;
            matches <<= CHUNK;
            matches |= ((chunk >> 7).wrapping_mul(extract)) >> 56;
            if i < CHUNK {
                break;
            }
            i -= CHUNK;
        }
    }

    // The trick marks non-matching bytes; invert to get matching positions.
    matches = !matches;
    if row_entries < 64 {
        matches &= (1u64 << row_entries) - 1;
    }
    rotate_right_with_width(matches, head_grouped, row_entries)
}

#[allow(unsafe_code)]
#[inline(always)]
pub(crate) fn source_match_passes_gate_at_offset(
    src: &[u8],
    left: usize,
    right: usize,
    gate_offset: usize,
) -> bool {
    // Since left < right (candidate before current pos), only the larger
    // index needs a bounds check. Use raw pointer reads to skip the
    // per-read bounds checks in read_u32.
    debug_assert!(left <= right);
    if right + gate_offset + MIN_MATCH > src.len() {
        return false;
    }
    // SAFETY: right + gate_offset + 4 <= src.len() and left <= right,
    // so both reads are within bounds.
    unsafe {
        let base = src.as_ptr();
        let left_val = core::ptr::read_unaligned(base.add(left + gate_offset) as *const u32);
        let right_val = core::ptr::read_unaligned(base.add(right + gate_offset) as *const u32);
        left_val == right_val
    }
}

pub(crate) fn source_match_passes_best_length_gate(
    src: &[u8],
    left: usize,
    right: usize,
    current_best_length: usize,
) -> bool {
    source_match_passes_gate_at_offset(
        src,
        left,
        right,
        current_best_length.saturating_sub(MIN_MATCH - 1),
    )
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedChainDictionaryTables {
    pub(crate) prefix_finder: Arc<MatchFinder>,
}

impl PreparedChainDictionaryTables {
    pub(crate) fn prefix_finder(&self) -> Arc<MatchFinder> {
        self.prefix_finder.clone()
    }
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedRowDictionaryTables {
    pub(crate) prefix_finder: Arc<RowHashFinder>,
}

impl PreparedRowDictionaryTables {
    pub(crate) fn prefix_finder(&self) -> Arc<RowHashFinder> {
        self.prefix_finder.clone()
    }

    pub(crate) fn row_hash_log(&self) -> u32 {
        self.prefix_finder.row_hash_log
    }
}

/// The chain search's per-call diagnostic record, and **only** in test builds.
///
/// Every field here is written under `cfg(test)` and read by
/// `trace_chain_search`, which is itself a no-op outside test builds -- so in a
/// shipping build the contents were always dead. What was not dead was the
/// struct: at ~190 bytes it sits inside `ChainRegularMatchSearch`, which
/// `best_chain_ext_dict_regular_match_core` returns by value once per search,
/// and a return type that size is passed through memory rather than registers.
/// Collapsing it to a unit struct off the test path is what makes that return
/// small enough to stay in registers.
#[cfg(test)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ChainRegularMatchTrace {
    pub(crate) source_length: usize,
    pub(crate) source_offset: usize,
    pub(crate) dict_length: usize,
    pub(crate) dict_offset: usize,
    pub(crate) attempts_left_before_dict: usize,
    pub(crate) hash_slot: usize,
    pub(crate) head_index: usize,
    pub(crate) next_to_update: usize,
    pub(crate) low_limit: usize,
    pub(crate) min_chain: usize,
    pub(crate) chain_link_count: usize,
    pub(crate) chain_link_indices: [usize; 4],
    pub(crate) chain_link_sources: [SequenceTraceMatchSource; 4],
    pub(crate) visit_count: usize,
    pub(crate) visit_indices: [usize; 4],
    pub(crate) visit_lengths: [usize; 4],
}

/// See the test-build twin above: outside tests this carries nothing, so the
/// search's return value stays small.
#[cfg(not(test))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ChainRegularMatchTrace {}

#[cfg_attr(not(test), allow(dead_code))]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct ChainRegularMatchSearch {
    pub(crate) candidate: Option<MatchCandidate>,
    pub(crate) source: SequenceTraceMatchSource,
    pub(crate) trace: ChainRegularMatchTrace,
}

#[cfg(test)]
pub(crate) fn trace_chain_search(
    plan: &mut SequencePlan,
    anchor: usize,
    pos: usize,
    probe_depth: u8,
    offset_1: usize,
    offset_2: usize,
    current_best: LazyParserMatch,
    current_source: SequenceTraceMatchSource,
    rep_length: usize,
    regular_search: ChainRegularMatchSearch,
    chosen: LazyParserMatch,
    chosen_source: SequenceTraceMatchSource,
) {
    if !plan.tracing_enabled() {
        return;
    }
    plan.trace_chain_searches.push(SequenceTraceChainSearch {
        anchor,
        pos,
        probe_depth,
        offset_1,
        offset_2,
        current_kind: match current_best.kind {
            LazyMatchKind::Repeat1 => SequenceTraceEmissionKind::Rep,
            LazyMatchKind::Regular => SequenceTraceEmissionKind::Regular,
        },
        current_source,
        current_length: current_best.length,
        current_offbase: current_best.offbase(),
        rep_length,
        source_length: regular_search.trace.source_length,
        source_offset: regular_search.trace.source_offset,
        dict_length: regular_search.trace.dict_length,
        dict_offset: regular_search.trace.dict_offset,
        hash_slot: regular_search.trace.hash_slot,
        head_index: regular_search.trace.head_index,
        next_to_update: regular_search.trace.next_to_update,
        low_limit: regular_search.trace.low_limit,
        min_chain: regular_search.trace.min_chain,
        chain_link_count: regular_search.trace.chain_link_count,
        chain_link_indices: regular_search.trace.chain_link_indices,
        chain_link_sources: regular_search.trace.chain_link_sources,
        visit_count: regular_search.trace.visit_count,
        visit_indices: regular_search.trace.visit_indices,
        visit_lengths: regular_search.trace.visit_lengths,
        regular_winner: regular_search.source,
        chosen_kind: match chosen.kind {
            LazyMatchKind::Repeat1 => SequenceTraceEmissionKind::Rep,
            LazyMatchKind::Regular => SequenceTraceEmissionKind::Regular,
        },
        chosen_source,
        chosen_start: chosen.start,
        chosen_length: chosen.length,
        chosen_offbase: chosen.offbase(),
        attempts_left_before_dict: regular_search.trace.attempts_left_before_dict,
    });
}

#[cfg(not(test))]
pub(crate) fn trace_chain_search(
    _plan: &mut SequencePlan,
    _anchor: usize,
    _pos: usize,
    _probe_depth: u8,
    _offset_1: usize,
    _offset_2: usize,
    _current_best: LazyParserMatch,
    _current_source: SequenceTraceMatchSource,
    _rep_length: usize,
    _regular_search: ChainRegularMatchSearch,
    _chosen: LazyParserMatch,
    _chosen_source: SequenceTraceMatchSource,
) {
}

#[inline(always)]
pub(crate) fn finish_chain_regular_search(
    search: ChainRegularMatchSearch,
    timings: Option<&mut PlanningIterationTimings>,
    total_start: Option<Instant>,
    insert_update_total: Duration,
    match_count_total: Duration,
) -> ChainRegularMatchSearch {
    if let (Some(timings), Some(total_start)) = (timings, total_start) {
        timings.insert_update += insert_update_total;
        timings.match_count += match_count_total;
        timings.chain_search += total_start
            .elapsed()
            .saturating_sub(insert_update_total)
            .saturating_sub(match_count_total);
    }
    search
}

pub(crate) fn best_chain_dict_match_state_regular_match(
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    prefix_low: usize,
    source_low: usize,
    prefix_finder: &MatchFinder,
    src_finder: &mut MatchFinder,
    lazy_skipping: bool,
    timings: Option<&mut PlanningIterationTimings>,
) -> ChainRegularMatchSearch {
    if pos + MIN_MATCH > src.len() {
        return ChainRegularMatchSearch::default();
    }

    let total_start = timings.as_ref().map(|_| Instant::now());
    let mut insert_update_total = Duration::ZERO;
    let mut match_count_total = Duration::ZERO;
    let mut attempts_left = 1usize << params.search_log;
    let mut best = None;
    let mut best_length = MIN_MATCH - 1;
    let mut best_source = SequenceTraceMatchSource::Unknown;
    let chain_size = 1usize << params.chain_log.min(usize::BITS.saturating_sub(1));
    let source_min_chain = pos.saturating_sub(chain_size).max(source_low);
    let dict_min_chain = prefix_chain
        .len()
        .saturating_sub(chain_size)
        .max(prefix_low);
    #[allow(unused_mut)]
    let mut trace = ChainRegularMatchTrace::default();

    let insert_start = timings.as_ref().map(|_| Instant::now());
    let mut source_candidate = src_finder
        .insert_and_find_first_index(src, pos, lazy_skipping)
        .unwrap_or(NO_POS as usize);
    if let Some(insert_start) = insert_start {
        insert_update_total += insert_start.elapsed();
    }
    #[cfg(test)]
    {
        trace.head_index = source_candidate;
    }
    while attempts_left > 0 && source_candidate != NO_POS as usize && source_candidate >= source_low
    {
        #[cfg(test)]
        {
            let visit_index = trace
                .visit_count
                .min(trace.visit_indices.len().saturating_sub(1));
            if trace.visit_count < trace.visit_indices.len() {
                trace.visit_indices[visit_index] = source_candidate;
            }
        }
        // Only read under `cfg(test)`, by the visit record below.
        #[cfg_attr(not(test), allow(unused_variables))]
        let scored = source_match_passes_best_length_gate(src, source_candidate, pos, best_length);
        if scored {
            let match_count_start = timings.as_ref().map(|_| Instant::now());
            let candidate = MatchCandidate {
                offset: pos - source_candidate,
                length: count_match_length(src, source_candidate, pos),
            };
            if let Some(match_count_start) = match_count_start {
                match_count_total += match_count_start.elapsed();
            }
            #[cfg(test)]
            {
                let visit_index = trace
                    .visit_count
                    .min(trace.visit_indices.len().saturating_sub(1));
                if trace.visit_count < trace.visit_lengths.len() {
                    trace.visit_lengths[visit_index] = candidate.length;
                }
            }
            if candidate.length >= params.min_match_length_zero_literals
                && candidate.length > best_length
            {
                #[cfg(test)]
                {
                    trace.source_length = candidate.length;
                    trace.source_offset = candidate.offset;
                }
                best_length = candidate.length;
                best = Some(candidate);
                best_source = SequenceTraceMatchSource::Source;
                if pos + candidate.length == src.len() {
                    return finish_chain_regular_search(
                        ChainRegularMatchSearch {
                            candidate: best,
                            source: best_source,
                            trace,
                        },
                        timings,
                        total_start,
                        insert_update_total,
                        match_count_total,
                    );
                }
            }
        }
        #[cfg(test)]
        {
            let visit_index = trace
                .visit_count
                .min(trace.visit_indices.len().saturating_sub(1));
            if !scored && trace.visit_count < trace.visit_lengths.len() {
                trace.visit_lengths[visit_index] = 0;
            }
            trace.visit_count = trace.visit_count.saturating_add(1);
        }
        attempts_left -= 1;
        if source_candidate <= source_min_chain {
            break;
        }
        let prev = src_finder.previous_at(source_candidate);
        if prev == NO_POS {
            break;
        }
        source_candidate = prev as usize;
    }
    #[cfg(test)]
    {
        trace.attempts_left_before_dict = attempts_left;
    }

    let current = prefix_chain.len() + pos;
    let mut dict_candidate = prefix_finder
        .lookup_prefix_chain(prefix_chain, src, pos)
        .unwrap_or(NO_POS as usize);
    while attempts_left > 0 && dict_candidate != NO_POS as usize && dict_candidate >= prefix_low {
        if virtual_match_has_length(prefix_chain, src, dict_candidate, current, MIN_MATCH) {
            let match_count_start = timings.as_ref().map(|_| Instant::now());
            let candidate = MatchCandidate {
                offset: current - dict_candidate,
                length: count_match_length_virtual(prefix_chain, src, dict_candidate, current),
            };
            if let Some(match_count_start) = match_count_start {
                match_count_total += match_count_start.elapsed();
            }
            if candidate.length >= params.min_match_length_zero_literals
                && candidate.length > best_length
            {
                #[cfg(test)]
                {
                    trace.dict_length = candidate.length;
                    trace.dict_offset = candidate.offset;
                }
                best_length = candidate.length;
                best = Some(candidate);
                best_source = SequenceTraceMatchSource::Dict;
                if pos + candidate.length == src.len() {
                    break;
                }
            }
        }
        attempts_left -= 1;
        if dict_candidate <= dict_min_chain {
            break;
        }
        let prev = prefix_finder.previous_at(dict_candidate);
        if prev == NO_POS {
            break;
        }
        dict_candidate = prev as usize;
    }

    finish_chain_regular_search(
        ChainRegularMatchSearch {
            candidate: best,
            source: best_source,
            trace,
        },
        timings,
        total_start,
        insert_update_total,
        match_count_total,
    )
}

pub(crate) fn prefixed_chain_global_low_limit(
    prefix_len: usize,
    prefix_low: usize,
    source_low: usize,
) -> usize {
    if prefix_low < prefix_len {
        prefix_low
    } else {
        prefix_len + source_low
    }
}

fn best_chain_ext_dict_regular_match_core<const PROFILE: bool>(
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    prefix_low: usize,
    source_low: usize,
    finder: &mut MatchFinder,
    lazy_skipping: bool,
    timings: Option<&mut PlanningIterationTimings>,
) -> ChainRegularMatchSearch {
    if pos + MIN_MATCH > src.len() {
        return ChainRegularMatchSearch::default();
    }

    let total_start = PROFILE.then(Instant::now);
    let mut insert_update_total = Duration::ZERO;
    let mut match_count_total = Duration::ZERO;
    let prefix_len = prefix_chain.len();
    let current = prefix_len + pos;
    let mut attempts_left = 1usize << params.search_log;
    let mut best = None;
    let mut best_length = MIN_MATCH - 1;
    let mut best_source = SequenceTraceMatchSource::Unknown;
    let chain_size = 1usize << params.chain_log.min(usize::BITS.saturating_sub(1));
    let min_chain = current.saturating_sub(chain_size);
    let low_limit = prefixed_chain_global_low_limit(prefix_len, prefix_low, source_low);
    #[allow(unused_mut)]
    let mut trace = ChainRegularMatchTrace::default();
    #[cfg(test)]
    {
        trace.hash_slot = finder.hash_src_at(src, pos);
        trace.next_to_update = finder.next_to_update;
        trace.low_limit = low_limit;
        trace.min_chain = min_chain;
    }
    let insert_start = PROFILE.then(Instant::now);
    let mut candidate = finder
        .insert_and_find_first_index_ext_dict(prefix_len, src, pos, lazy_skipping)
        .unwrap_or(NO_POS as usize);
    if let Some(insert_start) = insert_start {
        insert_update_total += insert_start.elapsed();
    }
    #[cfg(test)]
    {
        trace.next_to_update = finder.next_to_update;
        trace.head_index = candidate;
        let mut raw_link = candidate;
        while trace.chain_link_count < trace.chain_link_indices.len()
            && raw_link != NO_POS as usize
            && raw_link >= low_limit
        {
            let link_index = trace.chain_link_count;
            trace.chain_link_indices[link_index] = raw_link;
            trace.chain_link_sources[link_index] = if raw_link < prefix_len {
                SequenceTraceMatchSource::Prefix
            } else {
                SequenceTraceMatchSource::Source
            };
            trace.chain_link_count += 1;
            if raw_link <= min_chain {
                break;
            }
            let prev = finder.previous_at(raw_link);
            if prev == NO_POS {
                break;
            }
            raw_link = prev as usize;
        }
    }

    let chain_walk_mask = finder.chain_mask();
    let previous_ptr = finder.previous.as_ptr();
    let previous_len = finder.previous.len();
    let _ = previous_len;
    while attempts_left > 0 && candidate != NO_POS as usize && candidate >= low_limit {
        let is_prefix = candidate < prefix_len;
        let candidate_length = if is_prefix {
            #[cfg(test)]
            if trace.attempts_left_before_dict == 0 {
                trace.attempts_left_before_dict = attempts_left;
            }
            if !virtual_match_has_length(prefix_chain, src, candidate, current, MIN_MATCH) {
                0
            } else {
                let match_count_start = PROFILE.then(Instant::now);
                let length = count_match_length_virtual(prefix_chain, src, candidate, current);
                if let Some(match_count_start) = match_count_start {
                    match_count_total += match_count_start.elapsed();
                }
                length
            }
        } else {
            let source_candidate = candidate - prefix_len;
            if !source_match_passes_best_length_gate(src, source_candidate, pos, best_length) {
                0
            } else {
                let match_count_start = PROFILE.then(Instant::now);
                let length = count_match_length(src, source_candidate, pos);
                if let Some(match_count_start) = match_count_start {
                    match_count_total += match_count_start.elapsed();
                }
                length
            }
        };
        #[cfg(test)]
        {
            let visit_index = trace
                .visit_count
                .min(trace.visit_indices.len().saturating_sub(1));
            if trace.visit_count < trace.visit_indices.len() {
                trace.visit_indices[visit_index] = candidate;
                trace.visit_lengths[visit_index] = candidate_length;
            }
            trace.visit_count = trace.visit_count.saturating_add(1);
        }

        let raw_offset = current - candidate;
        #[cfg(test)]
        if is_prefix {
            if candidate_length > trace.dict_length {
                trace.dict_length = candidate_length;
                trace.dict_offset = raw_offset;
            }
        } else if candidate_length > trace.source_length {
            trace.source_length = candidate_length;
            trace.source_offset = raw_offset;
        }

        if candidate_length >= params.min_match_length_zero_literals
            && candidate_length > best_length
        {
            best_length = candidate_length;
            best = Some(MatchCandidate {
                offset: raw_offset,
                length: candidate_length,
            });
            best_source = if is_prefix {
                SequenceTraceMatchSource::Prefix
            } else {
                SequenceTraceMatchSource::Source
            };
            if pos + candidate_length == src.len() {
                break;
            }
        }

        attempts_left -= 1;
        if candidate <= min_chain {
            break;
        }
        // Unchecked: `candidate & chain_walk_mask` is always in bounds since
        // chain_walk_mask = previous.len() - 1 and previous.len() is a power
        // of two. Removes the bounds-check branch from the hot pointer-chase.
        debug_assert!((candidate & chain_walk_mask) < previous_len);
        #[allow(unsafe_code)]
        let prev = unsafe { *previous_ptr.add(candidate & chain_walk_mask) };
        if prev == NO_POS {
            break;
        }
        candidate = prev as usize;
    }

    finish_chain_regular_search(
        ChainRegularMatchSearch {
            candidate: best,
            source: best_source,
            trace,
        },
        timings,
        total_start,
        insert_update_total,
        match_count_total,
    )
}

#[allow(dead_code)]
pub(crate) fn best_chain_ext_dict_regular_match(
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    prefix_low: usize,
    source_low: usize,
    finder: &mut MatchFinder,
    lazy_skipping: bool,
    timings: Option<&mut PlanningIterationTimings>,
) -> ChainRegularMatchSearch {
    if timings.is_some() {
        best_chain_ext_dict_regular_match_core::<true>(
            prefix_chain,
            src,
            pos,
            params,
            prefix_low,
            source_low,
            finder,
            lazy_skipping,
            timings,
        )
    } else {
        best_chain_ext_dict_regular_match_core::<false>(
            prefix_chain,
            src,
            pos,
            params,
            prefix_low,
            source_low,
            finder,
            lazy_skipping,
            timings,
        )
    }
}
pub(crate) fn plan_sequences_row_without_prefix_into(
    plan: &mut SequencePlan,
    src: &[u8],
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
) -> Result<()> {
    let mut finder = RowHashFinder::new(params.hash_bits, params.search_log, params.min_match);
    plan_sequences_row_without_prefix_from_into(
        plan,
        src,
        0,
        repeat_offsets,
        params,
        MatchFloor::fixed(0),
        &mut finder,
    )
}

pub(crate) fn plan_sequences_row_without_prefix_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let limit = row_search_limit(src.len());
    if block_start >= limit {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let depth = params.lazy_search_depth.min(2);
    // C's `ip += (dictAndPrefixLength == 0)`: skip the very first byte only
    // when there is no history at all to match it against.
    let pos = block_start + usize::from(block_start == match_floor.at(block_start));
    if pos >= limit {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    match (
        plan.tracing_enabled(),
        plan.planning_profile.is_enabled(),
        finder.min_match,
    ) {
        (true, true, 4) => plan_sequences_row_without_prefix_from_into_core::<true, true, 4>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (true, true, 5) => plan_sequences_row_without_prefix_from_into_core::<true, true, 5>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (true, true, _) => plan_sequences_row_without_prefix_from_into_core::<true, true, 6>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (true, false, 4) => plan_sequences_row_without_prefix_from_into_core::<true, false, 4>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (true, false, 5) => plan_sequences_row_without_prefix_from_into_core::<true, false, 5>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (true, false, _) => plan_sequences_row_without_prefix_from_into_core::<true, false, 6>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (false, true, 4) => plan_sequences_row_without_prefix_from_into_core::<false, true, 4>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (false, true, 5) => plan_sequences_row_without_prefix_from_into_core::<false, true, 5>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (false, true, _) => plan_sequences_row_without_prefix_from_into_core::<false, true, 6>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (false, false, 4) => plan_sequences_row_without_prefix_from_into_core::<false, false, 4>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (false, false, 5) => plan_sequences_row_without_prefix_from_into_core::<false, false, 5>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
        (false, false, _) => plan_sequences_row_without_prefix_from_into_core::<false, false, 6>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        ),
    }
}

fn plan_sequences_row_without_prefix_from_into_core<
    const TRACE: bool,
    const PROFILE: bool,
    const MLS: u32,
>(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
    limit: usize,
    mut pos: usize,
    depth: usize,
) -> Result<()> {
    // Fast path: when neither tracing nor profiling, use the lean loop that
    // avoids all timing/trace parameters and the RowMatchBufferTrace return.
    if !TRACE && !PROFILE {
        plan_sequences_row_fast::<MLS>(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
            limit,
            pos,
            depth,
        );
        return Ok(());
    }

    let (mut repeat_offsets, _, _, saved1, saved2) =
        invalidate_no_dict_repeat_offsets(repeat_offsets, pos, match_floor.at(pos));
    let [offset_1_raw, offset_2_raw, _] = repeat_offsets.values();
    let mut offset_1 = offset_1_raw as usize;
    let mut offset_2 = offset_2_raw as usize;
    let mut anchor = block_start;
    let mut lazy_skipping = false;
    finder.refill_hash_cache::<MLS>(src, limit);

    while pos < limit {
        let iteration_start = PROFILE.then(Instant::now);
        let mut iteration_timings = PROFILE.then(PlanningIterationTimings::default);

        let Some(best) = row_no_dict_find_lazy_match_core::<TRACE, PROFILE, MLS>(
            src,
            pos,
            params,
            match_floor,
            finder,
            lazy_skipping,
            limit,
            depth,
            anchor,
            offset_1,
            offset_2,
            &mut plan.trace_row_searches,
            &mut plan.trace_row_lazy_probes,
            iteration_timings.as_mut(),
        ) else {
            let step = skip_after_no_match(anchor, pos, params);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = pos.saturating_add(step);
            if let (Some(iteration_start), Some(iteration_timings)) =
                (iteration_start, iteration_timings)
            {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        };

        pos = row_no_dict_store_match_and_chain_rep2_core::<PROFILE, MLS>(
            plan,
            src,
            &mut anchor,
            &mut repeat_offsets,
            best,
            match_floor,
            finder,
            &mut lazy_skipping,
            limit,
            iteration_timings.as_mut(),
        )?;
        let [next_offset_1, next_offset_2, _] = repeat_offsets.values();
        offset_1 = next_offset_1 as usize;
        offset_2 = next_offset_2 as usize;
        if let (Some(iteration_start), Some(iteration_timings)) =
            (iteration_start, iteration_timings)
        {
            plan.planning_profile
                .record_iteration(iteration_start.elapsed(), iteration_timings);
        }
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = restore_invalidated_repeat_offsets(repeat_offsets, saved1, saved2);
    Ok(())
}

/// Production-only fast loop: no TRACE, no PROFILE, no timing parameters.
/// Only generic on MLS. This eliminates:
/// - All `Instant::now()` / `elapsed()` timing overhead
/// - `Option<&mut PlanningIterationTimings>` parameter passing
/// - `SequenceTraceRowLazyProbe` / `SequenceTraceRowSearch` collection
/// - `PlanningIterationCategorySnapshot` captures
/// - The 200-byte `RowMatchBufferTrace` return from match finding
fn plan_sequences_row_fast<const MLS: u32>(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
    limit: usize,
    mut pos: usize,
    depth: usize,
) {
    let (mut repeat_offsets, _, _, saved1, saved2) =
        invalidate_no_dict_repeat_offsets(repeat_offsets, pos, match_floor.at(pos));
    let [offset_1_raw, offset_2_raw, _] = repeat_offsets.values();
    let mut offset_1 = offset_1_raw as usize;
    let mut offset_2 = offset_2_raw as usize;
    let mut anchor = block_start;
    let mut lazy_skipping = false;
    finder.refill_hash_cache::<MLS>(src, limit);

    while pos < limit {
        let best = row_no_dict_find_lazy_match_fast::<MLS>(
            src,
            pos,
            params,
            match_floor,
            finder,
            lazy_skipping,
            limit,
            depth,
            anchor,
            offset_1,
            offset_2,
        );
        if best.length < MIN_MATCH {
            let step = ((pos - anchor) >> params.skip_search_strength) + 1;
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos += step;
            continue;
        }

        (pos, offset_1, offset_2) = row_no_dict_store_match_and_chain_rep2_fast::<MLS>(
            plan,
            src,
            &mut anchor,
            &mut repeat_offsets,
            best,
            match_floor,
            finder,
            &mut lazy_skipping,
            limit,
        );
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = restore_invalidated_repeat_offsets(repeat_offsets, saved1, saved2);
}

pub(crate) fn plan_sequences_row_with_prefixes_into(
    plan: &mut SequencePlan,
    src: &[u8],
    prefixes: &[&[u8]],
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
) -> Result<()> {
    let prefix_chain =
        PrefixChain::new(prefixes)?.expect("row-based prefix planning requires a non-empty prefix");
    let mut prefix_finder =
        RowHashFinder::new(params.hash_bits, params.search_log, params.min_match);
    prefix_finder.insert_prefix_chain(prefix_chain, src);
    let mut src_finder = RowHashFinder::new(params.hash_bits, params.search_log, params.min_match);
    plan_sequences_row_ext_dict_from_into(
        plan,
        src,
        0,
        prefix_chain,
        repeat_offsets,
        params,
        PrefixedMatchFloor::fixed(0, 0),
        &prefix_finder,
        &mut src_finder,
    )
}

#[inline(always)]
pub(crate) fn row_no_dict_repeat_match_candidate_core<const PROFILE: bool>(
    src: &[u8],
    pos: usize,
    raw_offset: usize,
    window_low: usize,
    ahead: bool,
    timings: Option<&mut PlanningIterationTimings>,
) -> Option<MatchCandidate> {
    let rep_start = PROFILE.then(Instant::now);
    let candidate = if ahead {
        repeat_ahead_match_without_prefix(src, pos, raw_offset, window_low)
    } else {
        repeat_match_without_prefix_at(src, pos, raw_offset, window_low)
    };
    if let (Some(timings), Some(rep_start)) = (timings, rep_start) {
        timings.rep_check += rep_start.elapsed();
    }
    candidate
}

/// Fast lazy match finder — no TRACE, no PROFILE. Only generic on MLS.
#[inline(always)]
fn row_no_dict_find_lazy_match_fast<const MLS: u32>(
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
    lazy_skipping: bool,
    limit: usize,
    depth: usize,
    _anchor: usize,
    rep1: usize,
    _rep2: usize,
) -> LazyParserMatch {
    // Hoist the search budget — it's constant across all probes. The floor is
    // not: C evaluates it at the position it is searching from, so each probe
    // below resolves its own.
    let attempts = finder.search_attempt_budget(params.search_log);
    let window_low = match_floor.at(pos);

    // Inline rep check — extract length/offset directly to avoid Option overhead.
    let (baseline_rep_length, baseline_rep_offset) =
        if rep1 > 0 && rep1 <= pos && pos + 1 + MIN_MATCH <= src.len() {
            if let Some(c) = repeat_ahead_match_without_prefix(src, pos, rep1, window_low) {
                (c.length, c.offset)
            } else {
                (0, rep1)
            }
        } else {
            (0, rep1)
        };
    let mut current_start = pos + 1;
    let mut current_offset = baseline_rep_offset;
    let mut current_length = baseline_rep_length;
    let mut current_kind = LazyMatchKind::Repeat1;
    // Only the bit width is state here, unlike the trace-carrying variant
    // below, which returns the off_base itself and so has to keep it. This
    // function returns a `LazyParserMatch`, which has no off_base field.
    let mut current_off_base_bits = 0i32;

    // At depth==0 (greedy), a rep match is always preferred — skip the
    // expensive regular hash search entirely. Matches C's `goto _storeSequence`.
    if depth == 0 && baseline_rep_length >= MIN_MATCH {
        return LazyParserMatch {
            start: current_start,
            offset: current_offset,
            length: current_length,
            kind: current_kind,
        };
    }

    let (baseline_regular, _) =
        finder.find_source_match_fast::<MLS>(src, pos, window_low, lazy_skipping, attempts);
    if baseline_regular.length > current_length {
        current_start = pos;
        current_offset = baseline_regular.offset;
        current_length = baseline_regular.length;
        current_kind = LazyMatchKind::Regular;
        current_off_base_bits = highbit32(explicit_offbase(baseline_regular.offset)) as i32;
    }

    if depth == 0 || current_length < MIN_MATCH {
        // length < MIN_MATCH signals "no match" to caller (C convention).
        return LazyParserMatch {
            start: current_start,
            offset: current_offset,
            length: current_length,
            kind: current_kind,
        };
    }

    // Lazy probes (depth >= 1)
    let mut probe_pos = pos;
    while probe_pos < limit {
        let depth1_pos = probe_pos + 1;
        if depth1_pos > limit {
            break;
        }
        let window_low = match_floor.at(depth1_pos);

        let repeat = repeat_match_without_prefix_at(src, depth1_pos, rep1, window_low);
        if let Some(repeat) = repeat {
            let gain2 = repeat.length as i32 * 3;
            let gain1 = current_length as i32 * 3 - current_off_base_bits + 1;
            if gain2 > gain1 {
                current_start = depth1_pos;
                current_offset = repeat.offset;
                current_length = repeat.length;
                current_kind = LazyMatchKind::Repeat1;
                current_off_base_bits = 0;
            }
        }

        let (regular, _) = finder.find_source_match_fast::<MLS>(
            src,
            depth1_pos,
            window_low,
            lazy_skipping,
            attempts,
        );
        let mut improved = false;
        if regular.length >= MIN_MATCH {
            let regular_off_base = explicit_offbase(regular.offset);
            let regular_off_base_bits = highbit32(regular_off_base) as i32;
            let gain2 = regular.length as i32 * 4 - regular_off_base_bits;
            let gain1 = current_length as i32 * 4 - current_off_base_bits + 4;
            if gain2 > gain1 {
                current_start = depth1_pos;
                current_offset = regular.offset;
                current_length = regular.length;
                current_kind = LazyMatchKind::Regular;
                current_off_base_bits = regular_off_base_bits;
                improved = true;
            }
        }
        if improved {
            probe_pos = depth1_pos;
            continue;
        }
        if depth != 2 {
            break;
        }

        // Depth 2 probe
        let depth2_pos = depth1_pos + 1;
        if depth2_pos > limit {
            break;
        }
        let window_low = match_floor.at(depth2_pos);

        let repeat = repeat_match_without_prefix_at(src, depth2_pos, rep1, window_low);
        if let Some(repeat) = repeat {
            let gain2 = repeat.length as i32 * 4;
            let gain1 = current_length as i32 * 4 - current_off_base_bits + 1;
            if gain2 > gain1 {
                current_start = depth2_pos;
                current_offset = repeat.offset;
                current_length = repeat.length;
                current_kind = LazyMatchKind::Repeat1;
                current_off_base_bits = 0;
            }
        }

        let (regular, _) = finder.find_source_match_fast::<MLS>(
            src,
            depth2_pos,
            window_low,
            lazy_skipping,
            attempts,
        );
        let mut improved = false;
        if regular.length >= MIN_MATCH {
            let regular_off_base = explicit_offbase(regular.offset);
            let regular_off_base_bits = highbit32(regular_off_base) as i32;
            // C: `gain1 = matchLength*4 - highbit32(offBase) + 7` in the depth-2
            // arm of `ZSTD_compressBlock_lazy_generic`. The bias grows with depth
            // (4 at depth 1, 7 at depth 2) because deferring the match another
            // byte costs another literal; a depth-2 candidate has to clear a
            // higher bar than a depth-1 one. Using the depth-1 bias here let
            // candidates through that C rejects, which defers matches and spends
            // the difference on literals.
            let gain2 = regular.length as i32 * 4 - regular_off_base_bits;
            let gain1 = current_length as i32 * 4 - current_off_base_bits + 7;
            if gain2 > gain1 {
                current_start = depth2_pos;
                current_offset = regular.offset;
                current_length = regular.length;
                current_kind = LazyMatchKind::Regular;
                current_off_base_bits = regular_off_base_bits;
                improved = true;
            }
        }
        if !improved {
            break;
        }
        probe_pos = depth2_pos;
    }

    LazyParserMatch {
        start: current_start,
        offset: current_offset,
        length: current_length,
        kind: current_kind,
    }
}

/// Fast store-match-and-chain — no PROFILE overhead.
/// Returns (next_pos, offset_1, offset_2) to keep offsets in registers
/// without re-extracting from RepeatOffsets struct.
#[inline(always)]
fn row_no_dict_store_match_and_chain_rep2_fast<const MLS: u32>(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    best: LazyParserMatch,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
    lazy_skipping: &mut bool,
    limit: usize,
) -> (usize, usize, usize) {
    row_no_dict_store_best_match_fast(plan, src, anchor, repeat_offsets, best);
    if *lazy_skipping {
        finder.refill_hash_cache::<MLS>(src, limit);
        *lazy_skipping = false;
    }
    let mut next_pos = *anchor;
    while next_pos <= limit {
        // The chain advances `next_pos` itself, so the floor moves with it.
        let window_low = match_floor.at(next_pos);
        let rep2 = repeat_offsets.values()[1] as usize;
        if rep2 == 0 || rep2 > next_pos || next_pos + MIN_MATCH > src.len() {
            break;
        }
        let match_start = next_pos - rep2;
        if match_start < window_low || !src_match_has_length(src, match_start, next_pos, MIN_MATCH)
        {
            break;
        }
        let match_length = count_match_length(src, match_start, next_pos);
        repeat_offsets.resolve_zero_literal_rep2_encode();
        push_lazy_sequence_no_trace(
            plan,
            src,
            anchor,
            SequenceCommand {
                literal_length: 0,
                offset_value: 1,
                match_length: match_length.min(u32::MAX as usize) as u32,
            },
        );
        next_pos = *anchor;
    }
    let [o1, o2, _] = repeat_offsets.values();
    (next_pos, o1 as usize, o2 as usize)
}

#[inline]
fn row_no_dict_trace_continue_step(
    probe_trace: &mut SequenceTraceRowLazyProbe,
    pos: usize,
    repeat: Option<MatchCandidate>,
    rep_improved: bool,
    regular: Option<MatchCandidate>,
    regular_improved: bool,
    current: LazyParserMatch,
) {
    if probe_trace.continue_step_count >= ROW_LAZY_TRACE_MAX_STEPS {
        return;
    }
    let index = probe_trace.continue_step_count;
    probe_trace.continue_step_count += 1;
    probe_trace.continue_positions[index] = pos;
    probe_trace.continue_rep_lengths[index] = repeat.map_or(0, |candidate| candidate.length);
    probe_trace.continue_rep_improved[index] = rep_improved;
    probe_trace.continue_regular_lengths[index] = regular.map_or(0, |candidate| candidate.length);
    probe_trace.continue_regular_off_bases[index] =
        regular.map_or(0, |candidate| explicit_offbase(candidate.offset) as usize);
    probe_trace.continue_regular_improved[index] = regular_improved;
    probe_trace.continue_current_kinds[index] = match current.kind {
        LazyMatchKind::Repeat1 => SequenceTraceEmissionKind::Rep,
        LazyMatchKind::Regular => SequenceTraceEmissionKind::Regular,
    };
    probe_trace.continue_current_starts[index] = current.start;
    probe_trace.continue_current_lengths[index] = current.length;
    probe_trace.continue_current_off_bases[index] = current.offbase() as usize;
}

fn row_no_dict_find_lazy_match_core<const TRACE: bool, const PROFILE: bool, const MLS: u32>(
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
    lazy_skipping: bool,
    limit: usize,
    depth: usize,
    anchor: usize,
    rep1: usize,
    rep2: usize,
    trace_row_searches: &mut Vec<SequenceTraceRowSearch>,
    trace_row_lazy_probes: &mut Vec<SequenceTraceRowLazyProbe>,
    timings: Option<&mut PlanningIterationTimings>,
) -> Option<LazyParserMatch> {
    let mut timings = timings;
    // C evaluates the floor at the position it is searching from, so every
    // probe below resolves its own rather than sharing this one.
    let window_low = match_floor.at(pos);
    let mut stop_reason = SequenceTraceRowLazyStopReason::None;
    let baseline_rep_start = PROFILE.then(Instant::now);
    let baseline_rep_snapshot =
        PlanningIterationCategorySnapshot::capture(PROFILE.then_some(timings.as_deref()).flatten());
    let baseline_rep = row_no_dict_repeat_match_candidate_core::<PROFILE>(
        src,
        pos,
        rep1,
        window_low,
        true,
        timings.as_deref_mut(),
    );
    let mut probe_trace = SequenceTraceRowLazyProbe::default();
    if TRACE {
        probe_trace = SequenceTraceRowLazyProbe {
            pos,
            anchor,
            offset_1: rep1,
            offset_2: rep2,
            baseline_rep_length: baseline_rep.map_or(0, |candidate| candidate.length),
            ..SequenceTraceRowLazyProbe::default()
        };
    }
    let baseline_rep_length = baseline_rep.map_or(0, |candidate| candidate.length);
    let mut current_start = pos + 1;
    let mut current_offset = baseline_rep.map_or(rep1, |candidate| candidate.offset);
    let mut current_length = baseline_rep_length;
    let mut current_kind = LazyMatchKind::Repeat1;
    let mut current_off_base = 1u32;
    let mut current_off_base_bits = 0i32; // highbit32(1) = 0
    if PROFILE {
        record_lazy_parser_phase(
            timings.as_deref_mut(),
            LazyParserPhase::BaselineRep,
            baseline_rep_start,
            baseline_rep_snapshot,
        );
    }

    if depth == 0 && anchor == pos && rep1 == pos && baseline_rep_length >= MIN_MATCH {
        if TRACE {
            probe_trace.stop_reason = SequenceTraceRowLazyStopReason::Depth0;
            probe_trace.chosen_kind = SequenceTraceEmissionKind::Rep;
            probe_trace.chosen_start = current_start;
            probe_trace.chosen_length = current_length;
            probe_trace.chosen_off_base = 1;
            probe_trace.literal_length = current_start.saturating_sub(anchor);
            trace_row_lazy_probes.push(probe_trace);
        }
        return Some(LazyParserMatch {
            start: current_start,
            offset: current_offset,
            length: current_length,
            kind: current_kind,
        });
    }

    let baseline_regular_start = PROFILE.then(Instant::now);
    let baseline_regular_snapshot =
        PlanningIterationCategorySnapshot::capture(PROFILE.then_some(timings.as_deref()).flatten());
    let baseline_regular = row_no_dict_regular_match_core::<TRACE, PROFILE, MLS>(
        src,
        pos,
        params,
        window_low,
        finder,
        lazy_skipping,
        trace_row_searches,
        timings.as_deref_mut(),
    );
    if TRACE {
        probe_trace.baseline_regular = trace_row_searches
            .last()
            .copied()
            .filter(|search| search.pos == pos)
            .unwrap_or_default();
    }
    if let Some(regular) = baseline_regular.filter(|regular| {
        !((depth == 0) && baseline_rep_length >= MIN_MATCH) && regular.length > current_length
    }) {
        current_start = pos;
        current_offset = regular.offset;
        current_length = regular.length;
        current_kind = LazyMatchKind::Regular;
        current_off_base = explicit_offbase(regular.offset);
        current_off_base_bits = highbit32(current_off_base) as i32;
    }
    if PROFILE {
        record_lazy_parser_phase(
            timings.as_deref_mut(),
            LazyParserPhase::BaselineRegular,
            baseline_regular_start,
            baseline_regular_snapshot,
        );
    }
    if depth == 0 {
        if current_length < MIN_MATCH {
            if TRACE {
                probe_trace.stop_reason = SequenceTraceRowLazyStopReason::NoBaseline;
                trace_row_lazy_probes.push(probe_trace);
            }
            return None;
        }
        if TRACE {
            probe_trace.stop_reason = SequenceTraceRowLazyStopReason::Depth0;
            probe_trace.chosen_kind = match current_kind {
                LazyMatchKind::Repeat1 => SequenceTraceEmissionKind::Rep,
                LazyMatchKind::Regular => SequenceTraceEmissionKind::Regular,
            };
            probe_trace.chosen_start = current_start;
            probe_trace.chosen_length = current_length;
            probe_trace.chosen_off_base = match current_kind {
                LazyMatchKind::Repeat1 => 1,
                LazyMatchKind::Regular => current_off_base as usize,
            };
            probe_trace.literal_length = current_start.saturating_sub(anchor);
            trace_row_lazy_probes.push(probe_trace);
        }
        return Some(LazyParserMatch {
            start: current_start,
            offset: current_offset,
            length: current_length,
            kind: current_kind,
        });
    }
    if current_length < MIN_MATCH {
        if TRACE {
            probe_trace.stop_reason = SequenceTraceRowLazyStopReason::NoBaseline;
            trace_row_lazy_probes.push(probe_trace);
        }
        return None;
    }

    if depth >= 1 {
        let mut probe_pos = pos;
        while probe_pos < limit {
            let depth1_pos = probe_pos + 1;
            if depth1_pos > limit {
                stop_reason = SequenceTraceRowLazyStopReason::Limit;
                break;
            }
            let phase_start = PROFILE.then(Instant::now);
            let snapshot = PlanningIterationCategorySnapshot::capture(
                PROFILE.then_some(timings.as_deref()).flatten(),
            );
            let repeat = row_no_dict_repeat_match_candidate_core::<PROFILE>(
                src,
                depth1_pos,
                rep1,
                window_low,
                false,
                timings.as_deref_mut(),
            );
            if TRACE && probe_pos == pos {
                probe_trace.depth1_rep_length = repeat.map_or(0, |candidate| candidate.length);
            }
            let mut rep_improved = false;
            if let Some(repeat) = repeat {
                let gain2 = repeat.length as i32 * 3;
                let gain1 = current_length as i32 * 3 - current_off_base_bits + 1;
                if gain2 > gain1 {
                    current_start = depth1_pos;
                    current_offset = repeat.offset;
                    current_length = repeat.length;
                    current_kind = LazyMatchKind::Repeat1;
                    current_off_base = 1;
                    current_off_base_bits = 0;
                    rep_improved = true;
                }
            }

            let regular = row_no_dict_regular_match_core::<TRACE, PROFILE, MLS>(
                src,
                depth1_pos,
                params,
                window_low,
                finder,
                lazy_skipping,
                trace_row_searches,
                timings.as_deref_mut(),
            );
            if TRACE && probe_pos == pos {
                probe_trace.depth1_regular_length = regular.map_or(0, |candidate| candidate.length);
                probe_trace.depth1_regular_off_base =
                    regular.map_or(0, |candidate| explicit_offbase(candidate.offset) as usize);
            }
            let mut improved = false;
            if let Some(regular) = regular {
                let regular_off_base = explicit_offbase(regular.offset);
                let gain2 = regular.length as i32 * 4 - highbit32(regular_off_base) as i32;
                let gain1 = current_length as i32 * 4 - current_off_base_bits + 4;
                if gain2 > gain1 {
                    current_start = depth1_pos;
                    current_offset = regular.offset;
                    current_length = regular.length;
                    current_kind = LazyMatchKind::Regular;
                    current_off_base = regular_off_base;
                    current_off_base_bits = highbit32(regular_off_base) as i32;
                    improved = true;
                }
            }
            if PROFILE {
                record_lazy_parser_phase(
                    timings.as_deref_mut(),
                    LazyParserPhase::Continue,
                    phase_start,
                    snapshot,
                );
            }
            if TRACE {
                row_no_dict_trace_continue_step(
                    &mut probe_trace,
                    depth1_pos,
                    repeat,
                    rep_improved,
                    regular,
                    improved,
                    LazyParserMatch {
                        start: current_start,
                        offset: current_offset,
                        length: current_length,
                        kind: current_kind,
                    },
                );
            }
            if improved {
                probe_pos = depth1_pos;
                continue;
            }

            if depth != 2 {
                stop_reason = SequenceTraceRowLazyStopReason::NoRegularImprove;
                break;
            }

            let depth2_pos = depth1_pos + 1;
            if depth2_pos > limit {
                stop_reason = SequenceTraceRowLazyStopReason::Limit;
                break;
            }
            let phase_start = PROFILE.then(Instant::now);
            let snapshot = PlanningIterationCategorySnapshot::capture(
                PROFILE.then_some(timings.as_deref()).flatten(),
            );
            let repeat = row_no_dict_repeat_match_candidate_core::<PROFILE>(
                src,
                depth2_pos,
                rep1,
                window_low,
                false,
                timings.as_deref_mut(),
            );
            if TRACE && probe_pos == pos {
                probe_trace.depth2_rep_length = repeat.map_or(0, |candidate| candidate.length);
            }
            let mut rep_improved = false;
            if let Some(repeat) = repeat {
                let gain2 = repeat.length as i32 * 4;
                let gain1 = current_length as i32 * 4 - current_off_base_bits + 1;
                if gain2 > gain1 {
                    current_start = depth2_pos;
                    current_offset = repeat.offset;
                    current_length = repeat.length;
                    current_kind = LazyMatchKind::Repeat1;
                    current_off_base = 1;
                    current_off_base_bits = 0;
                    rep_improved = true;
                }
            }

            let regular = row_no_dict_regular_match_core::<TRACE, PROFILE, MLS>(
                src,
                depth2_pos,
                params,
                window_low,
                finder,
                lazy_skipping,
                trace_row_searches,
                timings.as_deref_mut(),
            );
            if TRACE && probe_pos == pos {
                probe_trace.depth2_regular_length = regular.map_or(0, |candidate| candidate.length);
                probe_trace.depth2_regular_off_base =
                    regular.map_or(0, |candidate| explicit_offbase(candidate.offset) as usize);
            }
            let mut improved = false;
            if let Some(regular) = regular {
                let regular_off_base = explicit_offbase(regular.offset);
                let gain2 = regular.length as i32 * 4 - highbit32(regular_off_base) as i32;
                let gain1 = current_length as i32 * 4 - current_off_base_bits + 7;
                if gain2 > gain1 {
                    current_start = depth2_pos;
                    current_offset = regular.offset;
                    current_length = regular.length;
                    current_kind = LazyMatchKind::Regular;
                    current_off_base = regular_off_base;
                    current_off_base_bits = highbit32(regular_off_base) as i32;
                    improved = true;
                }
            }
            if PROFILE {
                record_lazy_parser_phase(
                    timings.as_deref_mut(),
                    LazyParserPhase::Continue,
                    phase_start,
                    snapshot,
                );
            }
            if TRACE {
                row_no_dict_trace_continue_step(
                    &mut probe_trace,
                    depth2_pos,
                    repeat,
                    rep_improved,
                    regular,
                    improved,
                    LazyParserMatch {
                        start: current_start,
                        offset: current_offset,
                        length: current_length,
                        kind: current_kind,
                    },
                );
            }
            if improved {
                probe_pos = depth2_pos;
                continue;
            }

            stop_reason = SequenceTraceRowLazyStopReason::NoRegularImprove;
            break;
        }
    }

    if TRACE {
        if stop_reason == SequenceTraceRowLazyStopReason::None {
            stop_reason = SequenceTraceRowLazyStopReason::Limit;
        }
        probe_trace.stop_reason = stop_reason;
        probe_trace.chosen_kind = match current_kind {
            LazyMatchKind::Repeat1 => SequenceTraceEmissionKind::Rep,
            LazyMatchKind::Regular => SequenceTraceEmissionKind::Regular,
        };
        probe_trace.chosen_start = current_start;
        probe_trace.chosen_length = current_length;
        probe_trace.chosen_off_base = match current_kind {
            LazyMatchKind::Repeat1 => 1,
            LazyMatchKind::Regular => current_off_base as usize,
        };
        probe_trace.literal_length = current_start.saturating_sub(anchor);
        trace_row_lazy_probes.push(probe_trace);
    }
    Some(LazyParserMatch {
        start: current_start,
        offset: current_offset,
        length: current_length,
        kind: current_kind,
    })
}

pub(crate) fn row_no_dict_store_repeat1_match(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    best: LazyParserMatch,
) -> Result<()> {
    store_lazy_sequence(
        plan,
        src,
        anchor,
        repeat_offsets,
        best.start,
        best.offset,
        best.length,
    )
}

pub(crate) fn row_no_dict_store_regular_match(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    best: LazyParserMatch,
) -> Result<()> {
    let found = extend_back_source_match(
        src,
        *anchor,
        DoubleFastMatch {
            start: best.start,
            offset: best.offset,
            length: best.length,
        },
    );
    // The rep-detecting store, matching `row_no_dict_store_best_match_fast`:
    // a regular match whose distance equals a live repeat offset is emitted as
    // that repcode. This must stay the same store the no-trace path uses, or
    // `traced_and_untraced_lazy_planners_agree_on_the_same_block` will say so.
    // It also labels the emission from the offset value rather than forcing
    // `Regular`, so a substituted sequence traces as the `Rep` it now is.
    store_lazy_sequence(
        plan,
        src,
        anchor,
        repeat_offsets,
        found.start,
        found.offset,
        found.length,
    )
}

/// Fast-path store without Result — only used from the no-trace production path.
///
/// Both kinds take the same store, and deliberately so: a regular match whose
/// distance happens to equal a live repeat offset is emitted as the repcode,
/// which costs an offset code of 0 or 1 instead of a full explicit offset with
/// its extra bits. C's lazy family cannot do this — `ZSTD_compressBlock_lazy_generic`
/// stores the `offBase` its search produced without ever comparing it against
/// `offset_1/2/3`, so the only repcode it emits is the one its own rep probe
/// found. See "The repcode substitution" in `docs/PARITY_PLAN.md`; the same
/// substitution is what btlazy2 has been doing since before it was understood.
#[inline(always)]
fn row_no_dict_store_best_match_fast(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    best: LazyParserMatch,
) {
    let found = match best.kind {
        LazyMatchKind::Repeat1 => DoubleFastMatch {
            start: best.start,
            offset: best.offset,
            length: best.length,
        },
        LazyMatchKind::Regular => extend_back_source_match(
            src,
            *anchor,
            DoubleFastMatch {
                start: best.start,
                offset: best.offset,
                length: best.length,
            },
        ),
    };
    let literal_length = found.start - *anchor;
    let offset_value = repeat_offsets.encode_offset_value_and_update(
        found.offset.min(u32::MAX as usize) as u32,
        literal_length.min(u32::MAX as usize) as u32,
    );
    push_lazy_sequence_no_trace(
        plan,
        src,
        anchor,
        SequenceCommand {
            literal_length: literal_length.min(u32::MAX as usize) as u32,
            offset_value,
            match_length: found.length.min(u32::MAX as usize) as u32,
        },
    );
}

#[inline(always)]
pub(crate) fn row_no_dict_store_best_match(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    best: LazyParserMatch,
) -> Result<()> {
    if !plan.tracing_enabled() {
        // No-trace fast path: skip all tracing state capture and emission.
        row_no_dict_store_best_match_fast(plan, src, anchor, repeat_offsets, best);
        return Ok(());
    }
    match best.kind {
        LazyMatchKind::Repeat1 => {
            row_no_dict_store_repeat1_match(plan, src, anchor, repeat_offsets, best)
        }
        LazyMatchKind::Regular => {
            row_no_dict_store_regular_match(plan, src, anchor, repeat_offsets, best)
        }
    }
}

fn row_no_dict_store_match_and_chain_rep2_core<const PROFILE: bool, const MLS: u32>(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    best: LazyParserMatch,
    match_floor: MatchFloor,
    finder: &mut RowHashFinder,
    lazy_skipping: &mut bool,
    limit: usize,
    timings: Option<&mut PlanningIterationTimings>,
) -> Result<usize> {
    let mut timings = timings;
    let store_start = PROFILE.then(Instant::now);
    let store_snapshot =
        PlanningIterationCategorySnapshot::capture(PROFILE.then_some(timings.as_deref()).flatten());
    row_no_dict_store_best_match(plan, src, anchor, repeat_offsets, best)?;
    if PROFILE {
        record_lazy_parser_phase(
            timings.as_deref_mut(),
            LazyParserPhase::Store,
            store_start,
            store_snapshot,
        );
    }
    if *lazy_skipping {
        let insert_start = PROFILE.then(Instant::now);
        finder.refill_hash_cache::<MLS>(src, limit);
        if let (Some(timings), Some(insert_start)) = (timings.as_deref_mut(), insert_start) {
            timings.insert_update += insert_start.elapsed();
        }
        *lazy_skipping = false;
    }
    let rep2_start = PROFILE.then(Instant::now);
    let rep2_snapshot =
        PlanningIterationCategorySnapshot::capture(PROFILE.then_some(timings.as_deref()).flatten());
    let mut next_pos = *anchor;
    let tracing = plan.tracing_enabled();
    while next_pos <= limit {
        // The chain advances `next_pos` itself, so the floor moves with it.
        let window_low = match_floor.at(next_pos);
        let rep2 = repeat_offsets.values()[1] as usize;
        let rep_check_start = PROFILE.then(Instant::now);
        let Some(match_length) = (|| {
            if rep2 == 0 || rep2 > next_pos || next_pos + MIN_MATCH > src.len() {
                return None;
            }
            let match_start = next_pos - rep2;
            if match_start < window_low
                || !src_match_has_length(src, match_start, next_pos, MIN_MATCH)
            {
                return None;
            }
            Some(count_match_length(src, match_start, next_pos))
        })() else {
            if let (Some(timings), Some(rep_check_start)) =
                (timings.as_deref_mut(), rep_check_start)
            {
                timings.rep_check += rep_check_start.elapsed();
            }
            break;
        };
        if let (Some(timings), Some(rep_check_start)) = (timings.as_deref_mut(), rep_check_start) {
            timings.rep_check += rep_check_start.elapsed();
        }
        if !tracing {
            // No-trace fast path: swap rep offsets and push directly.
            repeat_offsets.resolve_zero_literal_rep2_encode();
            push_lazy_sequence_no_trace(
                plan,
                src,
                anchor,
                SequenceCommand {
                    literal_length: 0,
                    offset_value: 1,
                    match_length: match_length.min(u32::MAX as usize) as u32,
                },
            );
        } else {
            store_lazy_zero_literal_rep2_with_source(
                plan,
                src,
                anchor,
                repeat_offsets,
                match_length,
                SequenceTraceMatchSource::Unknown,
            )?;
        }
        next_pos = *anchor;
    }
    if PROFILE {
        record_lazy_parser_phase(timings, LazyParserPhase::Rep2, rep2_start, rep2_snapshot);
    }
    Ok(next_pos)
}

#[inline(always)]
pub(crate) fn row_no_dict_regular_match_core<
    const TRACE: bool,
    const PROFILE: bool,
    const MLS: u32,
>(
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    window_low: usize,
    finder: &mut RowHashFinder,
    lazy_skipping: bool,
    trace_row_searches: &mut Vec<SequenceTraceRowSearch>,
    timings: Option<&mut PlanningIterationTimings>,
) -> Option<MatchCandidate> {
    let attempts = finder.search_attempt_budget(params.search_log);
    // Fast path: skip the 200-byte RowMatchBufferTrace return and timings
    // parameter when neither tracing nor profiling is active.
    if !TRACE && !PROFILE {
        let (candidate, _attempts_left) =
            finder.find_source_match_fast::<MLS>(src, pos, window_low, lazy_skipping, attempts);
        return (candidate.length >= MIN_MATCH).then_some(candidate);
    }
    let (candidate, attempts_left, trace) = finder
        .find_source_match_with_budget_core::<TRACE, PROFILE, MLS>(
            src,
            pos,
            window_low,
            lazy_skipping,
            attempts,
            timings,
        );
    if TRACE {
        trace_row_searches.push(SequenceTraceRowSearch {
            pos,
            next_to_update_before_search: trace.next_to_update_before_search,
            hash: trace.hash,
            rel_row: trace.rel_row,
            tag: trace.tag,
            low_limit: trace.low_limit,
            attempt_budget: trace.attempt_budget,
            head_index: trace.head_index,
            insert_index: trace.insert_index,
            group_width: trace.group_width,
            source_match_count: trace.num_matches,
            source_match_positions: trace.match_positions,
            source_match_indices: trace.match_indices,
            source_visit_count: trace.visit_count,
            source_visit_positions: trace.visit_positions,
            source_visit_indices: trace.visit_indices,
            source_visit_lengths: trace.visit_lengths,
            source_visit_gate_passes: trace.visit_gate_passes,
            source_visit_winner_lengths: trace.visit_winner_lengths,
            source_visit_winner_off_bases: trace.visit_winner_off_bases,
            source_length: candidate.map_or(0, |candidate| candidate.length),
            source_offset: candidate.map_or(0, |candidate| candidate.offset),
            dict_length: 0,
            dict_offset: 0,
            attempts_left_before_dict: attempts_left,
            winner: candidate
                .map(|_| SequenceTraceMatchSource::Source)
                .unwrap_or(SequenceTraceMatchSource::Unknown),
        });
    }
    candidate
}

pub(crate) fn row_dict_match_state_repeat_ahead_match(
    prefix: &[u8],
    src: &[u8],
    pos: usize,
    raw_offset: usize,
    prefix_low: usize,
    source_low: usize,
) -> Option<MatchCandidate> {
    let match_start =
        prefixed_offset_match_start(prefix.len(), pos + 1, raw_offset, prefix_low, source_low)?;
    if !logical_match_has_length(prefix, src, match_start, prefix.len() + pos + 1, MIN_MATCH) {
        return None;
    }
    Some(MatchCandidate {
        offset: raw_offset,
        length: prefixed_match_length_at(prefix, src, match_start, pos + 1, MIN_MATCH),
    })
}

pub(crate) fn row_dict_match_state_repeat_match_at(
    prefix: &[u8],
    src: &[u8],
    pos: usize,
    raw_offset: usize,
    prefix_low: usize,
    source_low: usize,
) -> Option<MatchCandidate> {
    let match_start =
        prefixed_offset_match_start(prefix.len(), pos, raw_offset, prefix_low, source_low)?;
    if !logical_match_has_length(prefix, src, match_start, prefix.len() + pos, MIN_MATCH) {
        return None;
    }
    Some(MatchCandidate {
        offset: raw_offset,
        length: prefixed_match_length_at(prefix, src, match_start, pos, MIN_MATCH),
    })
}

pub(crate) fn best_row_ext_dict_regular_match(
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    prefix_low: usize,
    source_low: usize,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
    lazy_skipping: bool,
    timings: Option<&mut PlanningIterationTimings>,
) -> Option<(MatchCandidate, SequenceTraceMatchSource)> {
    let attempts = src_finder.search_attempt_budget(params.search_log);
    let (src_regular, attempts_left, _) = src_finder.find_source_match_with_budget(
        src,
        pos,
        source_low,
        lazy_skipping,
        attempts,
        false,
        timings,
    );
    let mut best = src_regular;
    let mut best_source = src_regular.map(|_| SequenceTraceMatchSource::Source);
    if let Some(prefix_regular) = prefix_finder.find_ext_dict_match_with_budget(
        prefix_chain,
        src,
        pos,
        prefix_low,
        attempts_left,
        best.map_or(MIN_MATCH - 1, |candidate| candidate.length),
    ) {
        best = Some(prefix_regular);
        best_source = Some(SequenceTraceMatchSource::Prefix);
    }

    best.map(|candidate| {
        (
            candidate,
            best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
        )
    })
}

pub(crate) fn best_row_dict_match_state_regular_match_core<
    const TRACE: bool,
    const PROFILE: bool,
>(
    prefix: &[u8],
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    prefix_low: usize,
    source_low: usize,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
    lazy_skipping: bool,
    trace_first_row_contest: &mut Option<SequenceTraceRowSearchContest>,
    trace_row_searches: &mut Vec<SequenceTraceRowSearch>,
    timings: Option<&mut PlanningIterationTimings>,
) -> Option<(MatchCandidate, SequenceTraceMatchSource)> {
    let attempts = src_finder.search_attempt_budget(params.search_log);
    let (src_regular, attempts_left, src_trace) = src_finder.find_source_match_with_budget(
        src,
        pos,
        source_low,
        lazy_skipping,
        attempts,
        TRACE,
        timings,
    );
    let mut best = src_regular;
    let mut best_source = src_regular.map(|_| SequenceTraceMatchSource::Source);
    let dict_regular = prefix_finder.find_dict_match_state_match_with_budget(
        prefix,
        src,
        pos,
        prefix_low,
        attempts_left,
        MIN_MATCH - 1,
    );
    if let Some(dict_regular) = dict_regular
        .filter(|candidate| best.is_none_or(|current| candidate.length > current.length))
    {
        best = Some(dict_regular);
        best_source = Some(SequenceTraceMatchSource::Dict);
    }
    if TRACE && trace_first_row_contest.is_none() {
        if let (Some(source_candidate), Some(dict_candidate)) = (src_regular, dict_regular) {
            *trace_first_row_contest = Some(SequenceTraceRowSearchContest {
                winner: if dict_candidate.length > source_candidate.length {
                    SequenceTraceMatchSource::Dict
                } else {
                    SequenceTraceMatchSource::Source
                },
                source_length: source_candidate.length,
                dict_length: dict_candidate.length,
                attempts_left_before_dict: attempts_left,
            });
        }
    }
    if TRACE {
        trace_row_searches.push(SequenceTraceRowSearch {
            pos,
            next_to_update_before_search: src_trace.next_to_update_before_search,
            hash: src_trace.hash,
            rel_row: src_trace.rel_row,
            tag: src_trace.tag,
            low_limit: src_trace.low_limit,
            attempt_budget: src_trace.attempt_budget,
            head_index: src_trace.head_index,
            insert_index: src_trace.insert_index,
            group_width: src_trace.group_width,
            source_match_count: src_trace.num_matches,
            source_match_positions: src_trace.match_positions,
            source_match_indices: src_trace.match_indices,
            source_visit_count: src_trace.visit_count,
            source_visit_positions: src_trace.visit_positions,
            source_visit_indices: src_trace.visit_indices,
            source_visit_lengths: src_trace.visit_lengths,
            source_visit_gate_passes: src_trace.visit_gate_passes,
            source_visit_winner_lengths: src_trace.visit_winner_lengths,
            source_visit_winner_off_bases: src_trace.visit_winner_off_bases,
            source_length: src_regular.map_or(0, |candidate| candidate.length),
            source_offset: src_regular.map_or(0, |candidate| candidate.offset),
            dict_length: dict_regular.map_or(0, |candidate| candidate.length),
            dict_offset: dict_regular.map_or(0, |candidate| candidate.offset),
            attempts_left_before_dict: attempts_left,
            winner: best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
        });
    }

    best.map(|candidate| {
        (
            candidate,
            best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
        )
    })
}

fn row_dict_match_state_find_lazy_match_core<const TRACE: bool, const PROFILE: bool>(
    prefix: &[u8],
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
    lazy_skipping: bool,
    limit: usize,
    offset_1: usize,
    trace_first_row_contest: &mut Option<SequenceTraceRowSearchContest>,
    trace_row_searches: &mut Vec<SequenceTraceRowSearch>,
    timings: Option<&mut PlanningIterationTimings>,
) -> Option<(LazyParserMatch, SequenceTraceMatchSource)> {
    let (prefix_low, source_low) = match_floor.at(pos);
    let mut timings = timings;
    let depth = params.lazy_search_depth.min(2);
    let rep_start = PROFILE.then(Instant::now);
    let mut best =
        row_dict_match_state_repeat_ahead_match(prefix, src, pos, offset_1, prefix_low, source_low)
            .map(|candidate| LazyParserMatch {
                start: pos + 1,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Repeat1,
            });
    if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
        timings.rep_check += rep_start.elapsed();
    }
    let mut best_source = best.map(|_| SequenceTraceMatchSource::Rep);
    // Greedy takes the depth-0 repeat immediately. C's `ZSTD_compressBlock_lazy_generic`
    // runs its `isDxS` rep check before the first search and jumps straight out on a hit
    // -- `if (depth==0) goto _storeSequence;`, `zstd_lazy.c:1597` -- so the search never
    // runs and cannot overrule the repeat. Letting it run and win on raw length picks a
    // longer match at a worse offset: a rep costs one offset code where an explicit
    // offset costs its own bits, so the longer match is frequently the more expensive
    // sequence. The chain parser learned this already (see the ext-dict path above); both
    // row paths were still comparing, which cost 1.07x to 1.19x of upstream's size at the
    // greedy levels while lazy and lazy2 through the same finder stayed within 0.03%.
    //
    // Skipping the search also skips the row insert it would have done, which is again
    // what C does: the update lives inside `ZSTD_RowFindBestMatch`, so a `goto` past the
    // search leaves `nextToUpdate` behind and the next search catches up.
    let take_repeat_immediately = depth == 0 && best.is_some();
    if !take_repeat_immediately
        && let Some((candidate, source)) =
            best_row_dict_match_state_regular_match_core::<TRACE, PROFILE>(
                prefix,
                src,
                pos,
                params,
                prefix_low,
                source_low,
                prefix_finder,
                src_finder,
                lazy_skipping,
                trace_first_row_contest,
                trace_row_searches,
                timings.as_deref_mut(),
            )
    {
        let regular = LazyParserMatch {
            start: pos,
            offset: candidate.offset,
            length: candidate.length,
            kind: LazyMatchKind::Regular,
        };
        if best.is_none_or(|current| regular.length > current.length) {
            best = Some(regular);
            best_source = Some(source);
        }
    }

    let mut best = best?;

    if depth >= 1 {
        let mut search_pos = pos;
        while search_pos < limit {
            let depth1_pos = search_pos + 1;
            let (prefix_low, source_low) = match_floor.at(depth1_pos);
            if depth1_pos >= limit {
                break;
            }

            let rep_start = PROFILE.then(Instant::now);
            if let Some(candidate) = row_dict_match_state_repeat_match_at(
                prefix, src, depth1_pos, offset_1, prefix_low, source_low,
            )
            .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 3))
            {
                best = LazyParserMatch {
                    start: depth1_pos,
                    offset: candidate.offset,
                    length: candidate.length,
                    kind: LazyMatchKind::Repeat1,
                };
                best_source = Some(SequenceTraceMatchSource::Rep);
            }
            if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
                timings.rep_check += rep_start.elapsed();
            }

            if let Some((candidate, source)) =
                best_row_dict_match_state_regular_match_core::<TRACE, PROFILE>(
                    prefix,
                    src,
                    depth1_pos,
                    params,
                    prefix_low,
                    source_low,
                    prefix_finder,
                    src_finder,
                    lazy_skipping,
                    trace_first_row_contest,
                    trace_row_searches,
                    timings.as_deref_mut(),
                )
                .filter(|(candidate, _)| lazy_regular_match_improves(best, *candidate, 4))
            {
                best = LazyParserMatch {
                    start: depth1_pos,
                    offset: candidate.offset,
                    length: candidate.length,
                    kind: LazyMatchKind::Regular,
                };
                best_source = Some(source);
                search_pos = depth1_pos;
                continue;
            }

            if depth == 2 {
                let depth2_pos = depth1_pos + 1;
                let (prefix_low, source_low) = match_floor.at(depth2_pos);
                if depth2_pos >= limit {
                    break;
                }

                let rep_start = PROFILE.then(Instant::now);
                if let Some(candidate) = row_dict_match_state_repeat_match_at(
                    prefix, src, depth2_pos, offset_1, prefix_low, source_low,
                )
                .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 4))
                {
                    best = LazyParserMatch {
                        start: depth2_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Repeat1,
                    };
                    best_source = Some(SequenceTraceMatchSource::Rep);
                }
                if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
                    timings.rep_check += rep_start.elapsed();
                }

                if let Some((candidate, source)) =
                    best_row_dict_match_state_regular_match_core::<TRACE, PROFILE>(
                        prefix,
                        src,
                        depth2_pos,
                        params,
                        prefix_low,
                        source_low,
                        prefix_finder,
                        src_finder,
                        lazy_skipping,
                        trace_first_row_contest,
                        trace_row_searches,
                        timings.as_deref_mut(),
                    )
                    .filter(|(candidate, _)| lazy_regular_match_improves(best, *candidate, 7))
                {
                    best = LazyParserMatch {
                        start: depth2_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Regular,
                    };
                    best_source = Some(source);
                    search_pos = depth2_pos;
                    continue;
                }
            }

            break;
        }
    }

    Some((
        best,
        best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
    ))
}

fn row_dict_match_state_store_match_and_chain_rep2_core<const PROFILE: bool>(
    plan: &mut SequencePlan,
    prefix: &[u8],
    src: &[u8],
    anchor: &mut usize,
    repeat_offsets: &mut RepeatOffsets,
    offset_1: &mut usize,
    offset_2: &mut usize,
    best: LazyParserMatch,
    best_source: SequenceTraceMatchSource,
    match_floor: PrefixedMatchFloor,
    src_finder: &mut RowHashFinder,
    lazy_skipping: &mut bool,
    limit: usize,
    timings: Option<&mut PlanningIterationTimings>,
) -> Result<usize> {
    let mut timings = timings;
    let anchor_before = *anchor;
    let offset_1_before = *offset_1;
    let offset_2_before = *offset_2;
    match best.kind {
        LazyMatchKind::Repeat1 => {
            let raw_offset = store_lazy_sequence_with_offset_value_and_source(
                plan,
                src,
                anchor,
                repeat_offsets,
                best.start,
                1,
                best.length,
                SequenceTraceMatchSource::Rep,
            )?;
            debug_assert_eq!(raw_offset as usize, *offset_1);
            trace_lazy_emission(
                plan,
                SequenceTraceEmissionKind::Rep,
                SequenceTraceMatchSource::Rep,
                anchor_before,
                best.start,
                best.length,
                1,
                raw_offset as usize,
                offset_1_before,
                offset_2_before,
                *offset_1,
                *offset_2,
            );
        }
        LazyMatchKind::Regular => {
            // Extending backwards from the match's own start.
            let (prefix_low, source_low) = match_floor.at(best.start);
            let found = extend_back_logical_match_with_limits(
                prefix,
                src,
                *anchor,
                DoubleFastMatch {
                    start: best.start,
                    offset: best.offset,
                    length: best.length,
                },
                prefix_low,
                source_low,
            );
            let off_base = explicit_offbase(found.offset);
            *offset_2 = *offset_1;
            *offset_1 = found.offset;
            let raw_offset = store_lazy_sequence_with_offset_value_and_source(
                plan,
                src,
                anchor,
                repeat_offsets,
                found.start,
                off_base,
                found.length,
                best_source,
            )?;
            debug_assert_eq!(raw_offset as usize, found.offset);
            trace_lazy_emission(
                plan,
                SequenceTraceEmissionKind::Regular,
                best_source,
                anchor_before,
                found.start,
                found.length,
                off_base,
                raw_offset as usize,
                offset_1_before,
                offset_2_before,
                *offset_1,
                *offset_2,
            );
        }
    }
    debug_assert!(row_dict_match_state_offsets_synced(
        *repeat_offsets,
        *offset_1,
        *offset_2
    ));

    let mut pos = *anchor;
    if *lazy_skipping {
        let insert_start = PROFILE.then(Instant::now);
        match src_finder.min_match {
            4 => src_finder.refill_hash_cache::<4>(src, limit),
            5 => src_finder.refill_hash_cache::<5>(src, limit),
            _ => src_finder.refill_hash_cache::<6>(src, limit),
        }
        if let (Some(timings), Some(insert_start)) = (timings.as_mut(), insert_start) {
            timings.insert_update += insert_start.elapsed();
        }
        *lazy_skipping = false;
    }

    // Mirror the zstd lazy-row path: after storing the chosen match, greedily
    // chain immediate rep2 matches before resuming the main search loop.
    while pos <= limit {
        // The chain advances `pos` itself, so the floor moves with it.
        let (prefix_low, source_low) = match_floor.at(pos);
        let anchor_before = *anchor;
        let offset_1_before = *offset_1;
        let offset_2_before = *offset_2;
        let rep2 = *offset_2;
        let rep_start = PROFILE.then(Instant::now);
        let Some(candidate) =
            row_dict_match_state_repeat_match_at(prefix, src, pos, rep2, prefix_low, source_low)
        else {
            if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
                timings.rep_check += rep_start.elapsed();
            }
            break;
        };
        if let (Some(timings), Some(rep_start)) = (timings.as_mut(), rep_start) {
            timings.rep_check += rep_start.elapsed();
        }
        *offset_2 = *offset_1;
        *offset_1 = rep2;
        let raw_offset = store_lazy_sequence_with_offset_value_and_source(
            plan,
            src,
            anchor,
            repeat_offsets,
            pos,
            1,
            candidate.length,
            SequenceTraceMatchSource::Rep,
        )?;
        debug_assert_eq!(raw_offset as usize, rep2);
        trace_lazy_emission(
            plan,
            SequenceTraceEmissionKind::Rep,
            SequenceTraceMatchSource::Rep,
            anchor_before,
            pos,
            candidate.length,
            1,
            raw_offset as usize,
            offset_1_before,
            offset_2_before,
            *offset_1,
            *offset_2,
        );
        debug_assert!(row_dict_match_state_offsets_synced(
            *repeat_offsets,
            *offset_1,
            *offset_2
        ));
        pos = *anchor;
    }

    Ok(pos)
}

pub(crate) fn plan_sequences_row_ext_dict_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let limit = row_search_limit(src.len());
    if block_start >= limit {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    if plan.planning_profile.is_enabled() {
        return plan_sequences_row_ext_dict_from_into_core::<true>(
            plan,
            src,
            block_start,
            prefix_chain,
            repeat_offsets,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            limit,
        );
    }
    plan_sequences_row_ext_dict_from_into_core::<false>(
        plan,
        src,
        block_start,
        prefix_chain,
        repeat_offsets,
        params,
        match_floor,
        prefix_finder,
        src_finder,
        limit,
    )
}

fn plan_sequences_row_ext_dict_from_into_core<const PROFILE: bool>(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
    limit: usize,
) -> Result<()> {
    let depth = params.lazy_search_depth.min(2);
    let mut repeat_offsets = repeat_offsets;
    let [rep1_raw, _, _] = repeat_offsets.values();
    let mut rep1 = rep1_raw as usize;
    let mut rep2;
    let mut anchor = block_start;
    let mut pos = block_start;
    let mut lazy_skipping = false;
    match src_finder.min_match {
        4 => src_finder.refill_hash_cache::<4>(src, limit),
        5 => src_finder.refill_hash_cache::<5>(src, limit),
        _ => src_finder.refill_hash_cache::<6>(src, limit),
    }

    while pos < limit {
        let (prefix_low, source_low) = match_floor.at(pos);
        let iteration_start = PROFILE.then(Instant::now);
        let mut iteration_timings = PROFILE.then(PlanningIterationTimings::default);
        let rep_start = PROFILE.then(Instant::now);
        let mut best = repeat_ahead_match_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            rep1,
            prefix_low,
            source_low,
        )
        .map(|candidate| LazyParserMatch {
            start: pos + 1,
            offset: candidate.offset,
            length: candidate.length,
            kind: LazyMatchKind::Repeat1,
        });
        if let (Some(rep_start), Some(iteration_timings)) = (rep_start, iteration_timings.as_mut())
        {
            iteration_timings.rep_check += rep_start.elapsed();
        }
        let mut best_source = best.map(|_| SequenceTraceMatchSource::Rep);
        // Greedy takes the depth-0 repeat immediately, as in the dict-match-state row
        // path: `ZSTD_compressBlock_lazy_extDict_generic` jumps out of its rep check on a
        // hit at `zstd_lazy.c:1995` rather than searching and comparing lengths.
        let take_repeat_immediately = depth == 0 && best.is_some();
        if !take_repeat_immediately
            && let Some((candidate, source)) = best_row_ext_dict_regular_match(
                prefix_chain,
                src,
                pos,
                params,
                prefix_low,
                source_low,
                prefix_finder,
                src_finder,
                lazy_skipping,
                iteration_timings.as_mut(),
            )
        {
            let regular = LazyParserMatch {
                start: pos,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Regular,
            };
            if best.is_none_or(|current| regular.length > current.length) {
                best = Some(regular);
                best_source = Some(source);
            }
        }

        let Some(mut best) = best else {
            let step = skip_after_no_match(anchor, pos, params);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = pos.saturating_add(step);
            if let (Some(iteration_start), Some(iteration_timings)) =
                (iteration_start, iteration_timings)
            {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        };

        if depth >= 1 {
            let mut probe_pos = pos;
            loop {
                let depth1_pos = probe_pos + 1;
                if depth1_pos >= limit {
                    break;
                }
                // The floor moves with the probe, as it does in C.
                let (prefix_low, source_low) = match_floor.at(depth1_pos);

                let rep_start = PROFILE.then(Instant::now);
                if let Some(candidate) = repeat_match_with_prefix_chain_at(
                    prefix_chain,
                    src,
                    depth1_pos,
                    rep1,
                    prefix_low,
                    source_low,
                )
                .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 3))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Repeat1,
                    };
                    best_source = Some(SequenceTraceMatchSource::Rep);
                }
                if let (Some(rep_start), Some(iteration_timings)) =
                    (rep_start, iteration_timings.as_mut())
                {
                    iteration_timings.rep_check += rep_start.elapsed();
                }

                if let Some((candidate, source)) = best_row_ext_dict_regular_match(
                    prefix_chain,
                    src,
                    depth1_pos,
                    params,
                    prefix_low,
                    source_low,
                    prefix_finder,
                    src_finder,
                    lazy_skipping,
                    iteration_timings.as_mut(),
                )
                .filter(|(candidate, _)| lazy_regular_match_improves(best, *candidate, 4))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Regular,
                    };
                    best_source = Some(source);
                    probe_pos = depth1_pos;
                    continue;
                }

                if depth == 2 {
                    let depth2_pos = depth1_pos + 1;
                    if depth2_pos >= limit {
                        break;
                    }
                    let (prefix_low, source_low) = match_floor.at(depth2_pos);

                    let rep_start = PROFILE.then(Instant::now);
                    if let Some(candidate) = repeat_match_with_prefix_chain_at(
                        prefix_chain,
                        src,
                        depth2_pos,
                        rep1,
                        prefix_low,
                        source_low,
                    )
                    .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 4))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Repeat1,
                        };
                        best_source = Some(SequenceTraceMatchSource::Rep);
                    }
                    if let (Some(rep_start), Some(iteration_timings)) =
                        (rep_start, iteration_timings.as_mut())
                    {
                        iteration_timings.rep_check += rep_start.elapsed();
                    }

                    if let Some((candidate, source)) = best_row_ext_dict_regular_match(
                        prefix_chain,
                        src,
                        depth2_pos,
                        params,
                        prefix_low,
                        source_low,
                        prefix_finder,
                        src_finder,
                        lazy_skipping,
                        iteration_timings.as_mut(),
                    )
                    .filter(|(candidate, _)| lazy_regular_match_improves(best, *candidate, 7))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Regular,
                        };
                        best_source = Some(source);
                        probe_pos = depth2_pos;
                        continue;
                    }
                }

                break;
            }
        }

        match best.kind {
            LazyMatchKind::Repeat1 => {
                store_lazy_sequence_with_source(
                    plan,
                    src,
                    &mut anchor,
                    &mut repeat_offsets,
                    best.start,
                    best.offset,
                    best.length,
                    SequenceTraceMatchSource::Rep,
                )?;
            }
            LazyMatchKind::Regular => {
                let found = extend_back_prefix_chain_match(
                    prefix_chain,
                    src,
                    anchor,
                    best.start,
                    best.candidate(),
                    prefix_low,
                    source_low,
                );
                store_lazy_sequence_with_source(
                    plan,
                    src,
                    &mut anchor,
                    &mut repeat_offsets,
                    found.start,
                    found.offset,
                    found.length,
                    best_source.unwrap_or(SequenceTraceMatchSource::Unknown),
                )?;
            }
        }

        pos = anchor;
        let [next_rep1, next_rep2, _] = repeat_offsets.values();
        rep1 = next_rep1 as usize;
        rep2 = next_rep2 as usize;
        if lazy_skipping {
            let insert_start = PROFILE.then(Instant::now);
            match src_finder.min_match {
                4 => src_finder.refill_hash_cache::<4>(src, limit),
                5 => src_finder.refill_hash_cache::<5>(src, limit),
                _ => src_finder.refill_hash_cache::<6>(src, limit),
            }
            if let (Some(insert_start), Some(iteration_timings)) =
                (insert_start, iteration_timings.as_mut())
            {
                iteration_timings.insert_update += insert_start.elapsed();
            }
            lazy_skipping = false;
        }

        while pos <= limit {
            let rep_start = PROFILE.then(Instant::now);
            let Some(candidate) = repeat_match_with_prefix_chain_at(
                prefix_chain,
                src,
                pos,
                rep2,
                prefix_low,
                source_low,
            ) else {
                if let (Some(rep_start), Some(iteration_timings)) =
                    (rep_start, iteration_timings.as_mut())
                {
                    iteration_timings.rep_check += rep_start.elapsed();
                }
                break;
            };
            if let (Some(rep_start), Some(iteration_timings)) =
                (rep_start, iteration_timings.as_mut())
            {
                iteration_timings.rep_check += rep_start.elapsed();
            }
            store_lazy_sequence_with_source(
                plan,
                src,
                &mut anchor,
                &mut repeat_offsets,
                pos,
                candidate.offset,
                candidate.length,
                SequenceTraceMatchSource::Rep,
            )?;
            pos = anchor;
            let [next_rep1, next_rep2, _] = repeat_offsets.values();
            rep1 = next_rep1 as usize;
            rep2 = next_rep2 as usize;
        }
        if let (Some(iteration_start), Some(iteration_timings)) =
            (iteration_start, iteration_timings)
        {
            plan.planning_profile
                .record_iteration(iteration_start.elapsed(), iteration_timings);
        }
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

pub(crate) fn plan_sequences_row_dict_match_state_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix: &[u8],
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let limit = row_search_limit(src.len());
    if block_start >= limit {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    match (plan.tracing_enabled(), plan.planning_profile.is_enabled()) {
        (true, true) => plan_sequences_row_dict_match_state_from_into_core::<true, true>(
            plan,
            src,
            block_start,
            prefix,
            repeat_offsets,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            limit,
        ),
        (true, false) => plan_sequences_row_dict_match_state_from_into_core::<true, false>(
            plan,
            src,
            block_start,
            prefix,
            repeat_offsets,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            limit,
        ),
        (false, true) => plan_sequences_row_dict_match_state_from_into_core::<false, true>(
            plan,
            src,
            block_start,
            prefix,
            repeat_offsets,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            limit,
        ),
        (false, false) => plan_sequences_row_dict_match_state_from_into_core::<false, false>(
            plan,
            src,
            block_start,
            prefix,
            repeat_offsets,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            limit,
        ),
    }
}

fn plan_sequences_row_dict_match_state_from_into_core<const TRACE: bool, const PROFILE: bool>(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix: &[u8],
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &RowHashFinder,
    src_finder: &mut RowHashFinder,
    limit: usize,
) -> Result<()> {
    let mut repeat_offsets = repeat_offsets;
    let (mut offset_1, mut offset_2) = repeat_offsets12(repeat_offsets);
    let mut anchor = block_start;
    let mut pos = block_start;
    let mut lazy_skipping = false;
    match src_finder.min_match {
        4 => src_finder.refill_hash_cache::<4>(src, limit),
        5 => src_finder.refill_hash_cache::<5>(src, limit),
        _ => src_finder.refill_hash_cache::<6>(src, limit),
    }

    while pos < limit {
        let iteration_start = PROFILE.then(Instant::now);
        let mut iteration_timings = PROFILE.then(PlanningIterationTimings::default);
        let Some((best, best_source)) = row_dict_match_state_find_lazy_match_core::<TRACE, PROFILE>(
            prefix,
            src,
            pos,
            params,
            match_floor,
            prefix_finder,
            src_finder,
            lazy_skipping,
            limit,
            offset_1,
            &mut plan.trace_first_row_contest,
            &mut plan.trace_row_searches,
            iteration_timings.as_mut(),
        ) else {
            let step = skip_after_no_match(anchor, pos, params);
            lazy_skipping = step > LAZY_SKIPPING_STEP;
            pos = pos.saturating_add(step);
            if let (Some(iteration_start), Some(iteration_timings)) =
                (iteration_start, iteration_timings)
            {
                plan.planning_profile
                    .record_iteration(iteration_start.elapsed(), iteration_timings);
            }
            continue;
        };

        pos = row_dict_match_state_store_match_and_chain_rep2_core::<PROFILE>(
            plan,
            prefix,
            src,
            &mut anchor,
            &mut repeat_offsets,
            &mut offset_1,
            &mut offset_2,
            best,
            best_source,
            match_floor,
            src_finder,
            &mut lazy_skipping,
            limit,
            iteration_timings.as_mut(),
        )?;
        if let (Some(iteration_start), Some(iteration_timings)) =
            (iteration_start, iteration_timings)
        {
            plan.planning_profile
                .record_iteration(iteration_start.elapsed(), iteration_timings);
        }
    }

    debug_assert!(row_dict_match_state_offsets_synced(
        repeat_offsets,
        offset_1,
        offset_2
    ));
    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

pub(crate) fn plan_sequences_lazy_without_prefix_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut impl LazySearchFinder,
) -> Result<()> {
    if plan.tracing_enabled() {
        plan_sequences_lazy_without_prefix_tracing(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
        )
    } else {
        plan_sequences_lazy_without_prefix_no_trace(
            plan,
            src,
            block_start,
            repeat_offsets,
            params,
            match_floor,
            finder,
        );
        Ok(())
    }
}

/// Store a lazy sequence using local rep offset variables (no-trace path).
/// Computes offset_value from local reps, pushes via push_lazy_sequence_no_trace.
#[inline(always)]
fn store_lazy_sequence_local_reps(
    plan: &mut SequencePlan,
    src: &[u8],
    anchor: &mut usize,
    rep1: &mut usize,
    rep2: &mut usize,
    rep3: &mut usize,
    start: usize,
    raw_offset: usize,
    match_length: usize,
) {
    let literal_length = start - *anchor;
    let ll_zero = literal_length == 0;
    let raw_offset_u32 = raw_offset.min(u32::MAX as usize) as u32;
    let r1 = *rep1 as u32;
    let r2 = *rep2 as u32;
    let r3 = *rep3 as u32;

    let offset_value = if !ll_zero && raw_offset_u32 == r1 {
        // rep1 with literals: no state change
        1
    } else if raw_offset_u32 == r2 {
        // rep2 match: swap rep1/rep2, preserve rep3
        *rep2 = *rep1;
        *rep1 = raw_offset;
        if ll_zero { 1 } else { 2 }
    } else if raw_offset_u32 == r3 {
        *rep3 = *rep2;
        *rep2 = *rep1;
        *rep1 = raw_offset;
        if ll_zero { 2 } else { 3 }
    } else if ll_zero && r1 > 1 && raw_offset_u32 == r1 - 1 {
        *rep3 = *rep2;
        *rep2 = *rep1;
        *rep1 = raw_offset;
        3
    } else {
        *rep3 = *rep2;
        *rep2 = *rep1;
        *rep1 = raw_offset;
        raw_offset_u32 + 3
    };

    let sequence = SequenceCommand {
        literal_length: literal_length.min(u32::MAX as usize) as u32,
        offset_value,
        match_length: match_length.min(u32::MAX as usize) as u32,
    };
    push_lazy_sequence_no_trace(plan, src, anchor, sequence);
}

fn plan_sequences_lazy_without_prefix_no_trace(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut impl LazySearchFinder,
) {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return;
    }

    let depth = params.lazy_search_depth.min(2);
    let [r1, r2, r3] = repeat_offsets.values();
    let mut rep1 = r1 as usize;
    let mut rep2 = r2 as usize;
    let mut rep3 = r3 as usize;
    let mut anchor = block_start;
    let mut pos = block_start;
    let mut inserted_until = block_start;
    let mut lazy_skipping = false;

    while pos + MIN_MATCH <= src.len() {
        let window_low = match_floor.at(pos);
        if !lazy_skipping && inserted_until < pos {
            finder.insert_range(src, inserted_until, pos);
            inserted_until = pos;
        }

        let mut best =
            repeat_ahead_match_without_prefix(src, pos, rep1, window_low).map(|candidate| {
                LazyParserMatch {
                    start: pos + 1,
                    offset: candidate.offset,
                    length: candidate.length,
                    kind: LazyMatchKind::Repeat1,
                }
            });
        if let Some(candidate) = finder.find_match(src, pos, params, pos - anchor, window_low) {
            let regular = LazyParserMatch {
                start: pos,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Regular,
            };
            if best.is_none_or(|current| regular.length > current.length) {
                best = Some(regular);
            }
        }

        let Some(mut best) = best else {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            if step > LAZY_SKIPPING_STEP {
                // Only insert when first entering skip mode — subsequent
                // skip iterations waste hash entries on incompressible data.
                if !lazy_skipping {
                    finder.insert(src, pos);
                }
                inserted_until = next_pos.min(src.len());
                lazy_skipping = true;
            }
            pos = next_pos;
            continue;
        };

        // C's lazy family runs the depth probes for every match of 4 bytes or
        // more: the only exits from `ZSTD_compressBlock_lazy_generic` before
        // this point are `depth == 0` and `matchLength < 4`. This used to also
        // skip the probes once the depth-0 match reached
        // `good_enough_match_length`, described here as C's `sufficient_len`
        // optimization. That was a misreading — `sufficient_len` belongs to the
        // opt parsers and the lazy family has no counterpart — and the shortcut
        // only bit with a binary-tree finder, where the depth-0 match routinely
        // reaches 64 bytes. Removing it is worth roughly 5% of ratio at levels
        // 13 through 15 and leaves levels 6 through 12 byte-identical. The
        // tracing twin of this loop never had the gate, so the two also
        // disagreed about the parse they reported.
        if depth >= 1 {
            let mut probe_pos = pos;
            loop {
                let depth1_pos = probe_pos + 1;
                if depth1_pos + MIN_MATCH > src.len() {
                    break;
                }
                // C searches from `ip` after incrementing it, so the floor moves
                // with the probe. Reusing the enclosing position's floor lets a
                // depth probe reach one byte further back than the window.
                let window_low = match_floor.at(depth1_pos);

                if let Some(candidate) =
                    repeat_match_without_prefix_at(src, depth1_pos, rep1, window_low)
                        .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 3))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Repeat1,
                    };
                }

                if inserted_until < depth1_pos {
                    if lazy_skipping {
                        finder.insert(src, depth1_pos - 1);
                    } else {
                        finder.insert_range(src, inserted_until, depth1_pos);
                    }
                    inserted_until = depth1_pos;
                }

                if let Some(candidate) = finder
                    .find_match(src, depth1_pos, params, depth1_pos - anchor, window_low)
                    .filter(|candidate| lazy_regular_match_improves(best, *candidate, 4))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Regular,
                    };
                    probe_pos = depth1_pos;
                    continue;
                }

                if depth == 2 {
                    let depth2_pos = depth1_pos + 1;
                    if depth2_pos + MIN_MATCH > src.len() {
                        break;
                    }
                    let window_low = match_floor.at(depth2_pos);

                    if let Some(candidate) =
                        repeat_match_without_prefix_at(src, depth2_pos, rep1, window_low)
                            .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 4))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Repeat1,
                        };
                    }

                    if inserted_until < depth2_pos {
                        if lazy_skipping {
                            finder.insert(src, depth2_pos - 1);
                        } else {
                            finder.insert_range(src, inserted_until, depth2_pos);
                        }
                        inserted_until = depth2_pos;
                    }

                    if let Some(candidate) = finder
                        .find_match(src, depth2_pos, params, depth2_pos - anchor, window_low)
                        .filter(|candidate| lazy_regular_match_improves(best, *candidate, 7))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Regular,
                        };
                        probe_pos = depth2_pos;
                        continue;
                    }
                }

                break;
            }
        }

        match best.kind {
            LazyMatchKind::Repeat1 => {
                store_lazy_sequence_local_reps(
                    plan,
                    src,
                    &mut anchor,
                    &mut rep1,
                    &mut rep2,
                    &mut rep3,
                    best.start,
                    best.offset,
                    best.length,
                );
            }
            LazyMatchKind::Regular => {
                let found = extend_back_source_candidate(
                    src,
                    anchor,
                    best.start,
                    best.candidate(),
                    window_low,
                );
                store_lazy_sequence_local_reps(
                    plan,
                    src,
                    &mut anchor,
                    &mut rep1,
                    &mut rep2,
                    &mut rep3,
                    found.start,
                    found.offset,
                    found.length,
                );
            }
        }

        pos = anchor;
        lazy_skipping = false;

        while pos + MIN_MATCH <= src.len() {
            let window_low = match_floor.at(pos);
            let Some(candidate) = repeat_match_without_prefix_at(src, pos, rep2, window_low) else {
                break;
            };
            store_lazy_sequence_local_reps(
                plan,
                src,
                &mut anchor,
                &mut rep1,
                &mut rep2,
                &mut rep3,
                pos,
                candidate.offset,
                candidate.length,
            );
            pos = anchor;
        }
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = RepeatOffsets::from_values([
        rep1.min(u32::MAX as usize) as u32,
        rep2.min(u32::MAX as usize) as u32,
        rep3.min(u32::MAX as usize) as u32,
    ]);
}

fn plan_sequences_lazy_without_prefix_tracing(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: MatchFloor,
    finder: &mut impl LazySearchFinder,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let depth = params.lazy_search_depth.min(2);
    let mut repeat_offsets = repeat_offsets;
    let mut anchor = block_start;
    let mut pos = block_start;
    let mut inserted_until = block_start;
    let mut lazy_skipping = false;

    while pos + MIN_MATCH <= src.len() {
        let window_low = match_floor.at(pos);
        if !lazy_skipping && inserted_until < pos {
            finder.insert_range(src, inserted_until, pos);
            inserted_until = pos;
        }

        let rep1 = repeat_offsets.values()[0] as usize;
        let mut best =
            repeat_ahead_match_without_prefix(src, pos, rep1, window_low).map(|candidate| {
                LazyParserMatch {
                    start: pos + 1,
                    offset: candidate.offset,
                    length: candidate.length,
                    kind: LazyMatchKind::Repeat1,
                }
            });
        if let Some(candidate) = finder.find_match(src, pos, params, pos - anchor, window_low) {
            let regular = LazyParserMatch {
                start: pos,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Regular,
            };
            if best.is_none_or(|current| regular.length > current.length) {
                best = Some(regular);
            }
        }

        let Some(mut best) = best else {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            if step > LAZY_SKIPPING_STEP {
                finder.insert(src, pos);
                inserted_until = next_pos.min(src.len());
                lazy_skipping = true;
            }
            pos = next_pos;
            continue;
        };

        if depth >= 1 {
            let mut probe_pos = pos;
            loop {
                let depth1_pos = probe_pos + 1;
                if depth1_pos + MIN_MATCH > src.len() {
                    break;
                }
                // C searches from `ip` after incrementing it, so the floor moves
                // with the probe. Reusing the enclosing position's floor lets a
                // depth probe reach one byte further back than the window.
                let window_low = match_floor.at(depth1_pos);

                if let Some(candidate) =
                    repeat_match_without_prefix_at(src, depth1_pos, rep1, window_low)
                        .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 3))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Repeat1,
                    };
                }

                if inserted_until < depth1_pos {
                    if lazy_skipping {
                        finder.insert(src, depth1_pos - 1);
                    } else {
                        finder.insert_range(src, inserted_until, depth1_pos);
                    }
                    inserted_until = depth1_pos;
                }

                if let Some(candidate) = finder
                    .find_match(src, depth1_pos, params, depth1_pos - anchor, window_low)
                    .filter(|candidate| lazy_regular_match_improves(best, *candidate, 4))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Regular,
                    };
                    probe_pos = depth1_pos;
                    continue;
                }

                if depth == 2 {
                    let depth2_pos = depth1_pos + 1;
                    if depth2_pos + MIN_MATCH > src.len() {
                        break;
                    }
                    let window_low = match_floor.at(depth2_pos);

                    if let Some(candidate) =
                        repeat_match_without_prefix_at(src, depth2_pos, rep1, window_low)
                            .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 4))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Repeat1,
                        };
                    }

                    if inserted_until < depth2_pos {
                        if lazy_skipping {
                            finder.insert(src, depth2_pos - 1);
                        } else {
                            finder.insert_range(src, inserted_until, depth2_pos);
                        }
                        inserted_until = depth2_pos;
                    }

                    if let Some(candidate) = finder
                        .find_match(src, depth2_pos, params, depth2_pos - anchor, window_low)
                        .filter(|candidate| lazy_regular_match_improves(best, *candidate, 7))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Regular,
                        };
                        probe_pos = depth2_pos;
                        continue;
                    }
                }

                break;
            }
        }

        match best.kind {
            LazyMatchKind::Repeat1 => {
                store_lazy_sequence(
                    plan,
                    src,
                    &mut anchor,
                    &mut repeat_offsets,
                    best.start,
                    best.offset,
                    best.length,
                )?;
            }
            LazyMatchKind::Regular => {
                let found = extend_back_source_candidate(
                    src,
                    anchor,
                    best.start,
                    best.candidate(),
                    window_low,
                );
                store_lazy_sequence(
                    plan,
                    src,
                    &mut anchor,
                    &mut repeat_offsets,
                    found.start,
                    found.offset,
                    found.length,
                )?;
            }
        }

        pos = anchor;
        lazy_skipping = false;

        while pos + MIN_MATCH <= src.len() {
            let rep2 = repeat_offsets.values()[1] as usize;
            let Some(candidate) = repeat_match_without_prefix_at(src, pos, rep2, window_low) else {
                break;
            };
            store_lazy_sequence(
                plan,
                src,
                &mut anchor,
                &mut repeat_offsets,
                pos,
                candidate.offset,
                candidate.length,
            )?;
            pos = anchor;
        }
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

pub(crate) fn plan_sequences_lazy_with_prefix_chain_from_into(
    plan: &mut SequencePlan,
    src: &[u8],
    block_start: usize,
    prefix_chain: PrefixChain<'_>,
    repeat_offsets: RepeatOffsets,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    prefix_finder: &impl LazySearchFinder,
    src_finder: &mut impl LazySearchFinder,
    mode: PrefixMatchMode,
) -> Result<()> {
    let block_len = src.len().saturating_sub(block_start);
    plan.reset_for_block(block_len);
    if block_len < MIN_MATCH {
        plan.literals.extend_from_slice(&src[block_start..]);
        plan.repeat_offsets = repeat_offsets;
        return Ok(());
    }

    let depth = params.lazy_search_depth.min(2);
    let mut repeat_offsets = repeat_offsets;
    let mut anchor = block_start;
    let mut pos = block_start;
    let mut inserted_until = block_start;
    let mut lazy_skipping = false;
    let search_limit = lazy_search_limit(src.len());

    while pos < search_limit {
        let (prefix_low, source_low) = match_floor.at(pos);
        if !lazy_skipping && inserted_until < pos {
            src_finder.insert_range(src, inserted_until, pos);
            inserted_until = pos;
        }

        let rep1 = repeat_offsets.values()[0] as usize;
        let mut best = repeat_ahead_match_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            rep1,
            prefix_low,
            source_low,
        )
        .map(|candidate| LazyParserMatch {
            start: pos + 1,
            offset: candidate.offset,
            length: candidate.length,
            kind: LazyMatchKind::Repeat1,
        });
        if let Some(candidate) = best_regular_match_with_prefix_chain(
            prefix_chain,
            src,
            pos,
            pos - anchor,
            params,
            prefix_low,
            source_low,
            mode,
            prefix_finder,
            src_finder,
        ) {
            let regular = LazyParserMatch {
                start: pos,
                offset: candidate.offset,
                length: candidate.length,
                kind: LazyMatchKind::Regular,
            };
            if best.is_none_or(|current| regular.length > current.length) {
                best = Some(regular);
            }
        }

        let Some(mut best) = best else {
            let step = skip_after_no_match(anchor, pos, params);
            let next_pos = pos.saturating_add(step);
            if step > LAZY_SKIPPING_STEP {
                // Only insert when first entering skip mode — subsequent
                // skip iterations waste hash entries on incompressible data.
                if !lazy_skipping {
                    src_finder.insert(src, pos);
                }
                inserted_until = next_pos.min(src.len());
                lazy_skipping = true;
            }
            pos = next_pos;
            continue;
        };

        // C's lazy family runs the depth probes for every match of 4 bytes or
        // more: the only exits from `ZSTD_compressBlock_lazy_generic` before
        // this point are `depth == 0` and `matchLength < 4`. This used to also
        // skip the probes once the depth-0 match reached
        // `good_enough_match_length`, described here as C's `sufficient_len`
        // optimization. That was a misreading — `sufficient_len` belongs to the
        // opt parsers and the lazy family has no counterpart — and the shortcut
        // only bit with a binary-tree finder, where the depth-0 match routinely
        // reaches 64 bytes. Removing it is worth roughly 5% of ratio at levels
        // 13 through 15 and leaves levels 6 through 12 byte-identical. The
        // tracing twin of this loop never had the gate, so the two also
        // disagreed about the parse they reported.
        if depth >= 1 {
            let mut probe_pos = pos;
            loop {
                let depth1_pos = probe_pos + 1;
                if depth1_pos > search_limit {
                    break;
                }
                // The floor moves with the probe, as it does in C.
                let (prefix_low, source_low) = match_floor.at(depth1_pos);

                if let Some(candidate) = repeat_match_with_prefix_chain_at(
                    prefix_chain,
                    src,
                    depth1_pos,
                    rep1,
                    prefix_low,
                    source_low,
                )
                .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 3))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Repeat1,
                    };
                }

                if inserted_until < depth1_pos {
                    if lazy_skipping {
                        src_finder.insert(src, depth1_pos - 1);
                    } else {
                        src_finder.insert_range(src, inserted_until, depth1_pos);
                    }
                    inserted_until = depth1_pos;
                }

                if let Some(candidate) = best_regular_match_with_prefix_chain(
                    prefix_chain,
                    src,
                    depth1_pos,
                    depth1_pos - anchor,
                    params,
                    prefix_low,
                    source_low,
                    mode,
                    prefix_finder,
                    src_finder,
                )
                .filter(|candidate| lazy_regular_match_improves(best, *candidate, 4))
                {
                    best = LazyParserMatch {
                        start: depth1_pos,
                        offset: candidate.offset,
                        length: candidate.length,
                        kind: LazyMatchKind::Regular,
                    };
                    probe_pos = depth1_pos;
                    continue;
                }

                if depth == 2 {
                    let depth2_pos = depth1_pos + 1;
                    if depth2_pos > search_limit {
                        break;
                    }
                    let (prefix_low, source_low) = match_floor.at(depth2_pos);

                    if let Some(candidate) = repeat_match_with_prefix_chain_at(
                        prefix_chain,
                        src,
                        depth2_pos,
                        rep1,
                        prefix_low,
                        source_low,
                    )
                    .filter(|candidate| lazy_repeat_match_improves(best, *candidate, 4))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Repeat1,
                        };
                    }

                    if inserted_until < depth2_pos {
                        if lazy_skipping {
                            src_finder.insert(src, depth2_pos - 1);
                        } else {
                            src_finder.insert_range(src, inserted_until, depth2_pos);
                        }
                        inserted_until = depth2_pos;
                    }

                    if let Some(candidate) = best_regular_match_with_prefix_chain(
                        prefix_chain,
                        src,
                        depth2_pos,
                        depth2_pos - anchor,
                        params,
                        prefix_low,
                        source_low,
                        mode,
                        prefix_finder,
                        src_finder,
                    )
                    .filter(|candidate| lazy_regular_match_improves(best, *candidate, 7))
                    {
                        best = LazyParserMatch {
                            start: depth2_pos,
                            offset: candidate.offset,
                            length: candidate.length,
                            kind: LazyMatchKind::Regular,
                        };
                        probe_pos = depth2_pos;
                        continue;
                    }
                }

                break;
            }
        }

        match best.kind {
            LazyMatchKind::Repeat1 => {
                store_lazy_sequence_with_source(
                    plan,
                    src,
                    &mut anchor,
                    &mut repeat_offsets,
                    best.start,
                    best.offset,
                    best.length,
                    SequenceTraceMatchSource::Rep,
                )?;
            }
            LazyMatchKind::Regular => {
                let found = extend_back_prefix_chain_match(
                    prefix_chain,
                    src,
                    anchor,
                    best.start,
                    best.candidate(),
                    prefix_low,
                    source_low,
                );
                let source = regular_match_source_for_prefix_mode(mode, found.start, found.offset);
                let anchor_before = anchor;
                let [offset_1_before, offset_2_before, _] = repeat_offsets.values();
                let off_base = explicit_offbase(found.offset);
                let raw_offset = store_lazy_sequence_with_offset_value_and_source(
                    plan,
                    src,
                    &mut anchor,
                    &mut repeat_offsets,
                    found.start,
                    off_base,
                    found.length,
                    source,
                )?;
                let [offset_1_after, offset_2_after, _] = repeat_offsets.values();
                trace_lazy_emission(
                    plan,
                    SequenceTraceEmissionKind::Regular,
                    source,
                    anchor_before,
                    found.start,
                    found.length,
                    off_base,
                    raw_offset as usize,
                    offset_1_before as usize,
                    offset_2_before as usize,
                    offset_1_after as usize,
                    offset_2_after as usize,
                );
            }
        }

        pos = anchor;
        lazy_skipping = false;

        while pos <= search_limit {
            let (prefix_low, source_low) = match_floor.at(pos);
            let rep2 = repeat_offsets.values()[1] as usize;
            let Some(candidate) = repeat_match_with_prefix_chain_at(
                prefix_chain,
                src,
                pos,
                rep2,
                prefix_low,
                source_low,
            ) else {
                break;
            };
            store_lazy_sequence_with_source(
                plan,
                src,
                &mut anchor,
                &mut repeat_offsets,
                pos,
                candidate.offset,
                candidate.length,
                SequenceTraceMatchSource::Rep,
            )?;
            pos = anchor;
        }
    }

    plan.literals.extend_from_slice(&src[anchor..]);
    plan.repeat_offsets = repeat_offsets;
    Ok(())
}

pub(crate) fn should_accept_match(
    candidate: MatchCandidate,
    literal_length: usize,
    params: MatchFinderParameters,
) -> bool {
    let min_match_length = if literal_length == 0 {
        params.min_match_length_zero_literals
    } else {
        params.min_match_length_after_literals
    };
    if candidate.length < min_match_length {
        return false;
    }
    true
}

pub(crate) fn find_lazy_match_skip_without_prefix(
    src: &[u8],
    pos: usize,
    anchor: usize,
    repeat_offsets: [u32; 3],
    candidate: MatchCandidate,
    params: MatchFinderParameters,
    finder: &mut impl LazySearchFinder,
    match_floor: MatchFloor,
) -> LazyMatchDecision {
    if params.lazy_search_depth == 0 {
        return LazyMatchDecision::default();
    }

    let mut best_skip = 0usize;
    let mut best_candidate = candidate;
    let mut inserted = 0usize;
    let window_low = match_floor.at(pos);

    if let Some(next_rep_candidate) =
        repeat_ahead_match_without_prefix(src, pos, repeat_offsets[0] as usize, window_low).filter(
            |candidate| {
                should_take_lazy_match_with_skip(
                    best_candidate,
                    best_skip,
                    *candidate,
                    1,
                    repeat_offsets,
                    pos.saturating_sub(anchor),
                )
            },
        )
    {
        finder.insert(src, pos);
        inserted = 1;
        best_skip = 1;
        best_candidate = next_rep_candidate;
    }

    if best_candidate.length >= params.good_enough_match_length {
        return LazyMatchDecision {
            skip: best_skip,
            inserted,
        };
    }

    while inserted < params.lazy_search_depth {
        let next_pos = pos + inserted + 1;
        if next_pos + MIN_MATCH > src.len() {
            break;
        }

        finder.insert(src, pos + inserted);
        inserted += 1;

        let next_literal_length = next_pos - anchor;
        // The probe advances `next_pos`, so its floor moves with it.
        let window_low = match_floor.at(next_pos);
        if let Some(next_candidate) = best_match_without_prefix(
            src,
            next_pos,
            repeat_offsets,
            next_literal_length,
            params,
            finder,
            window_low,
        )
        .filter(|candidate| should_accept_match(*candidate, next_literal_length, params))
        .filter(|next_candidate| {
            should_take_lazy_match_with_skip(
                best_candidate,
                best_skip,
                *next_candidate,
                inserted,
                repeat_offsets,
                pos.saturating_sub(anchor),
            )
        }) {
            best_skip = inserted;
            best_candidate = next_candidate;
            if best_candidate.length >= params.good_enough_match_length {
                break;
            }
        }
    }

    LazyMatchDecision {
        skip: best_skip,
        inserted,
    }
}

pub(crate) fn find_lazy_match_skip_with_prefix_chain(
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    pos: usize,
    anchor: usize,
    repeat_offsets: [u32; 3],
    candidate: MatchCandidate,
    params: MatchFinderParameters,
    match_floor: PrefixedMatchFloor,
    mode: PrefixMatchMode,
    prefix_finder: &impl LazySearchFinder,
    src_finder: &mut impl LazySearchFinder,
) -> LazyMatchDecision {
    if params.lazy_search_depth == 0 {
        return LazyMatchDecision::default();
    }

    let mut best_skip = 0usize;
    let mut best_candidate = candidate;
    let (prefix_low, source_low) = match_floor.at(pos);
    let mut inserted = 0usize;

    if let Some(next_rep_candidate) = repeat_ahead_match_with_prefix_chain(
        prefix_chain,
        src,
        pos,
        repeat_offsets[0] as usize,
        prefix_low,
        source_low,
    )
    .filter(|candidate| {
        should_take_lazy_match_with_skip(
            best_candidate,
            best_skip,
            *candidate,
            1,
            repeat_offsets,
            pos.saturating_sub(anchor),
        )
    }) {
        src_finder.insert(src, pos);
        inserted = 1;
        best_skip = 1;
        best_candidate = next_rep_candidate;
    }

    if best_candidate.length >= params.good_enough_match_length {
        return LazyMatchDecision {
            skip: best_skip,
            inserted,
        };
    }

    while inserted < params.lazy_search_depth {
        let next_pos = pos + inserted + 1;
        if next_pos + MIN_MATCH > src.len() {
            break;
        }

        src_finder.insert(src, pos + inserted);
        inserted += 1;

        let next_literal_length = next_pos - anchor;
        // The probe advances `next_pos`, so its floor moves with it.
        let (prefix_low, source_low) = match_floor.at(next_pos);
        if let Some(next_candidate) = best_match_with_prefix_chain(
            prefix_chain,
            src,
            next_pos,
            repeat_offsets,
            next_literal_length,
            params,
            prefix_low,
            source_low,
            mode,
            prefix_finder,
            src_finder,
        )
        .filter(|candidate| should_accept_match(*candidate, next_literal_length, params))
        .filter(|next_candidate| {
            should_take_lazy_match_with_skip(
                best_candidate,
                best_skip,
                *next_candidate,
                inserted,
                repeat_offsets,
                pos.saturating_sub(anchor),
            )
        }) {
            best_skip = inserted;
            best_candidate = next_candidate;
            if best_candidate.length >= params.good_enough_match_length {
                break;
            }
        }
    }

    LazyMatchDecision {
        skip: best_skip,
        inserted,
    }
}

pub(crate) fn should_take_lazy_match_with_skip(
    current: MatchCandidate,
    current_skip: usize,
    next: MatchCandidate,
    next_skip: usize,
    repeat_offsets: [u32; 3],
    literal_length: usize,
) -> bool {
    let current_score = estimated_match_score_bits(
        current,
        repeat_offsets,
        literal_length.saturating_add(current_skip),
    );
    let next_score = estimated_match_score_bits(
        next,
        repeat_offsets,
        literal_length.saturating_add(next_skip),
    );
    next_score > current_score
        || (next_score == current_score
            && (next.length > current.length
                || (next.length == current.length
                    && (next_skip < current_skip
                        || (next_skip == current_skip && next.offset < current.offset)))))
}

#[derive(Debug, Clone)]
pub(crate) struct MatchFinder {
    pub(crate) heads: Vec<u32>,
    pub(crate) previous: Vec<u32>,
    pub(crate) hash_bits: u32,
    pub(crate) min_match: u32,
    pub(crate) next_to_update: usize,
}

pub(crate) trait LazySearchFinder {
    fn insert(&mut self, src: &[u8], pos: usize);
    fn insert_range(&mut self, src: &[u8], start: usize, end: usize);
    fn find_match(
        &mut self,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        window_low: usize,
    ) -> Option<MatchCandidate>;
    fn find_prefix_chain_match(
        &self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        prefix_low: usize,
    ) -> Option<MatchCandidate>;
    /// Returns true when `find_match` at `pos` would return None due to
    /// a position-skip optimization (DUBT's `nextToUpdate` check).
    /// In C, when this skip fires, BOTH the source AND dictionary searches
    /// are gated by the same check. Hash chain finders never skip.
    fn would_skip(&self, _pos: usize) -> bool {
        false
    }
    /// Returns true when this finder's source search does NOT include
    /// dictionary entries. In C's ExtDict BinaryTree mode, the DUBT only
    /// searches the source tree; there is no separate dictionary tree search
    /// for regular matches. Hash chain finders in ExtDict clone the prefix
    /// chain into the source, so they DO include dictionary entries.
    fn skips_prefix_regular_search(&self) -> bool {
        false
    }
    /// Find a match using virtual coordinates when a prefix (dictionary)
    /// has been pre-populated into this finder. Falls back to `find_match`
    /// when no prefix is present.
    ///
    /// Both halves of the floor are passed because a finder that has the
    /// prefix pre-populated searches one index space covering both regions,
    /// and which half bounds a candidate depends on where it landed. A finder
    /// that keeps the two apart reaches the prefix through
    /// [`Self::find_prefix_chain_match`] instead and needs only `source_low`,
    /// which is what the default below does.
    fn find_match_with_prefix(
        &mut self,
        _prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        _prefix_low: usize,
        source_low: usize,
    ) -> Option<MatchCandidate> {
        self.find_match(src, pos, params, literal_length, source_low)
    }
}

impl MatchFinder {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(src_len: usize, hash_bits: u32, min_match: u32) -> Self {
        Self::with_chain_log(src_len, hash_bits, hash_bits, min_match)
    }

    pub(crate) fn with_chain_log(
        src_len: usize,
        hash_bits: u32,
        chain_log: u32,
        min_match: u32,
    ) -> Self {
        let _ = src_len;
        let hash_bits = hash_bits.clamp(10, MAX_MATCH_HASH_BITS);
        let chain_bits = chain_cycle_log(chain_log);
        Self {
            heads: vec![NO_POS; 1usize << hash_bits],
            previous: vec![NO_POS; 1usize << chain_bits],
            hash_bits,
            min_match: min_match.clamp(4, 6),
            next_to_update: 0,
        }
    }

    pub(crate) fn chain_mask(&self) -> usize {
        self.previous.len() - 1
    }

    /// The alignment [`shift_positions`](Self::shift_positions) requires, which
    /// is the length of the cycle `previous` is indexed by.
    pub(crate) fn rebase_period(&self) -> usize {
        self.previous.len()
    }

    /// Rebase every position by `delta`, which must be a multiple of
    /// [`rebase_period`](Self::rebase_period).
    ///
    /// `heads` is indexed by a hash of the bytes at each position, and those
    /// bytes do not change when the buffer in front of them is dropped. But
    /// `previous` is indexed by `position & chain_mask`, so subtracting from the
    /// values alone would leave every link in a slot that no longer names it.
    /// An aligned `delta` is exactly the condition under which it does: it
    /// leaves the low `chain_log` bits of every position untouched.
    pub(crate) fn shift_positions(&mut self, delta: usize) {
        debug_assert!(
            delta.is_multiple_of(self.rebase_period()),
            "rebasing a hash chain by an unaligned delta moves entries out of their slots"
        );
        shift_raw_positions(&mut self.heads, delta, NO_POS);
        shift_raw_positions(&mut self.previous, delta, NO_POS);
        self.next_to_update = self.next_to_update.saturating_sub(delta);
    }

    /// Rebase every position by any `delta`, valid only where no live position
    /// reaches the cycle: `live_end` must be at most
    /// [`rebase_period`](Self::rebase_period).
    ///
    /// See [`BinaryTreeFinder::shift_positions_by_slot`]; `previous` holds one
    /// entry per position rather than two, so the shift is the delta itself.
    pub(crate) fn shift_positions_by_slot(&mut self, delta: usize, live_end: usize) {
        debug_assert!(
            live_end <= self.rebase_period(),
            "a position past the cycle does not sit in the slot it names"
        );
        let len = self.previous.len();
        let kept = len - delta.min(len);
        self.previous.copy_within(len - kept.., 0);
        self.previous[kept..].fill(NO_POS);
        shift_raw_positions(&mut self.heads, delta, NO_POS);
        shift_raw_positions(&mut self.previous[..kept], delta, NO_POS);
        self.next_to_update = self.next_to_update.saturating_sub(delta);
    }

    pub(crate) fn previous_index(&self, pos: usize) -> usize {
        pos & self.chain_mask()
    }

    #[inline(always)]
    pub(crate) fn previous_at(&self, pos: usize) -> u32 {
        self.previous[self.previous_index(pos)]
    }

    #[inline(always)]
    pub(crate) fn hash_src_at(&self, src: &[u8], pos: usize) -> usize {
        if pos + HASH_READ_SIZE <= src.len() {
            hash_at_mls(src, pos, self.hash_bits, self.min_match)
        } else {
            hash_at(src, pos, self.hash_bits)
        }
    }

    #[inline(always)]
    pub(crate) fn hash_prefix_chain_at(
        &self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
    ) -> usize {
        if pos + HASH_READ_SIZE <= prefix_chain.len() + src.len() {
            hash_prefix_chain_at_mls(prefix_chain, src, pos, self.hash_bits, self.min_match)
        } else {
            hash_prefix_chain_at(prefix_chain, src, pos, self.hash_bits)
        }
    }

    #[inline(always)]
    pub(crate) fn insert_direct(&mut self, src: &[u8], pos: usize) {
        if pos + MIN_MATCH > src.len() {
            return;
        }
        let slot = self.hash_src_at(src, pos);
        let prev_index = self.previous_index(pos);
        self.previous[prev_index] = self.heads[slot];
        self.heads[slot] = pos as u32;
    }

    #[inline(always)]
    pub(crate) fn insert_prefix_chain_pos_direct(
        &mut self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
    ) {
        if pos + MIN_MATCH > prefix_chain.len() + src.len() || pos >= prefix_chain.len() {
            return;
        }
        let slot = self.hash_prefix_chain_at(prefix_chain, src, pos);
        let prev_index = self.previous_index(pos);
        self.previous[prev_index] = self.heads[slot];
        self.heads[slot] = pos as u32;
    }

    pub(crate) fn ext_dict_virtual_pos(prefix_len: usize, pos: usize) -> usize {
        prefix_len + pos
    }

    #[inline(always)]
    pub(crate) fn insert_and_find_first_index(
        &mut self,
        src: &[u8],
        pos: usize,
        lazy_skipping: bool,
    ) -> Option<usize> {
        if pos + MIN_MATCH > src.len() {
            return None;
        }
        debug_assert!(pos + HASH_READ_SIZE <= src.len());
        let target = pos;
        let hash_bits = self.hash_bits;
        let min_match = self.min_match;
        match min_match {
            4 => {
                let mut idx = self.next_to_update.min(target);
                while idx < target {
                    let slot = hash_at_mls_4(src, idx, hash_bits);
                    let prev_index = self.previous_index(idx);
                    self.previous[prev_index] = self.heads[slot];
                    self.heads[slot] = idx as u32;
                    idx += 1;
                    if lazy_skipping {
                        break;
                    }
                }
                self.next_to_update = target;

                let candidate = self.heads[hash_at_mls_4(src, pos, hash_bits)];
                (candidate != NO_POS && (candidate as usize) < pos).then_some(candidate as usize)
            }
            5 => {
                let mut idx = self.next_to_update.min(target);
                while idx < target {
                    let slot = hash_at_mls_5(src, idx, hash_bits);
                    let prev_index = self.previous_index(idx);
                    self.previous[prev_index] = self.heads[slot];
                    self.heads[slot] = idx as u32;
                    idx += 1;
                    if lazy_skipping {
                        break;
                    }
                }
                self.next_to_update = target;

                let candidate = self.heads[hash_at_mls_5(src, pos, hash_bits)];
                (candidate != NO_POS && (candidate as usize) < pos).then_some(candidate as usize)
            }
            _ => {
                let mut idx = self.next_to_update.min(target);
                while idx < target {
                    let slot = hash_at_mls_6(src, idx, hash_bits);
                    let prev_index = self.previous_index(idx);
                    self.previous[prev_index] = self.heads[slot];
                    self.heads[slot] = idx as u32;
                    idx += 1;
                    if lazy_skipping {
                        break;
                    }
                }
                self.next_to_update = target;

                let candidate = self.heads[hash_at_mls_6(src, pos, hash_bits)];
                (candidate != NO_POS && (candidate as usize) < pos).then_some(candidate as usize)
            }
        }
    }

    #[inline(always)]
    pub(crate) fn insert_and_find_first_index_ext_dict(
        &mut self,
        prefix_len: usize,
        src: &[u8],
        pos: usize,
        lazy_skipping: bool,
    ) -> Option<usize> {
        if pos + MIN_MATCH > src.len() {
            return None;
        }
        debug_assert!(pos + HASH_READ_SIZE <= src.len());

        let target = Self::ext_dict_virtual_pos(prefix_len, pos);
        let chain_mask = self.chain_mask();
        let hash_bits = self.hash_bits;
        let min_match = self.min_match;
        let heads_len = self.heads.len();
        match min_match {
            4 => {
                self.catchup_ext_dict_insert::<4>(
                    src,
                    prefix_len,
                    target,
                    chain_mask,
                    hash_bits,
                    lazy_skipping,
                );
                let slot = hash_at_mls_4(src, pos, hash_bits);
                debug_assert!(slot < heads_len);
                // SAFETY: slot comes from `hash_at_mls_4` which shifts by
                // (32 - hash_bits); result is always < 1 << hash_bits = heads.len().
                #[allow(unsafe_code)]
                let candidate = unsafe { *self.heads.as_ptr().add(slot) };
                (candidate != NO_POS && (candidate as usize) < target).then_some(candidate as usize)
            }
            5 => {
                self.catchup_ext_dict_insert::<5>(
                    src,
                    prefix_len,
                    target,
                    chain_mask,
                    hash_bits,
                    lazy_skipping,
                );
                let slot = hash_at_mls_5(src, pos, hash_bits);
                debug_assert!(slot < heads_len);
                #[allow(unsafe_code)]
                let candidate = unsafe { *self.heads.as_ptr().add(slot) };
                (candidate != NO_POS && (candidate as usize) < target).then_some(candidate as usize)
            }
            _ => {
                self.catchup_ext_dict_insert::<6>(
                    src,
                    prefix_len,
                    target,
                    chain_mask,
                    hash_bits,
                    lazy_skipping,
                );
                let slot = hash_at_mls_6(src, pos, hash_bits);
                debug_assert!(slot < heads_len);
                #[allow(unsafe_code)]
                let candidate = unsafe { *self.heads.as_ptr().add(slot) };
                (candidate != NO_POS && (candidate as usize) < target).then_some(candidate as usize)
            }
        }
    }

    /// Insert virtual positions `[next_to_update, target)` into the hash chain for
    /// the ext-dict parser. Uses unchecked indexing on `heads` and `previous`
    /// (both lengths are powers of two sized by hash_bits / chain_log), which
    /// removes the bounds check on `heads[slot]` the compiler otherwise emits.
    /// `lazy_skipping` inserts only the first position then advances
    /// `next_to_update` to `target`, matching C zstd's `ZSTD_HcFindBestMatch`
    /// skip-ahead shortcut.
    #[inline(always)]
    #[allow(unsafe_code)]
    fn catchup_ext_dict_insert<const MLS: u32>(
        &mut self,
        src: &[u8],
        prefix_len: usize,
        target: usize,
        chain_mask: usize,
        hash_bits: u32,
        lazy_skipping: bool,
    ) {
        let start = self.next_to_update.min(target).max(prefix_len);
        if start >= target {
            self.next_to_update = target;
            return;
        }

        let heads_ptr = self.heads.as_mut_ptr();
        let previous_ptr = self.previous.as_mut_ptr();
        let heads_len = self.heads.len();
        let previous_len = self.previous.len();
        debug_assert!(heads_len.is_power_of_two());
        debug_assert_eq!(chain_mask, previous_len - 1);
        let _ = (heads_len, previous_len);

        if lazy_skipping {
            let src_pos = start - prefix_len;
            let slot = hash_at_mls_const::<MLS>(src, src_pos, hash_bits);
            debug_assert!(slot < heads_len);
            debug_assert!((start & chain_mask) < previous_len);
            unsafe {
                let prev = *heads_ptr.add(slot);
                *previous_ptr.add(start & chain_mask) = prev;
                *heads_ptr.add(slot) = start as u32;
            }
            self.next_to_update = target;
            return;
        }

        let mut idx = start;
        while idx < target {
            let src_pos = idx - prefix_len;
            let slot = hash_at_mls_const::<MLS>(src, src_pos, hash_bits);
            debug_assert!(slot < heads_len);
            debug_assert!((idx & chain_mask) < previous_len);
            unsafe {
                let prev = *heads_ptr.add(slot);
                *previous_ptr.add(idx & chain_mask) = prev;
                *heads_ptr.add(slot) = idx as u32;
            }
            idx += 1;
        }
        self.next_to_update = target;
    }

    pub(crate) fn find_match(
        &self,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        window_low: usize,
    ) -> Option<MatchCandidate> {
        let mut candidate = self.lookup(src, pos)?;
        if candidate < window_low {
            return None;
        }
        let mut best = None;
        // C's `minChain` (`zstd_lazy.c:686`), which stops the walk one chain
        // length back. Past that the link at `candidate & chainMask` has been
        // overwritten by a nearer position, so following it leaves the bucket
        // the walk started in. Inert across all 1794 baseline rows -- removing
        // it moves none of them -- and kept because it is what C does and what
        // `best_chain_dict_match_state_regular_match` beside it already does.
        // Before it, an instrumented run of the walk followed a link that did
        // not decrease 21 times on one 256 KiB body.
        let min_chain = pos.saturating_sub(self.previous.len());

        for _ in 0..params.search_depth {
            let offset = pos.checked_sub(candidate)?;
            if offset == 0 {
                break;
            }
            let length = count_match_length(src, candidate, pos);
            if length >= params.min_match_length_zero_literals {
                best = choose_better_regular_match(
                    best,
                    Some(MatchCandidate { offset, length }),
                    literal_length,
                );
                // C's only early exit from the chain walk, and it is not a
                // quality bar: `if (ip+currentMl == iLimit) break`
                // (`zstd_lazy.c:727`), taken because a match that already runs
                // to the end of the block cannot be beaten and the next
                // candidate's four-byte pre-check would read past it.
                //
                // This used to break at `good_enough_match_length` instead,
                // which is the `sufficient_len` of the *opt* parsers -- the
                // lazy family has no counterpart, as the note in
                // `plan_sequences_lazy_without_prefix_no_trace` already records
                // for the depth probes. Here it cost far more than it did
                // there. It fired on 40% of searches and capped the parse at
                // 64-byte matches while C kept walking to matches thousands of
                // bytes long, and because it fired before the depth ran out it
                // made the whole parse *insensitive to `search_log`*: forced
                // Lazy on 256 KiB of `raw-dictionary` gave byte-identical
                // output at search depth 8, 32 and 128, against upstream
                // halving from 7312 to 3632 over the same range. Removing it
                // lands on 3632 exactly.
                //
                if pos + length == src.len() {
                    break;
                }
            }

            if candidate <= min_chain {
                break;
            }
            let prev = self.previous_at(candidate);
            if prev == NO_POS || (prev as usize) < window_low {
                break;
            }
            candidate = prev as usize;
        }

        best
    }

    pub(crate) fn find_prefix_chain_match(
        &self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        prefix_low: usize,
    ) -> Option<MatchCandidate> {
        if pos + MIN_MATCH > src.len() {
            return None;
        }

        let current = prefix_chain.len() + pos;
        let mut candidate = self.lookup_prefix_chain(prefix_chain, src, pos)?;
        if candidate < prefix_low {
            return None;
        }
        let mut best: Option<MatchCandidate> = None;

        for _ in 0..params.dictionary_search_depth {
            let offset = current.checked_sub(candidate)?;
            if offset == 0 {
                break;
            }
            if best.is_some_and(|best_candidate| {
                !virtual_match_can_reach_length(
                    prefix_chain,
                    src,
                    candidate,
                    current,
                    minimum_regular_match_length_to_tie(best_candidate, candidate, literal_length),
                )
            }) {
                let prev = self.previous_at(candidate);
                if prev == NO_POS {
                    break;
                }
                candidate = prev as usize;
                continue;
            }
            let length = count_match_length_virtual(prefix_chain, src, candidate, current);
            if length >= params.min_match_length_zero_literals {
                best = choose_better_regular_match(
                    best,
                    Some(MatchCandidate { offset, length }),
                    literal_length,
                );
                if params.source_score_penalty_with_prefix == 0
                    && length >= params.good_enough_match_length
                {
                    break;
                }
            }

            let prev = self.previous_at(candidate);
            if prev == NO_POS || (prev as usize) < prefix_low {
                break;
            }
            candidate = prev as usize;
        }

        best
    }

    pub(crate) fn reset(&mut self) {
        self.heads.fill(NO_POS);
        self.previous.fill(NO_POS);
        self.next_to_update = 0;
    }

    pub(crate) fn insert(&mut self, src: &[u8], pos: usize) {
        if pos + MIN_MATCH > src.len() || pos < self.next_to_update {
            return;
        }
        self.insert_direct(src, pos);
        self.next_to_update = pos + 1;
    }

    pub(crate) fn insert_range(&mut self, src: &[u8], start: usize, end: usize) {
        let end = end.min(hash_insert_end(src.len()));
        let start = start.max(self.next_to_update).min(end);
        for pos in start..end {
            self.insert_direct(src, pos);
        }
        self.next_to_update = self.next_to_update.max(end);
    }

    #[inline(always)]
    pub(crate) fn insert_ext_dict_range(
        &mut self,
        prefix_len: usize,
        src: &[u8],
        start: usize,
        end: usize,
    ) {
        let chain_mask = self.chain_mask();
        let start = Self::ext_dict_virtual_pos(prefix_len, start);
        let end = Self::ext_dict_virtual_pos(prefix_len, end.min(hash_insert_end(src.len())));
        let start = start.max(self.next_to_update).min(end);
        for pos in start..end {
            let src_pos = pos - prefix_len;
            let slot = self.hash_src_at(src, src_pos);
            let prev_index = pos & chain_mask;
            self.previous[prev_index] = self.heads[slot];
            self.heads[slot] = pos as u32;
        }
        self.next_to_update = self.next_to_update.max(end);
    }

    pub(crate) fn insert_prefix_chain(&mut self, prefix_chain: PrefixChain<'_>, src: &[u8]) {
        let end = prefix_chain.len().min(
            prefix_chain
                .len()
                .saturating_add(src.len())
                .saturating_sub(MIN_MATCH)
                + 1,
        );
        let start = self.next_to_update.min(end);
        for pos in start..end {
            self.insert_prefix_chain_pos_direct(prefix_chain, src, pos);
        }
        self.next_to_update = end;
    }

    pub(crate) fn insert_prefix_chain_for_cdict(&mut self, prefix_chain: PrefixChain<'_>) {
        if prefix_chain.len() <= HASH_READ_SIZE {
            self.next_to_update = prefix_chain.len();
            return;
        }

        // Upstream CDict lazy/hash-chain loading only inserts positions up to
        // `iend - HASH_READ_SIZE`, then marks the entire dictionary as loaded.
        let end = prefix_chain.len() - HASH_READ_SIZE;
        let start = self.next_to_update.min(end);
        for pos in start..end {
            self.insert_prefix_chain_pos_direct(prefix_chain, &[], pos);
        }
        self.next_to_update = prefix_chain.len();
    }

    #[allow(dead_code)]
    pub(crate) fn insert_prefix_chain_pos(
        &mut self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
    ) {
        if pos + MIN_MATCH > prefix_chain.len() + src.len()
            || pos >= prefix_chain.len()
            || pos < self.next_to_update
        {
            return;
        }
        self.insert_prefix_chain_pos_direct(prefix_chain, src, pos);
        self.next_to_update = pos + 1;
    }

    pub(crate) fn lookup(&self, src: &[u8], pos: usize) -> Option<usize> {
        if pos + MIN_MATCH > src.len() {
            return None;
        }
        let candidate = self.heads[self.hash_src_at(src, pos)];
        (candidate != NO_POS && (candidate as usize) < pos).then_some(candidate as usize)
    }

    pub(crate) fn lookup_prefix_chain(
        &self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
    ) -> Option<usize> {
        if pos + MIN_MATCH > src.len() {
            return None;
        }
        let candidate = self.heads[self.hash_src_at(src, pos)];
        (candidate != NO_POS && (candidate as usize) < prefix_chain.len())
            .then_some(candidate as usize)
    }
}

impl LazySearchFinder for MatchFinder {
    fn insert(&mut self, src: &[u8], pos: usize) {
        Self::insert(self, src, pos);
    }

    fn insert_range(&mut self, src: &[u8], start: usize, end: usize) {
        Self::insert_range(self, src, start, end);
    }

    fn find_match(
        &mut self,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        window_low: usize,
    ) -> Option<MatchCandidate> {
        Self::find_match(self, src, pos, params, literal_length, window_low)
    }

    fn find_prefix_chain_match(
        &self,
        prefix_chain: PrefixChain<'_>,
        src: &[u8],
        pos: usize,
        params: MatchFinderParameters,
        literal_length: usize,
        prefix_low: usize,
    ) -> Option<MatchCandidate> {
        Self::find_prefix_chain_match(
            self,
            prefix_chain,
            src,
            pos,
            params,
            literal_length,
            prefix_low,
        )
    }
}

pub(crate) fn row_search_limit(src_len: usize) -> usize {
    src_len.saturating_sub(8 + ROW_HASH_CACHE_SIZE)
}

pub(crate) fn row_insert_end(len: usize) -> usize {
    len.saturating_sub(8) + usize::from(len >= 8)
}

pub(crate) fn row_next_insert_index(head_index: usize, row_mask: usize) -> usize {
    let mut next = head_index.wrapping_sub(1) & row_mask;
    if next == 0 {
        next += row_mask;
    }
    next
}

pub(crate) fn row_next_index(tag_row: &mut [u8], row_mask: usize) -> usize {
    let next = row_next_insert_index(usize::from(tag_row[0]), row_mask);
    tag_row[0] = next as u8;
    next
}

/// Row hash with compile-time MLS dispatch. Eliminates the runtime `match mls`
/// branch, matching C zstd's macro-expanded approach.
#[allow(unsafe_code)]
#[inline(always)]
pub(crate) fn row_hash_src_at_const<const MLS: u32>(
    src: &[u8],
    pos: usize,
    hash_bits: u32,
    salt: u64,
) -> usize {
    debug_assert!(hash_bits <= 32);
    debug_assert!(pos + 8 <= src.len());
    match MLS {
        4 => {
            let value = unsafe { crate::entropy::mem::read_u32_unchecked(src, pos) };
            (((value.wrapping_mul(0x9E37_79B1)) ^ salt as u32) >> (32 - hash_bits)) as usize
        }
        5 => {
            let value = unsafe { crate::entropy::mem::read_u64_unchecked(src, pos) };
            ((((value << 24).wrapping_mul(889_523_592_379)) ^ salt) >> (64 - hash_bits)) as usize
        }
        _ => {
            let value = unsafe { crate::entropy::mem::read_u64_unchecked(src, pos) };
            ((((value << 16).wrapping_mul(227_718_039_650_203)) ^ salt) >> (64 - hash_bits))
                as usize
        }
    }
}

#[allow(unsafe_code)]
pub(crate) fn row_hash_src_at(
    src: &[u8],
    pos: usize,
    hash_bits: u32,
    mls: u32,
    salt: u64,
) -> usize {
    debug_assert!((4..=6).contains(&mls));
    debug_assert!(hash_bits <= 32);
    debug_assert!(pos + 8 <= src.len());
    match mls {
        4 => row_hash_src_at_const::<4>(src, pos, hash_bits, salt),
        5 => row_hash_src_at_const::<5>(src, pos, hash_bits, salt),
        _ => row_hash_src_at_const::<6>(src, pos, hash_bits, salt),
    }
}

#[inline(always)]
pub(crate) fn row_hash_prefix_at(
    prefix: &[u8],
    pos: usize,
    hash_bits: u32,
    mls: u32,
    salt: u64,
) -> usize {
    debug_assert!((4..=6).contains(&mls));
    debug_assert!(hash_bits <= 32);
    match mls {
        4 => {
            let value = crate::entropy::mem::read_u32(prefix, pos);
            (((value.wrapping_mul(0x9E37_79B1)) ^ salt as u32) >> (32 - hash_bits)) as usize
        }
        5 => {
            let value = crate::entropy::mem::read_u64(prefix, pos);
            ((((value << 24).wrapping_mul(889_523_592_379)) ^ salt) >> (64 - hash_bits)) as usize
        }
        _ => {
            let value = crate::entropy::mem::read_u64(prefix, pos);
            ((((value << 16).wrapping_mul(227_718_039_650_203)) ^ salt) >> (64 - hash_bits))
                as usize
        }
    }
}

#[inline(always)]
pub(crate) fn row_hash_prefix_chain_at(
    prefix_chain: PrefixChain<'_>,
    src: &[u8],
    pos: usize,
    hash_bits: u32,
    mls: u32,
    salt: u64,
) -> usize {
    let bytes = [
        virtual_byte(prefix_chain, src, pos),
        virtual_byte(prefix_chain, src, pos + 1),
        virtual_byte(prefix_chain, src, pos + 2),
        virtual_byte(prefix_chain, src, pos + 3),
        virtual_byte(prefix_chain, src, pos + 4),
        virtual_byte(prefix_chain, src, pos + 5),
        virtual_byte(prefix_chain, src, pos + 6),
        virtual_byte(prefix_chain, src, pos + 7),
    ];
    row_hash_bytes(bytes, hash_bits, mls, salt)
}

pub(crate) fn row_hash_bytes(bytes: [u8; 8], hash_bits: u32, mls: u32, salt: u64) -> usize {
    debug_assert!((4..=6).contains(&mls));
    debug_assert!(hash_bits <= 32);
    match mls {
        4 => {
            let value = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (((value.wrapping_mul(0x9E37_79B1)) ^ salt as u32) >> (32 - hash_bits)) as usize
        }
        5 => {
            let value = u64::from_le_bytes(bytes);
            ((((value << 24).wrapping_mul(889_523_592_379)) ^ salt) >> (64 - hash_bits)) as usize
        }
        _ => {
            let value = u64::from_le_bytes(bytes);
            ((((value << 16).wrapping_mul(227_718_039_650_203)) ^ salt) >> (64 - hash_bits))
                as usize
        }
    }
}

#[cfg(test)]
pub(crate) fn debug_row_hash_for_params(
    src: &[u8],
    pos: usize,
    params: MatchFinderParameters,
    salt: u64,
) -> usize {
    row_hash_src_at(src, pos, params.hash_bits, params.min_match, salt)
}
