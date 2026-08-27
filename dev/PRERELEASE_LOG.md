# Pre-release development log

Everything that happened between the first commit and the `0.1.0` release,
kept verbatim from the changelog it used to live in.

None of it is release history. Nothing here ever shipped: `0.1.0` is the first
published version, so a defect "fixed" below was fixed in code no caller could
install, and an API "changed" below was changed before anybody could depend on
it. Presenting that as a changelog asked a reader to work out which of a
hundred entries described the crate they were about to install and which
described a Tuesday.

It is kept because much of it records *why* something is shaped the way it is,
with the measurements that decided it, and that is worth more to a maintainer
than to a user. `/dev` is excluded from the packaged crate, so this costs a
consumer nothing.

Read `CHANGELOG.md` for what `0.1.0` actually is. Section headings below are as
they were and repeat, because each working session appended its own.

---

### Added

- **Literal compression is selectable.**
  `ParameterOverrides::literal_compression` exposes upstream's
  `ZSTD_c_literalCompressionMode` as `Auto`, `Enabled` or `Disabled`. `Auto` is
  the default and unchanged: literals are Huffman-coded unless the parameters
  resolve to an accelerated `Fast`, which is what the negative levels do.

  `Disabled` stores literals verbatim, which speeds up both encode and decode
  at some cost in ratio. It also reaches the optimal parser, whose cost model
  then prices a literal at the eight bits it will really occupy rather than at
  its entropy, so the parse trades matches for literals differently.

  132 of the 195 new parity rows are upstream's exact bytes. The one recorded
  gap is a single post-sequence split boundary on one corpus at level 19, at
  the same total size.

- **The row match finder is selectable.**
  `ParameterOverrides::use_row_match_finder` exposes upstream's
  `ZSTD_c_useRowMatchFinder` as `Auto`, `Enabled` or `Disabled`. `Auto` is the
  default and unchanged: the row finder comes on for the greedy, lazy and lazy2
  strategies once the window exceeds `1 << 14`.

  It reaches parameter resolution as well as parser choice. Upstream caps
  `hash_log` when the row finder is in use and counts `auto` as in use for that
  decision, so `Disabled` also lets a larger hash through.

  Forcing it on under a strategy that has no row parser is accepted and does
  nothing, as upstream does.

- **Eager dictionary preparation.** `PreparedDictionary::prepare` builds a
  dictionary's match-state tables when you call it, rather than inside the
  first compression that needs them. Parsing a dictionary reads its entropy
  tables but does not index its content for matching; indexing is sized by the
  compression parameters, so it had to wait for them. Upstream splits the work
  the same way and resolves it in the other direction, in
  `ZSTD_createCDict`.

  One call covers every input size at those options; a level resolving to
  different table geometry wants its own. Skipping it changes nothing but when
  the cost lands — the frame is byte-identical either way.

  On a 110 KiB trained dictionary compressing a 1 KiB body, this moves 124 µs
  off the first call at level 5 and 13.1 ms at level 12.

- **Dictionaries that own their bytes.** `PreparedDictionary::from_shared`
  takes anything convertible into an `Arc<[u8]>` — a `Vec<u8>` moves in rather
  than being copied — and returns a `PreparedDictionary<'static>` that is
  `Send + Sync`. `PreparedDictionary::new` still borrows, which stays the
  cheaper choice whenever the caller can keep the buffer alive.

  The gap this closes is holding a dictionary at all. A borrowed dictionary
  cannot be stored in a struct beside the buffer it borrows from, so the only
  way to keep one was to reparse per call — which rebuilds the entropy tables
  and discards the parser-built match-state tables that a prepared dictionary
  exists to cache. Cloning shares both the bytes and that cache.

  Internally the dictionary's bytes moved behind a storage enum and its content
  is held as an offset rather than a second slice, since two slices cannot both
  borrow from an `Arc` the same struct owns. The streaming decoder no longer
  caches a content slice in its per-frame state, deriving it from the
  dictionary it already holds.

- **Long-distance matching.** `ParameterOverrides::long_distance_matching`
  turns on upstream's `ZSTD_c_enableLongDistanceMatching`, with the four
  `ZSTD_c_ldm*` table parameters alongside it. It finds very long matches at
  distances the per-block parsers cannot reach, and enabling it also widens the
  window to `1 << 27` before the window is fitted to the source, as upstream
  does.

  Every parser takes it, in the two ways upstream provides: the six below
  `btopt` have the matches laid down for them and parse only the gaps between
  them, while the three optimal parsers search as they always would and price
  each long-distance match as one more candidate they can decline.

  Output is byte-identical to upstream on sixty-four of the sixty-five swept
  corpus/parser rows whose no-LDM baseline is already exact, and on fifty-eight
  of the sixty-four rows of a second sweep that supplies the four table
  parameters rather than deriving them. The exceptions are recorded, with their
  sizes, in the tests that find them; five of the seven are one corpus and
  parser reached through five different shapes, and our frame is the smaller one
  in all five. The matcher itself is byte-identical to C's across two grids of
  432 configurations each: one varying corpus, window, strategy and block size
  against the derived parameters, one varying six parameter shapes over two
  windows. 310 of the 864 find no long-range match on either side, which the
  harness now reports rather than counting as agreement.

  `StreamingEncoder` takes it too. Its history buffer is compacted as the
  stream advances and every position the matcher's table holds is an index into
  that buffer, so the table is rebased at each compaction the way upstream's
  overflow correction rebases it. Against upstream's own streaming encoder, on
  a window narrow enough that a two-megabyte frame compacts three times, the
  matcher changes the frame on exactly the rows where upstream's does and the
  worst of thirty-six rows gives up 0.41% of its baseline.

  One configuration is refused rather than encoded without it, because a frame
  that quietly omits it is indistinguishable from one that used it: a
  dictionary. `LdmMode::Auto` stays off until that is supported, since
  honouring it would start refusing configurations that never asked for
  long-distance matching.

- **Compression parameter overrides.** `EncoderOptions::parameters` takes a
  `ParameterOverrides`, whose seven `Option` fields replace whatever the
  compression level chose: window, hash and chain sizes, search depth, minimum
  match, target length, and a public `Strategy` enum naming the nine parsers.
  `None` means "whatever the level chose", and every parameter's accepted range
  is published as a `ParameterBounds` constant so a value can be checked or a
  control sized without compressing anything first. Out-of-range values are
  reported rather than clamped.

  Overrides land where upstream puts them: on parameters the level and source
  size have *already* been fitted once, and the result is fitted again. Asking
  for a narrower window therefore also pulls the hash and chain sizes down with
  it, exactly as upstream does. They reach the dictionary side too, which is
  what upstream's `ZSTD_CCtx_loadDictionary` does and what a one-sided
  implementation would silently get wrong.

  Output is byte-identical to upstream at the same `ZSTD_c_*` settings on 849
  swept rows. Two families are not, and are recorded with their measurements
  rather than asserted: a `window_log` narrower than the frame, and any
  override of `strategy`. Both reach parameter combinations no compression
  level produces; details are in `docs/PARITY_PLAN.md`.

- **A pledged source size.** `EncoderOptions::pledged_src_size` tells a stream
  how much it will carry, upstream's `ZSTD_CCtx_setPledgedSrcSize`. The stream
  then sizes its parameters for that much rather than for the largest tier, and
  its frame header declares a content size instead of only a window. The pledge
  is checked: `finish` reports an error if the stream carried something else,
  and the one-shot entry points reject a pledge that disagrees with their
  input.

- **`EncoderOptions::write_content_size`**, upstream's `ZSTD_c_contentSizeFlag`.
  Off, a frame declares a window and stays silent about its length, trading a
  decoder's ability to size its output buffer in one go for up to eight bytes
  per frame.

- **Magicless frames.** `EncoderOptions::format` and `DecoderOptions::format`
  take a `Format` of `Zstd1` or `Zstd1Magicless`, upstream's `ZSTD_c_format`
  and `ZSTD_d_format`. A magicless frame is a standard frame with its
  four-byte magic number removed, for callers whose own framing already
  identifies the payload. It cannot be detected, only asserted, so the decoder
  has to be told; `parse_frame_header_with_format` is the header-level
  equivalent.

- **Negative compression levels.** `CompressionLevel` now accepts `-131072..=22`,
  matching upstream's range. Negative levels are zstd's "fast mode": they all
  share one parameter set and use the level's magnitude as an acceleration
  factor, so the parser skips further between match attempts. More negative is
  faster and compresses less. Level `0` is an alias for the default level, as
  upstream treats it.

  **One-shot** output is byte-identical to upstream's on 480 of the 484 rows
  swept (every corpus against levels `-40..=-1` plus `-50`, `-100`, `-1000` and
  the floor). The four exceptions are recorded in the test that checks this;
  all four emit slightly *fewer* bytes than upstream, and each is a single
  block's payload inside an otherwise identical frame.

  Streaming output is not held to byte parity at any level, negative or
  positive, and negative levels are no exception: it stays within the ratio
  bound the streaming tests already enforce.

  Getting there needed more than the level table. Upstream disables Huffman
  coding of the literals section whenever the strategy is `fast` and the target
  length is non-zero, which is exactly the negative-level configuration and
  nothing else. Until that landed, these levels produced about 0.64x of
  upstream's size across every corpus, which looks like a win rather than a
  defect.

### Changed

- **Blocks are capped at the window, and a frame declares the window alone.**
  This is upstream's arrangement — `blockSize = MIN(maxBlockSize, windowSize)`
  and a header carrying `1 << windowLog` — replacing one where this crate kept
  the caller's `block_size` and declared a window wide enough for whatever such
  a block could reach. A block wider than its own window was the reason that
  extra width was needed: the fast and double-fast parsers keep a
  block-constant match floor, and a block wider than the window left them
  floored at the block's own start. Capping removes the cause rather than
  declaring around it.

  A `window_log` override below the frame is now byte-identical to upstream
  without the caller also pinning `block_size`, which is what
  `overriding_the_window_below_the_frame_matches_upstream` previously had to do
  by hand. Nothing else moves: 176 encodes across all 22 levels, one-shot and
  streaming, at three sizes and with a dictionary, are byte-identical before
  and after, because no compression level produces a window narrower than a
  block.

  Callers who set `block_size` above the window will now see more, smaller
  blocks than they asked for. `EncoderOptions::block_size` was already an upper
  bound rather than an exact size.

### Tests

- **The benchmark report summarizes throughput per case, not just per row.**
  Its only speed metric was a count of rows below 50% of upstream, which is
  deliberately loose because a single row's throughput straddles a threshold on
  identical code. That left it blind to the opposite shape: a whole case a
  little behind at every level. Both dictionary cases sat at 0.62-0.82x across
  levels 1-10 while the summary read `Encode rows below 50% | 0`. A new
  `Throughput by Case` table counts how many of each case's levels run below
  90% of upstream, alongside the median and the worst level, and the summary
  flags a case once a third of its levels are. Four cases are flagged for
  encode and one for decode. The count rather than the median because these
  cases are banded: `raw-dictionary` is behind on levels 1-10 and level with
  upstream above, which puts its median on the boundary between the two, where
  it read 0.84x and then 0.95x on consecutive sweeps of identical code. The
  count is steady for those bands, which are far enough below the floor never
  to cross it, and not steady for a case sitting just above it -- `tabular-csv`
  read 4 of 22 and then 6 across identical code, and `log-lines` 4, 4 and then
  8. No threshold on this metric is out of reach of that, so the flag is not a
  gate and a change of one or two cases between reports means nothing. The
  cause is the measurement rather than the threshold: each row times upstream's
  encode at one end and ours at the other, and a sweep runs ten minutes of
  sustained load, so a case carries drift from wherever it sits in the run.
  `raw-dictionary`, tenth of eleven cases, moved its decode count from 0 and 2
  of 22 to 16 with nothing in the library changed. Both the report and the code
  now say so, and the fix is to interleave the trials. The report's own
  revision stamp no longer counts the report as a dirty tree, so regenerating
  one no longer marks it `-dirty`.

- **The benchmark report states how large each dictionary case's dictionary
  is.** They are the interop suite's parity fixtures, 156 and 512 bytes, and
  the report feeds them 4 MiB inputs -- one dictionary byte per 26886 bytes of
  input. At equal parameters the raw fixture saves 0.6% while the frame still
  pays for a prefix on all 32 blocks, so the encode column on those two cases
  measures the prefix machinery rather than dictionary compression, and the
  flagged 0.62-0.82x band should be read as such. The regime dictionaries are
  used in, a large dictionary against a small input, has no case in the report.

- **A dictionary in front of a stream is now in the regression baseline.**
  Every dictionary row was one-shot, so the one branch of compaction that
  cannot rebase its tables and has to rebuild them -- the one a dictionary
  forces, because the dictionary leaves the window as soon as history is
  dropped -- had no row on it at all. Rebuilding over half the retained history
  moves 23 rows, all of them this mode, and nothing else in the grid notices.
  Nothing was wrong. Two further paths were probed and turned out to be covered
  or unreachable: a band of forced strategies crossed with parameter extremes
  was built and dropped, because across six injected clamps the level axis
  caught every one that can be reached at all, and two of the six cannot be --
  one needs a gigabyte of table before it binds and the other a search still
  paying its way past 512 probes, which no corpus here gives it.

- **The encoder regression baseline now covers buffer compaction, which it
  never did.** No row in the grid compacted: the narrowest window any level
  declares is 512 KiB and every body is 256 KiB, so the streaming rows were
  frames that dropped no history. That was measured rather than assumed, by
  re-running the grid against two injected compaction defects; both moved zero
  of its 884 rows, including the exact defect that was fixed. The file had
  recorded the gap as needing bodies four times the size to close, which held
  the window fixed when the window is the side that is free to move: overriding
  it to 64 KiB compacts three times over the same 256 KiB. The two injections
  now move 19 and 60 of 1170 rows. Every corpus carries the new rows rather
  than the four the dictionary modes use, because confining them detected the
  first defect on 5 rows instead of 19, and a narrow window is cheap enough to
  search that the whole grid grew by four seconds.

- **A frame produced after a streaming reset is now checked byte-for-byte
  against the same frame from a fresh encoder.** The existing reset test asserts
  only that the output round-trips, which state leaking across a reset does not
  disturb: it changes the parse and leaves the frame perfectly decodable. It is
  also not a bound a ratio test could replace, because the leak that matters
  makes the next frame *smaller*. Nothing was wrong; removing the reset's
  clear of the parser state fails 36 of the 108 comparisons, all three optimal
  parsers and none of the other six.

- **Compaction is now swept across the window/cycle geometry it is computed
  from**, at 192 configurations in under two seconds, rather than only at a
  fixed window. Nothing was wrong: 1890 configurations were checked off-tree
  and every divergence found was the measurement's own fault, in two ways now
  recorded in the tree. A streamed frame whose content is an exact multiple of
  the block size carries a three-byte empty last block that a one-shot frame
  does not, as upstream's `ZSTD_writeEpilogue` does; and a stream given no
  pledged size cannot shrink its tables to the content while the one-shot
  control does, which had the two sides running different parameters and was
  worth 5.80% on one corpus. What the new grid does and does not reach was
  measured by injection rather than asserted, including the strategies it
  provably cannot reach.

### Fixed

- **The fuzzer could not name a disabled row match finder.** The two bits it cut
  the mode from lived above a shift its caller had already applied, so one of
  them was always zero and `RowMatchFinderMode::Disabled` was unreachable. An
  inert switch in a fuzz target fails nothing — every configuration it can still
  produce is legal — so the modes now come from bits nothing else uses, and a
  test walks the control byte and pins that each state is reachable.

- **Fast and double-fast indexed the dictionary once per frame and threw it
  away.** Both built the dictionary's prefix hash table whenever a dictionary
  was in use, but under the attached-dictionary mode both parsers return into
  the prepared-table path before ever reading it. The fill was dead there, and
  it is the mode small inputs take — so the cost landed on exactly the workload
  dictionaries exist for, once per frame rather than once per dictionary. The
  table is now built only for the mode that reads it.

  Steady-state, on a 110 KiB trained dictionary compressing a 1 KiB body:
  level 1 goes 173µs to 4.4µs, level 3 goes 414µs to 4.6µs. Levels 5 and 9 are
  unchanged, being the strategies the fill never applied to. Output is
  unchanged at every level.

- **Prepared dictionary tables were rebuilt once per source size.** The
  match-state cache was keyed partly on `secondary_hash_bits`, which sizes the
  *source* side of the double-fast tables and reaches no dictionary table on any
  strategy. It is derived from the source-adjusted chain log, so below the
  attach cutoff it tracks the input length: one dictionary at one level keyed
  several ways across a spread of small inputs and rebuilt identical tables
  under each. Small inputs are where a prepared dictionary earns the most, so
  the split was worst exactly where the cache mattered. The key now carries the
  geometry the tables are built from and nothing else. Output is unchanged.

- **Streaming never re-keyed the optimal parsers' three-byte match table.** When
  the encoder drops history off the front of its buffer, every table that
  addresses that buffer has to be re-keyed, as upstream's `ZSTD_reduceIndex`
  re-keys all three of its own. This one was missed: it is the only search
  structure not held on the match state, so all three compaction routes walked
  past it and left it naming bytes that were no longer there.

  It is direct-mapped and refilled only forward, so a stale entry is never
  corrected by a later block — the three-byte matches in that bucket are lost
  for the rest of the frame, and every further compaction loses more. Frames
  stayed valid and grew.

  Affects streaming only, at `btopt` and above with a `min_match` of 3, once the
  body passes about three windows. On a megabyte of the `wikipedia` corpus at
  `window_log` 17 it was worth 19.5% at `btopt` and 15.5% at both ultra
  strategies, rising with body length; streaming now lands within 0.1% of the
  one-shot encoder on the same bytes, where before it drifted by up to 19.6%.

  Streaming with a trained dictionary reaches this without setting any
  parameter, because an attached dictionary resolves to small tables that lean
  on the three-byte one: two megabytes of `wikipedia` at level 16 were 17.2%
  over upstream and are now 7.1% under it.

- **The binary tree threaded itself through positions outside the window.** Its
  inserts ran to the bottom of the buffer instead of stopping where upstream's
  do, at `ZSTD_getLowestMatchIndex(ms, target, windowLog)`. The insert records no
  matches, so this could never emit an illegal offset and nothing rejected it;
  what it did was leave the tree linked through positions the window had already
  dropped, so a later search followed those links, met its own floor, and gave up
  before candidates that were still in range. Frames stayed valid and got bigger.

  Affects the optimal parsers (levels 16 and up) whenever the body outgrows its
  window and has matches out near the window distance. On a megabyte of the
  `wikipedia` corpus with `window_log` 17 that was 10.93% at level 16, and it is
  now byte-identical to upstream; the same body at level 19 was 6.33%. Streaming
  is the other way in, since a stream past its window is the ordinary case: a
  3.5 MiB body at `window_log` 19 came down 1.91% at `BinaryTreeOpt`. Output at
  default settings is unchanged, including for bodies well past the window.

  This also closes the two largest gaps in the long-distance parameter sweep,
  6.3% and 7.7%, which were not about long-distance matching at all — the same
  rows were 7.81% over upstream with the matcher switched off.

- **A raw-content dictionary silently ignored a low `search_log`.** Loading one
  raised the match finder's search depth to a floor of 8, so a caller asking for
  `search_log` below 3 got 8 compares regardless and their override did nothing.
  Upstream floors nothing here. No compression level was affected — none pairs a
  `search_log` that low with a strategy that reads the depth — so this was only
  ever reachable by setting the parameter explicitly, and no default output
  changes. On the raw-dictionary corpus at levels 12 through 22 the override now
  takes effect and lands within three bytes of upstream, closing the largest
  known parameter-override gap in the suite, which was 29.6%.

- **The chain and row match finders dropped positions at every block
  boundary.** Both filed the last 64 positions of a block into their tables when
  the block finished, and moved their cursor to the block's end. Upstream files
  nothing there: a parser stops short of the block's last few bytes, leaves its
  cursor where it stopped, and the next block's first search catches up from
  there. Whatever sat between where the parser stopped and those last 64 bytes
  was therefore never filed at all, so later blocks could not match into it. On
  `json-records` at greedy the parser stopped 108 bytes short of a boundary and
  the next block missed a 79-byte match upstream took.

  Five corpus/parser rows that had differed from upstream without any
  long-distance matching involved are now byte-identical, and the sweep of the
  four long-distance table parameters went from failing to passing. One row went
  the other way by three bytes and is recorded as a known gap. Affects greedy, lazy
  and lazy2 — the three parsers that use a hash chain or a row table — on any
  input longer than one block. The fast pair never filed there, and the binary
  tree was already excluded because re-inserting positions it had covered
  corrupts it.

- **Streaming lost the binary tree's search state whenever it compacted its
  history buffer.** Compaction drops bytes off the front, so every position a
  match finder holds has to move with them. Chain and binary-tree finders index
  a table by `position & mask`, which subtracting alone cannot fix, so both were
  cleared and rebuilt. That is only wasteful for the chain, but a binary tree
  cannot be rebuilt from the bytes it describes: re-inserting a whole window in
  one go leaves most of each hash bucket unsorted and unreachable, so the tree
  the parser got back was worse than the one it had. The frames still decoded,
  just much larger — measured against the one-shot encoder on identical
  parameters, up to 321.7% larger, and 12.2% on `tabular-csv` at `btlazy2` with
  a window of 19. Affects every level from 13 up, and any explicit `btlazy2` or
  optimal strategy, on streams longer than twice the window.

  Both tables now survive it, by the two routes upstream's own overflow
  correction implies: the drop is rounded down to a whole number of the table's
  cycle where the cycle is the narrower, and where the cycle is wider than the
  entire buffer nothing wraps and the table shifts bodily instead. Between the
  two the buffer is allowed to hold one cycle, which leaves it at no more than
  three windows and two blocks where it was two windows and a block.

- **A long-distance match could extend backwards past the window floor.** The
  matcher grew a match backwards until it ran out of agreeing bytes or reached
  the start of its buffer, where upstream stops at the same window floor its
  forward search rejects candidates against. The sequence it emitted was still
  decodable, but longer than upstream's and starting earlier. Only reachable
  once a frame outgrows its own window, which is why the sweep that found it
  had to be widened to windows narrower than the corpus.

- **Literal symbols that tied on frequency got the wrong code lengths.** The
  Huffman table builder sorted symbols with an insertion sort, ported from an
  older upstream that used one; the pinned revision quicksorts any bucket over
  eight symbols. Symbols with equal counts come out of the two sorts in
  different orders, and the tree built from that order gives them different
  code lengths — the same total cost, a different table, and a frame that
  differs from upstream's on an input both parsed identically. Found on a block
  of json records where three hex digits appeared 169 times each. Across the
  benchmark sweep this closes two recorded size gaps and opens one, each a
  single byte.

- **A raw dictionary let double-fast reach past its window when streaming.**
  The ext-dict double-fast parser merges the dictionary and the frame into one
  logical index space, then tested candidates against the dictionary's floor
  alone. That is the bottom of the whole space, so source candidates were
  effectively unbounded, and a block whose floor forbade any match at all could
  emit one reaching twice the declared window. Reached through the streaming
  encoder with a raw dictionary, where a stream carrying no pledged size
  selects a 16 KiB window. Same shape as the binary-tree defect below, and
  fixed the same way.

- **btlazy2 reached a whole dictionary past its window.** A dictionary gives
  the match finders a second region to address, and every finder but one keeps
  the two apart, taking a floor into the prefix and a floor into the source.
  The binary tree does not: it indexes prefix position `p` at `p + 2` and
  source position `p` at `prefix_len + p`, one space covering both. It was
  handed the source half of the floor and compared it against those combined
  indices, so its bound fell short by the length of the whole dictionary and
  the parser emitted offsets past the window the frame declares. Frames like
  that are non-conforming, and this crate's own decoder rejects them.

  No compression level reaches it, since fitting the parameters to the source
  keeps the window at least as wide as the frame; a `window_log` override with
  a dictionary does. A sweep of seven strategies across window sizes,
  dictionary sizes, body sizes and levels failed on 119 of 1260 cases, every
  one of them btlazy2 and none from any other parser.

  Output is unchanged wherever the floor was not being violated: 132 ordinary
  dictionary encodes, all 22 levels across three dictionary sizes and two body
  sizes, are byte-identical before and after. Found by the dictionary fuzz
  target.

- **An empty dictionary produced a corrupt frame.** Passing a
  `PreparedDictionary` built from zero bytes is the same as passing no
  dictionary at all, and upstream treats it that way — it clears the dictionary
  slot before choosing a single parameter. This crate instead asked whether a
  dictionary was *supplied* rather than whether it had any content, and got two
  different things wrong as a result.

  The block loop sent an empty dictionary down a third encode path that rebuilt
  each block's history as a prefix slice. That path emitted matches which are
  not in the source — one twelve-byte match whose bytes agreed for four — so
  the frame failed its own checksum, and upstream's decoder rejected it too. It
  needed a block after the first and one of the optimal parsers; levels 13
  through 15 with more than one block were enough. Parameter selection then
  picked the unadjusted large-source row, so every empty-dictionary frame that
  did survive was larger than the same bytes with no dictionary: at level 13 on
  a small source that is a chain log of 22 against 12, and a different parser.

  An empty dictionary now takes the ordinary no-dictionary path and produces a
  byte-identical frame, which is what upstream does at every level measured.
  Found by the dictionary fuzz target.

- **A narrow window found no history at all.** Every parser but the fast pair
  took one match floor per block, measured from the block's end. Upstream
  computes it at the position doing the looking, so a match anywhere in a block
  may reach a full window back from *itself*. With a window as narrow as a
  block, the old floor landed exactly on the block's start and the parser found
  no earlier history whatsoever: roughly 900 extra literal bytes in every block
  after the first. No compression level could reach it, because fitting the
  parameters to the source already keeps the window at least as wide as the
  frame, but a `window_log` override could.

  Sweeping `window_log` 15 and 17 across seven levels and nine corpora,
  byte-identical output against upstream went from 98 rows of 126 to 112, and
  the same sweep against a raw dictionary cut its total deviation from upstream
  ninefold. Ordinary levels move too, on inputs larger than their window.
  Encode throughput is unchanged in the median; `log-lines` at level 15 is
  about 7% slower, which is the binary tree searching the history the fix makes
  reachable.

  Neither encoder shrinks a block to the window yet, so frames still declare a
  window wide enough for the blocks they emit; the fast parsers and the
  prefixed ones both still need it. A caller who sets `block_size` to the
  window gets upstream's frame exactly. See `docs/PARITY_PLAN.md`.

- **A hash wider than 24 bits took the fast parsers out of bounds.** The fast
  and double-fast match finders pack a table index and an 8-bit tag into one
  32-bit hash, so a `hash_log` or `chain_log` above 24 left no room for the
  tag: the shift underflowed, which panicked under a debug build and produced
  an out-of-range table index under a release one. No compression level gets
  near it — the widest hash on a fast row is 17 — but the new parameter
  overrides reach it directly.

- **The optimal parsers ignored a `target_length` of zero.** They substituted a
  per-strategy constant for upstream's `sufficient_len`, which upstream never
  does. Every optimal row in the level table carries a non-zero target length,
  so only an override could reach this; where it did, it was worth up to 6 KB
  on a 128 KiB frame.

- **A narrow window with an empty-content dictionary emitted an offset past
  the window the frame declared,** which this crate's own decoder then
  rejected. That encode path took its history floor from the block's start
  while the other two took it from the block's end. No compression level
  reaches it — adjustment leaves the window at least as wide as the source plus
  the dictionary — but a `window_log` override does.

- **A reused `Encoder` stopped reusing its match state.** The cache was keyed on
  the hash width the caller asked for rather than the one the finder built, and
  the two now differ wherever a finder clamps. Harmless in itself, but the key
  is the thing that has to reject a table built for different parameters, so it
  is now derived from the same function the finders use.

### Changed

- **`CompressionLevel::MIN` is now `-131072`**, the smallest level the codec
  accepts, rather than `1`. Code that used `MIN` to mean "the fastest ordinary
  level" wants the new `CompressionLevel::MIN_POSITIVE`, or `FASTEST`, which
  both stay at `1`. This is a silent behaviour change for anyone using `MIN`,
  which is why it is called out here; it is being made now because the crate
  has not shipped and the constant should mean what upstream's does.

- **`CompressionLevel::as_u8` is removed.** It cannot represent a negative
  level. Use `as_i32`, which its documentation already recommended.

- **Out-of-range compression levels are rejected rather than clamped.**
  Upstream clamps a level below its floor to the floor; `try_new` reports
  `InvalidParameter` instead, so that a caller who names a level nobody
  implements hears about it rather than silently getting a different one. Every
  level that is accepted stays byte-compatible with upstream.

- **The license is now `MIT OR Apache-2.0`**, replacing `BSD-2-Clause OR
  GPL-2.0`. The old pairing mirrored upstream `zstd`, but the GPL-2.0 arm was
  doing no work: nobody picks it when a permissive option sits beside it, and
  BSD-2 is already GPL-2 compatible for anyone who needs to combine with
  GPL-2-only code. The new pairing is the customary one for Rust crates and
  adds Apache-2.0's explicit patent grant, which neither BSD-2 nor MIT carries
  and which is worth having for a compression codec.

  A new `ATTRIBUTION.md` records that this is an independent implementation
  written against the reference library, and reproduces upstream's notice so it
  travels with every copy of the source. `deny.toml` no longer allows GPL-2.0
  dependencies; that entry existed only because the crate was itself GPL-2.0
  dual, and a copyleft dependency is now something the check must catch.

- **Literal-heavy blocks now decode through Huffman's double-symbol table.**
  Zstandard's Huffman decoder has two table shapes: the narrow one resolves a
  single symbol per lookup, the wide one packs a *pair* of symbols into most
  entries and emits two bytes per lookup. Only the narrow shape existed here,
  which is why decode throughput split by corpus — JSON, logs and CSV lagged
  upstream while match-heavy inputs matched it.

  Both shapes are now built, and the choice between them uses upstream's own
  cost model, so a given block decodes through the same shape upstream picks.
  The wide table costs more to build and less to run, so it wins on large,
  well-compressed literals and is declined on small or barely-compressible
  ones. A dictionary's Huffman table is always built wide, as upstream does.

  Measured against the previous decoder on the same frames: the literals stage
  itself runs 1.16x to 1.51x faster where the wide table is chosen, and
  whole-frame decode gains up to 17% on log-like data and 4-5% on JSON.
  Corpora where the model keeps the narrow table are unchanged. Nothing about
  the compressed format changes — both shapes decode the same bits.

- **`BENCHMARKS.md` and the README chart are regenerated against that decoder.**
  Decode now measures 99.6% of upstream at the median row, up from 97%, and 94%
  at the top levels, up from 90%; the slowest row anywhere rose from 79% to 81%.
  All 242 ratio rows are byte-identical to the previous report, so the two
  sweeps are directly comparable and both gates stay empty. The README's decode
  sentence is updated to match.

### Added

- **Dictionary training from samples**, the equivalent of upstream's
  `ZDICT_trainFromBuffer`. `train_dictionary(&samples, capacity)` builds a
  formatted Zstandard dictionary — content, entropy tables, dictionary ID and
  all — from a corpus of representative records;
  `train_dictionary_with_parameters` exposes the underlying fastCover controls
  (`k`, `d`, `f`, `accel`, `steps`, split point) and an explicit dictionary ID.
  This was the last part of the dictionary story the crate could not do: it
  could consume a dictionary but not produce one, so callers had to keep the C
  tooling around solely to run `zstd --train`.

  Two limits worth knowing. Trained dictionaries are *not* byte-identical to
  upstream's, because the trainer scores candidate contents by compressing
  samples with this crate's encoder and can therefore settle on a different
  segment size; measured against upstream's dictionary on held-out samples they
  land within about a percent either way, and the interop suite checks both that
  bound and that content selection is byte-identical for a fixed `(k, d)`.
  Training is also single-threaded, as `ZDICT_trainFromBuffer` itself is.

- **Encoding into a caller-owned slice, with a size bound to go with it.**
  `compress_bound` states the largest frame an encode can produce, and
  `encode_into_slice` (plus `Encoder::encode_into_slice` and its dictionary
  form) writes into a `&mut [u8]` you already have, returning the frame's
  length. Together they close the crate's only hard blocker for arena, FFI, and
  fixed-budget callers: every previous entry point owned a growable
  destination, so "compress into this buffer, allocate nothing" was not
  expressible. A buffer that turns out too small is `Error::DstSizeTooSmall`,
  never a truncated frame. Sizing a `Vec` with `compress_bound` also makes
  `Encoder::encode_into` allocation-free, which it could not previously
  guarantee because nothing told the caller how much to reserve.

- **`DecoderOptions::single_frame` rejects anything after the first frame.** The
  default concatenates every frame in the input and passes over skippable
  frames, matching the reference library; that is right for a `.zst` file and
  wrong for a compressed payload carried inside another protocol, where the
  enclosing framing already fixed the length and a second frame means the length
  was wrong. Decoding the first frame and reporting success hands the caller a
  truncated message that looks complete. With the option set, a second frame, a
  skippable frame, or a trailing byte is `Error::TrailingInput { offset }`. The
  default is unchanged.
- **`StreamingDecoder::input_consumed` and `unconsumed_input`.** How much of the
  pushed stream a frame used, and what followed it, so the same question is
  answerable without turning it into an error. `io::Reader` gains
  `into_inner_with_remainder`, which returns the compressed bytes the reader
  pulled from its source but did not consume — a chunked reader always overshoots
  the end of a frame, and those bytes were previously unrecoverable.
- **The library is checked on `wasm32-unknown-unknown` in CI.** Building without
  a C toolchain is the reason to prefer this crate over a binding on that
  target, where the C sources do not compile at all. The property held, but
  nothing verified it.
- **`Decoder` can decode into a caller-owned buffer.** `decode_all_into` and its
  options and dictionary variants replace the contents of a `Vec` you supply
  instead of returning a fresh one, so decoding a stream of frames costs one
  allocation rather than one per frame. This is the counterpart to
  `Encoder::encode_into`, and to upstream's `ZSTD_decompress` into a destination
  the caller owns. `Decoder` already claimed to amortize buffer allocation
  across calls, but only reused its internal scratch; for any real frame the
  output buffer is the larger allocation by far.
- **The `unsafe` code is checked for undefined behavior.** Nothing had ever run
  an execution-level UB checker against the crate, which has around 150 `unsafe`
  escapes and does unchecked indexing and unaligned wide loads throughout the
  entropy coders, match finders, and sequence executor. `cargo miri test --test
  miri` now does, weekly and on demand. It found six defects on its
  first run, all listed below; five were on the decode path and four of those
  need only a corrupt frame. `tests/miri.rs` is sized for an interpreter — small
  bodies, small blocks, and `flush` to reach block boundaries the encoder would
  never choose — because the rest of the suite uses bodies Miri needs many
  minutes apiece to walk. See `CONTRIBUTING.md` for how to run it.
- **The encoder is fuzzed.** All five existing targets only decoded, so nothing
  exercised the parsers, which is where most of the crate's unchecked indexing
  lives. Three round-trip targets now cover one-shot encoding across every level
  and block size, streaming encoding under adversarial chunking and flush
  placement, and the dictionary paths. Round-trip is a real oracle rather than a
  crash check: decoding rejects any offset beyond the frame's declared window, so
  the targets also verify that the parsers stay inside what they told the decoder.
  They found five defects before their first full run, all listed below.
- **The fuzz corpus survives between runs, and starts from real frames.** Every
  scheduled run used to begin from nothing and re-explore the same shallow
  ground; the corpus is now cached between runs and minimized before it is
  stored. `cargo run --example fuzz_seeds` writes seed inputs — valid frames for
  the decode targets, bodies with different match structure for the encode ones —
  which a fuzzer starting from random bytes would never assemble on its own.

### Fixed

- **The crate did not compile on recent nightly toolchains.** A hot path calls the platform `memcpy` directly, because for large copies it beats what LLVM inlines for `ptr::copy_nonoverlapping`. The declaration described it as taking and returning `*mut u8`; the real symbol is `void*`-typed. The standard library depends on this symbol, so the compiler checks any declaration of it against the true one rather than letting the two disagree at link time, and nightly began rejecting the mismatch. It now declares the C signature exactly. This is the second part of the same declaration to be caught this way — an earlier version returned `()` — and each break appeared only on a toolchain the crate had not yet been compiled with, since the check is nightly-only and CI's `beta` job passes throughout.

- **Streaming skipped the two-pass price seeding at levels 19-22.** Those levels
  parse the block that opens a frame twice: once to collect symbol statistics,
  then again with the price model seeded from them. The one-shot encoder did
  this; the streaming encoder never did, because the reference library hides the
  pass inside its block compressor while both encoders here drive their own
  block loop. The whole cost landed in the first block and then followed the
  frame: `wikipedia` at level 21 was 4.49% larger than the reference library's
  streaming output, and every level 19-22 row across the benchmark corpora is
  now within 0.01% of it.

  On `tabular-csv` the seeding is worth *less* than not seeding, so streaming
  output at those levels grows about 22% relative to the previous release while
  matching the reference library exactly. Streaming and one-shot output at a
  given level now agree with each other, which they previously did not.

- **A block whose matchable content started late was thrown away.** The fast and
  double-fast parsers gave up on a block outright once they had scanned an
  eighth of it without finding a match, treating the block as incompressible;
  everything past that point became literals. A block that *starts*
  incompressible and turns compressible later — a log with an embedded binary
  blob, or any stream whose block boundaries do not line up with its content —
  lost the rest of its content to literals. Upstream has no such early exit, and
  removing ours leaves every benchmark corpus byte-identical while costing
  roughly 15-25% encode throughput at levels 3-4 on input that really is
  incompressible.

- **Streaming split its blocks the way the one-shot encoder does.** The split
  heuristic ran once per full buffer, but the remainder was carried forward and
  re-split against the next buffer's input rather than emitted as its own block.
  The reference library's streaming path splits a 128 KiB chunk at most once,
  where its one-shot path re-splits the whole input; ours produced 99 blocks
  where upstream produced 16, costing 1.8% of compressed size at 1 MiB and 2.3%
  at 4 MiB on structured binary input. Streaming output is now byte-identical to
  the reference library's on the corpora where our one-shot output already was.

- **Reusing an `Encoder` across frames could read far past the end of the
  input.** The row match finder keeps its position table between frames and
  relies on rotating the hash salt to invalidate the old entries. Tags are only
  a few bits, so collisions get through, and the position such an entry carries
  belongs to the *previous* frame. The search filtered candidates against a
  lower bound only, which cannot reject a position that is too large: encoding
  a 1 MiB frame and then a shorter one on the same `Encoder` produced candidate
  indices up to 995424 against a 131072-byte source, and both the candidate
  prefetch and the match-length count take that index unchecked. Levels 4-6 and
  9-10 were affected, in release builds, through the public API. Candidates at
  or beyond the position being searched are now rejected. Output is unchanged
  wherever the invariant already held, which is everywhere upstream parity is
  measured.
- **The output cap and frame corruption were confused for each other.** The
  sequence loop folds the block-size limit and `max_output_size` into one
  counter for speed, and when it ran out it blamed the block-size limit
  unconditionally, so a caller who set `max_output_size` to bound a
  decompression bomb was told the input was damaged rather than that their
  limit had worked. The one-shot and streaming decoders returned different
  errors for the same bytes. Both limits are now tested in order, the
  block-size limit first: a sequence that overruns it describes a block the
  format cannot represent, which is true whatever the caller allowed, and
  reporting the cap instead would excuse a corrupt frame and send the caller
  back to a damaged archive with a bigger buffer. The hot loop is unchanged;
  the disambiguation happens only on the way out.
- **The published crate's test suite could not compile.** `src/encode.rs`
  included two files from `benches/`, which the manifest's `exclude` list keeps
  out of the tarball. `cargo package` did not catch it and cannot: it verifies
  that the packaged crate builds, and a build never compiles `#[cfg(test)]`
  code. The shared modules moved under `src/`, and CI now packages the crate and
  runs its tests against the unpacked result.
- **Dictionary compression indexed most of its own history under the wrong
  hash.** With a dictionary at the hash-chain levels, positions are addressed
  from a base equal to the dictionary length. Searches used that; the per-block
  insert used a value three smaller, so the positions it added were filed under
  the hash of the bytes just after them and no search could find them. The
  effect is a few bytes per block, which stayed invisible on the 512 KiB case
  the gap was recorded against and reached 145 bytes at 4 MiB. `raw-dictionary`
  level 4 is now 7 bytes smaller than upstream rather than 145 larger, and level
  5 matches it exactly, at every size.
- **The block boundary discarded the positions a long match had skipped.** The
  per-block catch-up for dictionary frames inserted the last 64 bytes of a block
  but moved its cursor to the block end, so everything between where the parser
  stopped and that window was never inserted and never reachable again. Those
  are the positions a long match jumped over, which is where the next block
  looks for a repeat of it. It now inserts the whole span, as upstream does.
- **The binary tree re-walked a whole block after a long match, at 3x the cost.**
  The tree records how far it has been filled and lets the next block bridge the
  gap. A block the optimal parser crossed in one long match left that gap a whole
  block wide, and bridging it is not free: every skipped position is inserted
  against a buffer that now ends a block further away, so on input with a long
  period each of those insertions counted a match running to the end of the
  block. On a 4 MiB body with an 845 KB period at level 16 that was 14,013 such
  counts against upstream's 28, and 2.9 GB of byte comparison against upstream's
  289 MB, for 10% fewer positions actually inserted. Upstream abandons the oldest
  part of the gap instead (`ZSTD_buildSeqStore`'s "limited update after a very
  long match"), which costs the tree entries nothing was going to ask for and
  holds the catch-up to 192 positions. Adopting it took the five encode rows that
  were below half of upstream to at or above it — `wikipedia` levels 16 to 18
  from 0.34-0.38x to 1.06-1.08x, `repeated-chunk` level 17 to 2.59x and level 22
  to 1.45x — and made the frames byte-identical to upstream at `wikipedia` levels
  16 through 20. Across the 4 MiB corpus at every level it is also a small ratio
  win: 21 of 242 rows change, the largest gains being 526 to 690 bytes on
  `mixed-entropy` levels 13 to 15 and 110 bytes on `raw-dictionary` levels 9 and
  10, against a worst loss of 30 bytes.
- **Incompressible literals paid a histogram over every byte to be rejected.**
  Literal compression counted the whole block before deciding it was not worth
  coding, which on incompressible input is nearly the entire cost of encoding:
  78% of samples on a 4 MiB pseudorandom body at level 1 were in the histogram
  and 4% in the match finder. Upstream counts 4 KiB from each end first and
  turns the block away on that evidence, gated on the ratio of literals to
  sequences, and this crate now does the same. Encoding pseudorandom input is
  4.3x faster at level 1, taking four rows from roughly half of upstream's
  encode throughput to 1.5-2x it. Output is unchanged: the sample reaches the
  same verdict as the full count on all 242 case/level combinations of the
  benchmark corpus, dictionaries included.
- **A homegrown incompressibility guess replaced with upstream's.** The check
  that was supposed to prevent the above sampled 128 strided bytes and skipped
  Huffman only if 112 of them were distinct — a threshold uniform random data
  does not reach, since 128 draws from 256 values yield about 101 distinct. It
  could not fire on the input it existed for, and where it did fire it forced a
  raw block that compression might have won. Its tests passed because the
  fixture laid out 128-byte runs and the sampler strode by exactly 128, so the
  distinct count was a property of the fixture's period rather than its entropy.
- **Literal compression searched for its Huffman table depth at every level.**
  Upstream keeps two ways of choosing that depth: a closed-form estimate, and a
  search that builds the tree at every candidate depth and keeps the smallest
  result. It runs the search only from `ZSTD_btultra` up, which is level 18 and
  above. This crate ran it everywhere, which wrote a different table than
  upstream does for the same literals — so frames diverged inside the literals
  section even where the parse agreed exactly. That was the whole of the
  trained-dictionary parity gap at levels 5 through 7: same sequences, same
  literal payload, a Huffman tree one bit deeper. The three parity tests that
  documented the gap now pass and are no longer skipped, and the crate has no
  ignored tests left. The search still runs where upstream runs it; without it
  there, `binary-structured` at level 19 would give up 312 bytes.

  As with the lazy-probe fix below, matching upstream cost the places where the
  non-upstream behaviour happened to win: 26 rows across the corpus grew, all
  but one by under 30 bytes. The exception is `binary-structured` at level 17,
  which went from 259 bytes under upstream to 39 bytes over — the search had
  been masking a separate divergence there, which is now visible and untracked.

- **The parity harness looked for upstream in the developer's own checkout.**
  It defaulted to `../zstd` and only fell back to an environment variable, so a
  sibling checkout sitting at anything other than the pinned revision made
  every parity test skip on a local run. `upstream-zstd/` inside the crate,
  which is the layout CI provisions, is now tried first. The diagnostic
  binaries also asserted "requires sibling `../zstd` checkout" whatever the
  actual problem was, and now report the real reason.

- **The encode benchmark charged upstream for work it timed nobody else doing.**
  The helper built and freed a `ZSTD_CCtx` inside its timing loop, so upstream
  paid a workspace allocation and match-table init on every iteration while this
  crate reused one `Encoder` — the mirror image of the decode-column flaw fixed
  above, and this one flattered us. The context is now built once, as upstream's
  own `zstd -b` does. It is worth up to 18% at level 17 and above and under 5%
  elsewhere. Every frame the helper emits is unchanged.

- **The row match finder's parser judged its second lazy probe by the first
  probe's standard.** Upstream raises the bar a candidate must clear as the
  parser looks further ahead, because deferring a match another byte costs
  another literal: `gain1` gets `+ 4` at depth 1 and `+ 7` at depth 2. The
  shipping copy of this parser used `+ 4` in both, so it accepted candidates
  upstream rejects, deferred matches, and paid the difference in literals. This
  is the parser behind levels 8 through 12; on `tabular-csv` it cost between
  0.85% and 1.10%, and all five levels now land within 22 bytes of upstream at
  unchanged encode speed. Matching upstream also gave up an accidental win:
  `json-records` at those levels was 0.10% to 0.18% *smaller* than upstream and
  is now within 15 bytes of it.

  The parser has a second copy used for tracing, and that copy was correct,
  so the block-trace harness reproduced upstream's parse exactly while the
  encoder did not. `traced_and_untraced_lazy_planners_agree_on_the_same_block`
  exists to catch that, but covered only the binary-tree family; it now runs
  over every lazy-family strategy.

- **The optimal parser stopped searching once the body outgrew its match tree.**
  The binary-tree walk was floored at `btLow`, the low end of the children roll
  buffer. Upstream floors it at `windowLow` and uses `btLow` only for a break
  *inside* the loop, taken after the candidate it lands on has been compared, so
  folding it into the loop condition threw away every match reached from further
  back than the buffer. `btLow` is zero until the body passes
  `1 << (chainLog - 1)` bytes, which hid this everywhere except level 16 — the
  one optimal level whose tree is smaller than a 4 MiB body. On `json-records`
  it cost 7.56%; level 16 now lands 6 bytes under upstream at unchanged encode
  speed, and every corpus case is at or below upstream at levels 16 and 17.

- **The lazy parser gave up on better matches too early at levels 13 to 15.**
  It skipped the depth probes whenever the first match it found already reached
  64 bytes, described in the code as upstream's `sufficient_len` optimization.
  Upstream has no such thing in the lazy family: `sufficient_len` belongs to the
  optimal parsers, and `ZSTD_compressBlock_lazy_generic` probes for every match
  of four bytes or more. The shortcut only bit with a binary-tree match finder,
  where the first match routinely clears 64 bytes, so it cost about 5% of ratio
  at levels 13 through 15 and left levels 6 through 12 byte-identical. On
  `json-records` it turns level 13 from 3.15% worse than upstream into 5.1%
  better, and level 14 from 0.29% worse into 5.8% better, at no cost in encode
  speed. One byte moves the other way, on `raw-dictionary` level 9.
- **Block traces reported a parse the encoder had not made.** The traced and
  untraced copies of the lazy planner had drifted: only the untraced one carried
  the shortcut above, so from levels 13 to 15 the trace harness described a
  different parse from the one being shipped. Removing the shortcut makes the
  two agree, and a new test holds them together. Nothing had, because tracing
  defaults to on under `cfg(test)`: every unit test was exercising the traced
  copy and none was exercising the copy that ships.
- **`compare_ratio_rows` could not read a compressed literals section.** It
  re-derived the literals header instead of using the decoder's parser, with the
  wrong bit widths, and then took `Regenerated_Size` for the section's on-wire
  length where a compressed section stores `Compressed_Size`. Every compressed
  block hit its bail-out path and reported the whole payload as literals with
  zero sequences, so the per-block breakdown the tool exists to print was blank
  for exactly the blocks worth looking at. It now calls into the decoder. It
  also takes `--input-file` and `--skip-bytes`, so a body can be a slice of a
  file rather than a generated case. A generated case can only grow from its
  start, which conflates "this tail is harder" with "the compressor behaves
  differently this far into a stream" — feeding the tail back as a body of its
  own is what separated them, and is how the level 16 defect above was placed.
- **The benchmark report's decode column measured allocation, not decoding.**
  Upstream's helper allocates its destination once and reuses it for every
  timed iteration; this crate's side called `decode_all`, which returns a fresh
  `Vec`, so the two columns were never measuring the same thing. Usually the
  cost is nil, because the allocator hands the same block back; occasionally it
  is not, and the same json-records level-22 row read 4182 MiB/s benchmarked
  alone and 2353 MiB/s inside a seven-level sweep — identical frame, identical
  code, one run apart. Every row below half of upstream was being read as a
  decoder gap. The report now decodes into a hoisted buffer, matching what it
  compares against. The encode column never had the problem because
  `encode_into` existed and it used it.
- **Decode stage profiling covered one block of a 512 KiB input at levels 3-7.**
  Every row it was meant to explain is a whole frame of 4 MiB, and the slow ones
  are all at level 7 and above, so the profiler and the problem barely
  overlapped. `profile_decode_stage` now defaults to the report's own input size
  and profiles the whole frame, with `--first-block-only` for the old behavior.
  Its `--throughput` mode reports the real fused decode path, which the stage
  attribution does not use.
- **The decoder's overlapping-match copy computed a pointer offset near
  `usize::MAX`.** C's `ZSTD_overlapCopy8` does `*ip -= dec64table[offset]` and
  then `*ip += 8`; the port fused those into a single `add` of their difference,
  which is negative for match offsets 5, 6, and 7. Routed through `usize` that
  became an enormous value, and `pointer::add` is undefined once its offset
  leaves `isize` regardless of where the address lands — and it landed in bounds
  every time, which is why no test, sanitizer, or fuzzer could see it. Offsets
  that small are ordinary in real data, so this was on the decode path of most
  frames. Now applied as a signed `offset`.
- **Four more decoder paths computed out-of-range pointers before checking
  them.** The bounds checks were all present and correct; they just ran one step
  too late, and `pointer::add` is undefined the moment it leaves the allocation.
  The literal cursor advanced by an unvalidated literal length, the match
  prefetch used an offset validated eighteen lines later, and `BitDStream`
  derived a comparison limit as `start + size_of::<usize>()` — past the end for
  any bitstream shorter than one word, which is what a truncated frame supplies.
  All now form the address with `wrapping_add`, which keeps provenance, moves
  only the address, and leaves the existing checks doing their job.
- **The decoder's literals buffer had notional rather than real over-read
  headroom.** Literals are copied in fixed 16-byte units to match
  `ZSTD_execSequence`, so a run starting within 16 bytes of the last literal
  reads past it; C allows that by giving its buffer `WILDCOPY_OVERLENGTH` of
  slack. This crate used `Vec::reserve`, which is not the same thing: a slice's
  provenance ends at its length even when the allocation continues, and spare
  capacity is uninitialized. The headroom is now inside the slice and
  initialized, carried by a type that keeps it distinct from the literal count
  so padding can never be mistaken for data when bounding what a block may
  claim.
- **The encoder built a 256-entry histogram from partly uninitialized memory.**
  `analyze_all_codes` zeroed only the first 36, 32, and 53 entries — matching
  C's `HIST_count_simple`, whose arrays are sized to exactly that — and then
  called `MaybeUninit::assume_init` on the full 256. That is undefined on
  construction whether or not the tail is read, and it was read: the array is
  copied by value into `SequenceCodeStats` and copied again when choosing a
  compressed table. It runs for every compressed block.
- **The encoder could read up to 16 bytes past the end of the caller's buffer.**
  Short literal runs are copied with a fixed-width 16-byte copy, and the check
  guarding it only looked at the destination. A run ending near the end of the
  input read past it — out of bounds on any buffer without slack, which
  AddressSanitizer confirms on a 1.5 MiB body at level 1. Both ends are now
  bounded, as upstream's `ZSTD_storeSeq` does.
- **The sequence plan could write past the end of its own buffers.** Its
  capacity is re-reserved for each block, but the request was computed against
  the existing capacity while `Vec::reserve` measures from the length — which is
  zero right after the clear — so a buffer that already held capacity grew by
  nothing and the plan wrote sequence codes past the end of the allocation.
  Reachable by flushing blocks smaller than earlier ones.
- **A one-to-three byte body with a dictionary panicked at levels 4 through 8.**
  A block that short is emitted raw, but the encoder still indexes its positions
  for later blocks, and the bound on that loop admitted position 0 even when the
  body was shorter than the four-byte hash key.
- **A reused `Encoder` compressed worse on every call after the first.** The
  optimal parser's node array was kept across calls for its allocation but never
  reset, so a later parse read the previous one's prices at positions it had not
  written. Output stayed valid, so nothing failed — levels 13 through 15 simply
  alternated between two different results for the same input. The same input at
  the same level now produces the same bytes on every call.
- **Flushing a block of eight bytes or fewer panicked at levels 16 and 17.** A
  block that short is priced from the predefined tables, and the literal
  statistics it should hand to the next block were only maintained for blocks
  priced dynamically. The next block then built a dynamic model on a zero
  literal sum and underflowed its literal price. C keeps the two apart, and so
  does this now.
- **A trained dictionary at level 2 produced a frame this crate could not read
  back.** The window bound below was applied to the parsers that work without a
  dictionary, but the declared window was tightened for all of them, so the
  dictionary path could still emit an offset up to a block wider than the frame
  declared. On a 4 MiB body that was an offset of 1142716 against a 1048576-byte
  window. That path now measures its floor from the end of the block too.

- **Every parser could reach a block further back than the window, which cost
  ratio and forced frames to declare more memory than they need.** The floor on
  how far back a match may start was measured from the start of the block being
  encoded rather than its end, so a block could match up to `window +
  block_size` bytes back. Those over-wide matches spent offset bits and, on a
  body whose period sits just inside the window, displaced the repeat offset the
  encoder would otherwise have ridden for the rest of the frame: on 4 MiB of
  JSON records level 1 emitted 305581 bytes against upstream's 242386, a gap
  that stayed at zero up to the window and then grew with frame length. It now
  emits 242367. Across the corpus at 4 MiB, 24 case/level rows improved and none
  moved more than 0.2% the other way; at 16 MiB, 8 improved and none regressed.
- **Frames declared a window one block larger than the level's.** With matches
  now bounded by the window itself, `Window_Size` is the level's window exactly,
  matching upstream at every level. A decoder no longer reserves 128 KiB no
  frame ever needed, and these frames no longer cross upstream's memory limit a
  level sooner than upstream's own.

- **Streaming encode was quadratic, and compressed worse than one-shot.** The
  encoder built a new match finder for every block and re-inserted its whole
  retained window into it, so cost grew with the square of the frame length
  until the window filled and stayed proportional to `window / block_size`
  after that. Levels 13 and up also compressed 4x worse than one-shot, which
  had looked like a separate defect and was the same one. The match finder now
  lives for the whole frame. At level 15 a 526 KB stream went from 0.438s to
  0.017s and from 47117 bytes to 43701, matching one-shot. Streaming now lands
  within a few bytes of one-shot at every level.
- **Streaming ended blocks by size where one-shot ends them by content.** Once
  the match finder persisted, block boundaries started to matter, and on inputs
  whose statistics shift within a block a fixed cut cost several times the
  compressed size: level 1 emitted 92747 bytes where one-shot emitted 33741.
  Streaming now runs the same split heuristic, reading one block of buffered
  input to do it.

- **A tiny frame could occupy the streaming decoder for minutes.** Decode kept
  its sliding window in a `Vec` and retired the oldest byte with `drain(..1)`, a
  copy of the whole window per byte produced, so cost grew with output times
  window rather than with output. A 329-byte frame expanding to 3.2 MB took 42
  seconds, against well under a millisecond for the same frame decoded in one
  shot. The window is now a ring buffer and matches are copied in slices, which
  also made the test suite about twenty times faster. Streaming decode is no
  longer slower than one-shot on any shape measured, and `StreamingDecoder` has
  gained the fuzz coverage it never had.
- **The decoder accepted a corrupt frame and returned invented data.** A Huffman
  weight description too short to hold its two initial FSE decoder states was
  decoded anyway, yielding seven bytes that were never in the input, where
  upstream reports corruption. This is the frame upstream ships as
  `truncated_huff_state.zst`. All three files in its
  `golden-decompression-errors` corpus are now asserted to be rejected, and the
  parity suite walks that directory so new cases are picked up rather than
  skipped.
- **Out-of-bounds write in the decoder on a hostile frame.** A four-stream
  literals section splits its output at multiples of
  `ceil(regenerated_size / 4)`, which for a regenerated size of 5 puts the
  fourth segment's start one byte past the end. The decode loop writes through
  unchecked indexing bounded by those starts, so this corrupted memory in a
  release build rather than panicking. Upstream rejects every regenerated size
  below 6; the decoder now does too.
- **Decoder panic on a 40-byte hostile frame.** The Huff0 literals decode table
  was declared with half the entries the code indexing it assumed, so a
  literals section whose Huffman weights imply the largest legal table log ran
  off the end of the array. The symbol lookup on that same table also reads
  with unchecked indexing, justified by a comment naming the full size, so the
  bound was restored rather than the panic merely caught.

  Both were found by the fuzz suite within a minute of its first real run. The
  suite is named in `docs/SEMVER.md` as what enforces the decoder's panic-free
  guarantee, and it had never once run to completion in CI.

- **The crate did not compile on Linux, or on any beta toolchain.** A local
  `extern "C"` declaration of `memcpy` omitted its `*mut u8` return value, and
  newer compilers reject a mismatched signature for a symbol the standard
  library owns ("invalid definition of the runtime `memcpy` symbol"). Six of
  the eight CI legs failed on this before running a single test.

- **Panic when compressing at levels 16-22 in a debug build.** The binary-tree
  match finder bounded its traversal by `btLow`, which is zero until the tree
  outgrows the chain table, where C bounds the same loop by `windowLow` and
  never lets it reach the two phantom positions at the start of the window.
  Walking onto them underflowed the un-biasing subtraction: 64 KiB of ordinary
  log text at any optimal level aborted inside the library. Release builds
  wrapped instead and exited the loop on the next bounds check, so compressed
  output is unaffected — verified byte-for-byte across the change.

- **17-21% of compression ratio at levels 5 through 15, and a total failure to
  compress globally periodic data.** A sampling heuristic inspected 32 four-byte
  values of a block and, if they looked random, emitted the whole block raw
  without planning it or consulting history. When it guessed wrong the cost was
  the entire block: on 3.7 MB of C sources one misclassified 128 KiB block
  accounted for essentially the whole 113 KB gap against upstream. On data that
  is locally random but repeats every 1 MiB it was worse — the frame-level form
  of the check inspected only 128 KiB of history, so it declared the whole frame
  incompressible and returned 4,194,409 bytes where upstream returned 1,048,881.
  Upstream has no such bypass; it compresses and falls back to a raw block on
  measured size. The heuristic is removed, and every level 1-22 now lands within
  ±0.6% of upstream on that corpus. The cost is real but narrow: encoding 4 MiB
  of genuinely incompressible input at level 19 goes from ~0s to ~0.29s.

- **Corrupt output at levels 19-22.** The sequence-bitstream writer did not
  flush after emitting the first sequence's extra bits, so the encode loop
  started with an almost-full bit accumulator and overflowed it. Blocks whose
  final sequence carried wide literal-length, match-length and offset fields
  at once produced frames that neither this crate nor the reference `zstd`
  CLI could decode. C flushes at this point; we now do too.
- **Streaming frames the crate could not decode itself.** The streaming
  encoder declared `Window_Size = block_size` while retaining a full block of
  history, so offsets reached nearly twice the declared window. 400 KB of
  ordinary text at default options failed at levels 1 and 3.
- **Streaming ratio at levels 16 and up.** The optimal parsers cached their
  binary tree across blocks. That is correct for a dictionary, whose prefix is
  fixed, but not for streaming, where the prefix is the frame history and
  grows every block, so later blocks were parsed against a stale tree. On
  1.5 MiB of log text this cost roughly 17x in ratio.
- **Streaming compression beyond one block.** Retained history was clamped to
  `block_size`, discarding every match further back. It now holds the level's
  full window, matching the one-shot path.
- **Frames larger than 128 MiB were undecodable by the reference CLI.** The
  one-shot encoder always emitted a single-segment header, setting
  `Window_Size = Frame_Content_Size`. It now declares a single segment only
  when the content fits the window the level actually needs, and otherwise
  writes a window descriptor *and* the content size — the two header fields
  are independent, and APIs shaped like `ZSTD_decompress` need the size up
  front.
- **Uncatchable abort on a hostile frame.** `decode_all` reserved the declared
  `Frame_Content_Size` directly; a 17-byte frame declaring 2^46 bytes aborted
  the process inside the allocator. The reserve is now bounded by what the
  remaining input could expand to.

### Changed

- **A long match is copied by doubling rather than by a fixed-stride wildcopy.** The match copy advanced 32 bytes per iteration, or 8 at an offset below 16, however long the match was — so a 128 KiB match cost thousands of iterations to reproduce what is a single periodic pattern. It now seeds one period and then copies from a distance that doubles each round, about a dozen `memcpy` calls, degenerating to the one `memcpy` it always was when the offset is at least as long as the match. `small-alphabet` decodes 10x faster; both decoders benefit. The reference library copies the same way we used to, which is why this corpus already sat at parity in `BENCHMARKS.md` and read as having nothing left to win.

- **Streaming decode is several times faster, and now runs at 80-94% of the one-shot decoder where it ran at 15-27%.** It had been keeping two copies of every decoded byte: one in the buffer the caller drains, and one in a separate ring buffer that matches were read back out of. History now lives in the caller's buffer, so a match is a copy from earlier in the same allocation and a block goes through the same executor the one-shot decoder uses. Bytes are released off the front once the caller has drained them *and* they have fallen out of match range. Also gone from every block: a copy of the compressed payload, a freshly allocated literals buffer, a materialized sequence list, a duplicate FSE table build, and a checksum call per literal run and per match. `StreamingDecoder::take_output` now copies when called mid-frame, because the buffer it used to hand over is also the match history; between frames it still hands it over, so pushing a whole stream and taking the result once does not copy.

- **The streaming encoder's output size is now gated, not just its
  readability.** The upstream interop tests checked that our streamed frames
  round-tripped and nothing more, and readability is the one property a ratio
  regression never breaks: the encoder that rebuilt its match finder on every
  block emitted four times one-shot and every byte of it decoded correctly.
  Streaming output is now held against upstream at level 3 across four sizes,
  and the cost of a mid-stream flush is held against the same payload streamed
  without one. Both bounds were calibrated by measuring what they reject, not
  chosen for headroom. The two streaming-versus-one-shot bounds in
  `tests/property.rs` are re-derived the same way: the pinned one goes from 10%
  to 128 bytes on a shape where the measured difference is a constant 3, and
  the generated one from 2x to 1.5x against a worst case of 1.387x measured
  over all 8448 cases its generator can produce.
- **`BENCHMARKS.md` is regenerated, and both of its gates are now empty.** The
  published report had gone stale across eight commits and overstated the gap in
  both columns. Re-measured at 4 MiB per case over all 22 levels against the
  pinned `v1.5.7`: no encode or decode row is below half of upstream's
  throughput, where nine encode rows were; and no ratio regression exceeds 1%,
  where five did. Of the 47 rows still a byte or more larger than upstream, 32
  are under 0.02%, and the largest excess anywhere is 145 bytes on a 4 MiB case.
- `benchmark_report` reports a missing or misplaced upstream checkout the way
  the other diagnostic binaries do, naming the actual cause and the environment
  variable that overrides it, rather than asserting "requires sibling ../zstd
  checkout" — which names neither, and had been wrong since the pin landed.
- `CompressionLevel` is now backed by `i32` rather than `u8`, matching
  upstream. `try_new` takes an `i32` and `as_i32` is the primary accessor;
  `as_u8` and `TryFrom<u8>` remain. This is what lets negative "fast mode"
  levels be added later without changing any signature. **Breaking:**
  `From<CompressionLevel> for u8` became `From<CompressionLevel> for i32`.
- `Error` and `UnsupportedFeature` are `#[non_exhaustive]`. **Breaking:**
  `match` over them now needs a wildcard arm. Adding a variant was previously
  a major-version change, which the roadmap would have forced almost
  immediately.
- `Encoder`, `Decoder`, `StreamingEncoder`, `StreamingDecoder`, and
  `PreparedDictionary` implement `Debug`. Their absence meant no downstream
  type holding one could `#[derive(Debug)]`.
- Documentation stated the public level range was `1..=9`; it has been
  `1..=22` since the optimal parsers landed. `README.md`, `ROADMAP.md`, and
  the crate docs are corrected.
- `DecoderOptions::default()` now caps the window at 128 MiB
  (`DecoderOptions::DEFAULT_MAX_WINDOW_SIZE`), matching upstream's
  `ZSTD_WINDOWLOG_LIMIT_DEFAULT`. It was previously unbounded.
- Documentation no longer claims the crate contains no `unsafe`. It contains
  148 individually annotated uses, confined to the entropy coders, match
  finders, and sequence execution loop; CI now fails if that count grows.
  `README.md`, `SECURITY.md`, `CONTRIBUTING.md`, `docs/THREAT_MODEL.md`, and
  `docs/SEMVER.md` are corrected.
- Property tests now cover every level `1..=22` (was `1..=9`) and include
  inputs above 1.5 MiB, the threshold past which cross-block encoder defects
  appear. The benchmark corpus default rose from 512 KiB to 4 MiB for the
  same reason.
- The MSRV is now **Rust 1.96**, raised from a declared 1.85 that was never
  true. The crate has not built on 1.85 since `slice::as_chunks` was used in the
  match-length counter — that was stabilized in 1.88 — so the old number was
  documentation rather than a supported configuration. The MSRV leg of CI now
  tests the version actually claimed.
- The upstream revision the parity tests compare against is pinned in
  `upstream-zstd.ref` and verified at run time, and the pin moved from
  `v1.5.6` to `v1.5.7`. Previously CI hardcoded one revision while local runs
  used whatever happened to be checked out at `../zstd`, so the two were not
  comparable; against `v1.5.6` the library suite had 42 failures, and against
  `v1.5.7` it has none. Parity tests still skip when no checkout is present,
  but now say why, and `ZSTANDARD_REQUIRE_UPSTREAM` makes skipping an error so CI
  cannot silently stop comparing. `ZSTANDARD_UPSTREAM_DIR` selects a checkout
  other than `../zstd`.
- The benchmark report's ratio-regression list compares compressed byte counts
  and shows the byte and percentage delta, largest first. It previously compared
  ratios rounded to four decimal places, so a row 40 bytes larger printed
  identically to one 26% larger; 56 of the 62 rows it flagged were within 1% and
  nothing distinguished the ones that were not. The summary now also counts how
  many exceed 1%.
- `BENCHMARKS_full.md` is removed. It was last regenerated in April at 512 KiB
  per case and recorded no upstream revision, having predated the pin, so its
  numbers could not be compared against anything. `BENCHMARKS.md` covers the
  same 11 cases and 22 levels at 4 MiB against a pinned `v1.5.7`.
- Benchmark throughput is the fastest of three timing trials rather than one
  timed run, at the same total work. Background load can only make a trial
  slower, so a single sample put rows on either side of the report's 50%
  threshold depending on what else the machine was doing: three back-to-back
  measurements of identical code moved `log-lines` L16 decode across it, and two
  full sweeps disagreed on 34 decode rows over a code path neither had changed.
  The ratio columns are byte-exact and were never affected.

### Added

- `zstandard::io`, providing `Writer<W: Write>` and `Reader<R: Read>` so the codec
  composes with `io::copy`, `BufReader`, HTTP bodies, and anything else built
  on `std::io`. Includes `impl From<Error> for std::io::Error`, which preserves
  the underlying `Error` for `io::Error::downcast`.
- `docs/SEMVER.md` documenting the project's versioning policy, MSRV policy,
  and explicit list of what is and is not a breaking change.
- `docs/THREAT_MODEL.md` documenting the trust assumptions the decoder makes
  about its callers and what soundness guarantees the decoder provides.
- Property tests (`tests/property.rs`) covering one-shot roundtrip across
  every public level, arbitrary-chunking streaming roundtrip, dictionary
  roundtrip, and decoder no-panic on random input.
- `.oss-fuzz/` template directory with `project.yaml`, `Dockerfile`, and
  `build.sh` ready to copy into a `google/oss-fuzz` PR.
- `.github/workflows/coverage.yml` CI job running `cargo llvm-cov` and
  posting LCOV output to Codecov.
- `BENCHMARKS.md` now records the `git describe` of the upstream `../zstd`
  checkout used to generate the report.

### Changed

- Diagnostic encoder/decoder tracing and stage-profiling APIs (`BlockTrace*`,
  `trace_first_block_*`, `profile_first_block_*`, `EncodeStageProfile`,
  `DecodeStageProfile`) are now gated behind the non-default `internal-trace`
  feature and marked `#[doc(hidden)]`. They are maintainer tools tied to
  internal encoder/decoder structure and not part of the supported public
  surface.

### Fixed

- Cross-block history reuse: when the early-raw heuristic bypassed a block,
  the match state was left empty, causing subsequent blocks that overlapped
  with the bypassed content (duplicated payloads, deduplicated streams) to
  also fall through to raw and lose compression for the rest of the frame.
