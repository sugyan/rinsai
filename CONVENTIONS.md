# rinsai — frozen conventions

What the code already assumes. **Changing any of these needs a
[FAQ.md](./FAQ.md) answer**, because something is built on it.

Organised by subject rather than by the step that froze it. A rule here is what
the code assumes, stated in a few lines; if it takes a paragraph, what is being
written is the argument, and the argument belongs in the FAQ. The
moment a rule lands here, **the entry behind it retires to a line**, in the same
pull request.

## Source layout

- **A module with children is `foo.rs` beside `foo/`, never `foo/mod.rs`.** It
  is shunsai's layout too (`src/sliders.rs` beside `src/sliders/`), so it is the
  family convention rather than a preference.

## Vocabulary and scores

- **A bare `Position` means `shunsai::Position`** — the board a search walks,
  and the only one of the two with unmake and an incremental Zobrist key.
  `shogi_core::Position` is a *record* (root, current position, moves played) and
  `Game` delegates that half to it under the alias `Record` — `shogi_core::Game`
  was weighed for the same job and is `Position` plus a `GameResolution`, which
  becomes interesting at E2's declaration handling and not before; it is never
  imported unqualified. Its `from_usi` is not used at all, for the reason under
  `Game::from_usi_position`'s doc — **do not confuse "we cannot parse with it" with "we
  cannot store in it"**, which is the mistake the first draft of `Game` made.
- **Evaluation is negamax from the side to move.** Positive is good for whoever
  is to move; a parent takes `-child`. At the root the side to move is the
  engine, so USI's `score cp` needs no flip.
- **Centipawns, pawn = 100.** E3's network emits its own scale, and the
  conversion at the inference boundary is exactly where silent strength drift
  lives.
- **`Depth` is signed whole plies.** No fractional `ONE_PLY` scheme. Signed
  because quiescence runs at negative depth, counting down from zero towards
  `-QS_MAX_CHECK_PLIES` — note the sign; the constant itself is positive.
- **`MAX_PLY = 128`**, sizing the search stack, the mate band and the move
  buffer together.
- **Three score bands, in this order: evaluations, then a repetition won or
  lost, then the mates.** `clamp_to_eval` clamps below the *lowest* of the two
  upper bands, so clearing it clears both. ⚠️ A repetition value is **flat** —
  it carries no distance to the root, unlike a mate score — which keeps the
  transposition table's mate-by-ply adjustment the only ply-relative score in
  the engine.

## Evaluation

- **Piece values are a starting point for SPSA at E4, not a measurement.** The
  ordering comes from the rules of shogi — empty-board destination counts,
  slider or stepper, colour-bound or not, promotion potential — the magnitudes
  are interpolated on that ordering, and everything is rounded to a multiple of
  five so that nobody reads them as fitted. Pawn = 100 anchors it. Two
  modelling choices worth naming: a **promoted minor is exactly a gold on the
  board**, because it moves exactly as a gold and anything else would be a claim
  about the position rather than the material; and **a piece in hand is worth
  more than the same piece on the board**, by the widest margin for the kinds
  whose board placement is most constrained. The margins themselves are the
  table in `eval`. "と金は金以上" then falls out on its own.
  The one known simplification: 成銀 is genuinely worse than 銀 sometimes (a
  silver retreats diagonally, a promoted one does not) and this model says it is
  always 50 better. Left unmodelled deliberately; E3 replaces the whole table.
- ⚠️ **The values are ours, and that is a licensing property, not a preference.**
  CLAUDE.md forbids reusing a table from a GPL engine. The rounding to fives is
  the visible evidence, and `eval::tests::every_value_is_a_round_number` is what
  keeps it true.

## Search

- **Node counting: one node per `search`/`qsearch` entry, including the root,
  excluding bulk-counted leaves.** Frozen because these numbers are a
  regression test, and a convention changed afterwards invalidates the whole
  committed set.
- **The poll interval is 1024 nodes**, checked at the top of the interior node,
  at the top of every quiescence node, and once per deepening iteration — *not*
  per root move, which would break the first-root-move guarantee below.

  ⚠️ **Quiescence has to poll.** The test is an *exact* multiple, safe only
  while every increment inside the tree is seen by something that polls; a
  quiescence search that counted without polling could step clean over every
  multiple, missing `stop`, the deadline and the node limit unpredictably.
  Sabotage: drop the quiescence poll —
  `a_deep_search_still_answers_a_node_limit` goes red.

  ⚠️ **`negamax_root`'s own increment is not polled**, so an iteration
  beginning at `nodes ≡ 1023 (mod 1024)` consumes that multiple at the root
  with no poll. Bounded and benign, but an exception to the sentence above
  rather than an instance of it.

  ⚠️ **`self.nodes` accumulates across iterations and must not be reset per
  iteration**, or the poll starves where no single iteration reaches 1024 nodes
  on its own. This is **not** what makes the first root move complete — that is
  `Budget::without_limits` below, which stands on its own. Two separate
  properties of one counter; welding them with a "because" is a mistake this
  project made once already.

- **The search answers with a move it actually searched, unless `stop` cut the
  first root move short.** The mechanism is structural: **the first iteration's
  first root move runs against `Budget::without_limits`**, the same budget with
  the clock and the node limit suspended. Without it, a poll landing inside that
  subtree leaves `negamax_root` returning `None`, the deepening loop breaking,
  and the answer sitting at the unsearched seed — a move in shunsai's
  unspecified generation order, with no `info` line emitted at all.

  ⚠️ **Root move 0 and no further.** Alpha is still `-INFINITE` there, so any
  finite score raises it; relieving the rest of the iteration buys nothing.

  ⚠️ **`signals.stopped()` stays live** through it, which is why it is a second
  budget rather than a flag that skips the poll.

- **Each iteration re-seeds the root list with its own answer.** Without it, an
  iteration cut short reports the best of whatever prefix it reached, which can
  be a *worse* move than the last completed iteration chose. Root-only; not the
  interior move ordering E1 is about.

- **`MoveBuf::get` returns a `Move` by value, not a slice**, and that is what
  makes the recursion compile: a slice would borrow the buffer across the
  recursive `&mut self` call. **The root move list lives outside the buffer**,
  because it persists across iterations and gets reordered.

- **千日手 is decided where a child is dispatched, not at the top of the
  interior node.** Two things depend on the site: the depth-1 iteration
  dispatches straight into quiescence, so a check inside the interior node
  leaves it blind to the move that ends the game; and a cut-off that enters
  neither node function leaves the node-counting convention above untouched.
  ⚠️ The child's line has to be cleared on that path, or a parent that raises
  alpha on the verdict publishes a variation running past the end of the game.

- ⚠️ **A repetition verdict does reach the transposition table, by way of the
  parent.** The verdict node's own key is never probed or stored, but the parent
  takes the returned score as its `best` and stores it under a key that carries
  no path. So a later probe can hand back a draw or a 連続王手 score for a
  position that no repetition reached. This is the accepted imprecision, not a
  guarantee.

- **The repetition path is extended at interior nodes only, and quiescence is a
  deliberate hole in it.** Quiescence is the overwhelming majority of all nodes,
  so keeping the push and the scan out of it is most of what the rule costs. The
  hole is narrow rather than closed: a quiescence subtree's *entry* position is
  one its interior parent pushed, and a quiescence line cannot return to that
  entry — every ply but at most `QS_MAX_CHECK_PLIES` is a capture, and the only
  move quiescence has that puts a piece back on the board is a drop from an
  evasion. ⚠️ **That argument does not cover a quiescence position coinciding
  with one from the game's own history**, which would be a fourth occurrence the
  search does not see. ⚠️ **E1's SEE-in-quiescence item ends the bound too**:
  once quiescence generates checks and non-capture promotions a quiescence node
  can play a quiet move, so that item owns extending the path into quiescence.

- **A child gives the move buffer back exactly as it found it, asserted at the
  boundary rather than left to a test.** Forgetting a `truncate` is a leak, not
  a wrong answer — every ply reads its own base, so the search still plays
  correctly — so no ordinary test sees it, and with two node types an interior
  node's own `truncate` destroys the evidence of anything its quiescence
  children left behind. The invariant is a `debug_assert_eq!` on either side of
  every child call.

## Time control

- **`movetime n` is taken exactly; every other clock produces an allowance the
  engine derives.** Nothing is held back from `movetime` — it is a caller
  holding its own clock. `byoyomi 0` means "this time control has no byoyomi",
  not a zero-millisecond budget, and on its own it names no clock at all.

- **The byoyomi period is spent before any main time is.** A referee grants it
  afresh every move and charges main time only with the excess, so it is free:
  an allowance ignoring it answers `btime 0 byoyomi 10000` instantly with ten
  unused seconds.

  ⚠️ **`btime` and `wtime` are read by side to move, and reading the wrong one
  is silent** — both sides start equal, so a match diverges plies later.
  `a_derived_allowance_reads_the_clock_of_the_side_to_move` pins it.

  **`DeliveryMargin` bounds the allowance, `allowance ≤ available − margin`,
  rather than reducing it** — a spend already under that cap is left alone.
  ⚠️ No floor: once the mover holds less than the margin the allowance is zero
  and the engine answers at once, and a minimum thinking time put back would
  re-create the overrun the margin exists to remove.

- **`DEFAULT_DEPTH = 4`** answers "no budget of any kind was named" and only
  that. Applying it as a ceiling over a stated budget as well was a real bug,
  and `the_depth_ceiling_follows_the_budget_that_was_named` pins it.

  ⚠️ **The search stopping on time is not the move arriving on time**, and the
  difference is a lost game rather than a lost tempo. The deadline is
  `search()`'s entry plus the allowance: everything before that instant — the
  server sending `go`, the pipe, the protocol thread parsing it, the channel
  handing it to the worker — and everything after it — up to two poll intervals
  of overshoot, then building and flushing `bestmove`, then the wire back —
  falls outside it. `DeliveryMargin` covers that gap where it can lose a game
  — ⚠️ **but not under `movetime`, which is exempt from it**, so a peer that
  states a per-move budget that way gets no margin at all,
  and it is an operator setting because its right value is a property of the
  deployment rather than of the engine: microseconds on localhost, a round trip
  over a network, where a byoyomi overrun is an immediate loss.

## The transposition table

- **A stored move is never played without checking it against the list the node
  generated.** A hit is a hit on a Zobrist key, so a 64-bit collision hands back
  a move belonging to another position, and shunsai's `do_move` validates
  nothing. Membership in the already-generated list *is* the check; `is_legal`
  is not the caller for this and is documented as not being one.
- **A hit may be returned in place of searching only when its score lands on or
  past a window edge.** ⚠️ **An `Exact` score strictly inside the window may
  not**, even though the bound would justify it: the node's line has been
  cleared and not refilled, so a parent that raises alpha on it publishes one
  move followed by nothing. The FAQ carries the measurement that could not
  demonstrate this end to end and why the restriction is kept regardless.
- **Nothing is stored from an abandoned search.** Once the budget is spent a
  frame returns a placeholder, and a placeholder in the table outlives the
  search that made it — every later search then reads it as a proved value.
- **The table survives a `go` and is cleared by `usinewgame`.** A position's
  value does not stop being true because the clock moved. What a new search does
  instead is *age* the table, so its own entries win replacement contests
  against the previous search's without erasing them.
- **`USI_Hash` is queued through the search FIFO, never acknowledged.** The
  worker drains one queue, so waiting for a resize would hang the protocol
  thread behind whatever search is in front of it — an `isready` during
  `go infinite` would never return.
- **Table size is an input to every node count.** A bigger table evicts less, so
  a count quoted without a size is not a result, and `bench` fixes its own size
  rather than reading `USI_Hash`.
- **The allocation is fallible, and a shortfall is reported.** `USI_Hash`
  advertises sizes most machines cannot give; an infallible `vec![_; len]`
  answers a refusal by aborting the process, from the worker thread, on a value
  the engine itself offered. A smaller table plus a diagnostic is the only
  answer compatible with "bad input never stops the loop".

## `bench`

- **It is a binary subcommand, not a criterion target**, and its counts are a
  regression test rather than a speed measurement.
- **Everything that could move a count is fixed inside it**: the position set
  (compiled in), the depth, the table size, and a `new_game` between positions
  so that position *n* is not searched against what 1..n−1 left behind.
- **The frozen counts live beside the code**, because a baseline has to be
  executable to be a regression test.
- Its position set is one of the frozen sets below, under the two rules that
  hold of a set nobody generated.

## Frozen position sets

`positions/bench-v1.sfen` is the one `bench` compiles in; the `openings-*`
files are the ones the SPRT harness opens from, and it defaults to the newest.
The first two rules below hold of every one of them. The rest are about
*generated* sets, which the opening sets are and `bench-v1.sfen`, assembled by
hand, is not.

- **A later set is a new file, never an edit.** `bench` counts and paired-game
  results are comparable only within one set, so editing a set in place
  silently invalidates every number already attributed to it — including the
  ones in FAQ answers that can no longer be re-run.
- **Every line carries its provenance**, which is where the licensing rule
  bites hardest: the header names the rev and seed the set was generated from,
  and each line's own comment names the game behind it. A hand-assembled set
  says where each position came from instead.
- **What shapes a generated set travels with the code, with two exceptions.**
  The filters, the balance search's depth, node cap and table size, the target
  and the seed are all `PipelineConfig` fields a frozen constructor fills, so
  an earlier set is regenerated from the checkout its header names — the tag
  `openings-vN-generated` is what resolves that rev, which is never on `main`
  here. ⚠️ **The source days are the one *setting* the code does not carry** —
  required, no default, and the reason the header records a date range beside
  the rev. The records the days select from are the other exception: not
  carried either, and not in git. (`--seed` can override the constructor's
  seed; no committed set has, and each one's header repeats its rev's
  default.) ⚠️ **The node cap
  belongs on the travelling list even though it reads like a budget rather
  than a rule** — it decides how deep each candidate is judged, so a set
  generated at another cap is a different set.
- **A generated set's balance score comes from an iteration that finished.**
  The last `info` line published may belong to an iteration the node cap
  interrupted, whose score is a lower bound. ⚠️ **A lower bound against a
  two-sided window is wrong in both directions**, so it is not the safe
  reading.

## The SPRT harness

- **A fixed-node run may not take more than one lap of the opening set**, and
  the harness refuses rather than warns. Under `--nodes` a repeated opening is a
  repeated *game* for engines that answer a node budget deterministically, and
  the replays are indistinguishable from independent pairs downstream —
  `sprt::tests::replication_multiplies_the_llr_it_should_not_move` is what that
  costs the statistic. ⚠️ **A clock is exempt**: elapsed time differs between
  replays, so a lapped opening there is a fresh game rather than a copy.
- **`--concurrency > 1` under a clock warns rather than refuses**, and the run
  is recorded as `noisy` in its log, so it cannot later be read as a
  measurement it was not.

## The `info` line

- **It carries `depth seldepth time nodes nps hashfull score pv`, and nothing
  else.** `multipv` and `currmove` still have no consumer. A field that cannot
  be filled honestly is not printed, and a field that can is printed
  **unconditionally**, including when it is uninteresting: a token that comes
  and goes makes the line's shape depend on the position, which is how a
  reader's bug becomes one that reproduces on some positions and not others.
- **`seldepth` resets per iteration; `nodes` accumulates across them.** The
  asymmetry is deliberate. USI prints `seldepth` beside `depth` and it means the
  selective depth *of that iteration*. `nodes` accumulates for an unrelated
  reason — resetting it starves the poll, above.

## Portability

- **Threads and the wall clock may not become the only way to run a search.**
  Both may be used, and are. What this forbids, each true of the code
  today:
  1. **A search must be reachable without `std::thread`.** `Searcher::search`
     takes a job and returns a `BestMove`; `SearchDriver` is a caller of it, not
     its interface. Threads may make the search stronger; they may not be what
     makes it work.
  2. **The engine always accepts at least one budget with no clock in it.**
     `depth` and `nodes` are that budget — `Budget::expired` reads the clock
     lazily, inside the deadline's `is_some_and`, so a budget carrying no
     deadline is never *polled* against one. ⚠️ That is a property of `Budget`
     and not of the call path: `Searcher::search` reads its clock at entry and
     once per `info` line whatever the budget is. It is also *not* the claim
     that every budget has a clock-free form: `movetime` and `byoyomi` are
     wall-clock by definition (see Time control above).
  3. **The search reads its clock through a `Clock`.** ⚠️ That does not yet
     make it clock-free: `RealClock::now` is `Instant::elapsed`, and
     `with_clock` is private, so no caller outside the crate can supply another
     implementation.

  ⚠️ **This is a constraint on what may become load-bearing, not a claim that
  the engine runs on `wasm32-unknown-unknown`. It does not** — it compiles and
  traps. The condition that retires the whole rule is in the FAQ.

## Tests

- **No test may name a move the engine chose.** shunsai's public documentation
  says nothing about generation order either way, so it is an unspecified
  implementation detail. Assert "this is legal here" by replaying the move into
  a position the test builds itself.
- **A sabotage note is only worth writing if the mutation was made and the test
  went red** — made at the site the note is attached to, not merely somewhere
  its subject appears — and it has to sit on the test it describes. ⚠️ **It may
  state what that run showed and no more.** "In either run", "in both loops",
  "every row but the first" are generalisations past the mutation actually
  applied, and each has shipped as a false note while satisfying the sentence
  above — the mutation was made, a test did go red, and the note then claimed
  a second site nobody touched. Name the site mutated and the test that fired;
  if a neighbouring site was not tried, try it or say so. ⚠️ **After a change,
  re-run the sabotages in the files the diff touches** — a note is trusted
  exactly because it was verified once, and a note that cannot fire is worse
  than none. Full-tree sweeps run at phase gates (E0 exit, E1 exit, before
  E3's first training run), not per change: a global rule is quadratic in
  project age and would strangle the E1 queue.
- **A test of an optimisation has to run on input the optimisation helps.**
  "It is the obvious fixture" is not evidence that it does: the transposition
  move is worth 14× on one position and 1.00× on the initial position, and the
  first version of its test used the second. Where a fixture is what makes a
  test able to fail, say so in its doc and — where the test can — assert the
  property the fixture was chosen for, so that the day it stops holding the test
  says so instead of going quiet.
- **A surface with no caller stays if — and only if — a *specific* caller can be
  named, and the name goes in its doc comment. Otherwise it goes.** "It'll
  probably be useful" is what this rule exists to prevent; a named caller is a
  record rather than a wish. The FAQ names the one exception.

### Fixture provenance

The shared fixtures come from shunsai (MIT, same author),
`benches/suite/common.rs`, where their perft values are cross-checked against
nine independent implementations. ⚠️ That path is in shunsai's **repository**:
the published crate ships `src/` only, so a provenance scan run against the
vendored dependency will not find it. Written down because step 3b builds a
committed bench position set, which is where the licensing rule bites hardest.

- **The drop-heavy middlegame ("matsuri")**
  `l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1` — a
  diagram from the shogi perft literature, i.e. a fact about a board rather than
  an expression. The same diagram appears in other engines' suites, GPL ones
  included; that is not reuse, because no code is copied.
- **The 593-legal-move position**
  `R8/2K1S1SSk/4B4/9/9/9/9/9/1L1L1L3 b RBGSNLP3g3n17p 1` — same file, same
  standing.
- **`the_first_root_move_is_never_abandoned`'s fixture** is two plies on from
  the matsuri position, reached by rinsai's own search.
- **The committed floodgate records** under `crates/xtask/tests/fixtures/` are
  game records — factual data — copied from the local cache
  `cargo run -p xtask -- fetch-floodgate` fills. There are two corpora and the
  second is deliberately a single game: `floodgate/` feeds the extractor's
  reproducibility gate, and `floodgate-capped/` exists because **no candidate
  in the first one separates the balance filter's two possible rules**, at any
  cap tried. Its one game was chosen for a position that does.
- **The mate-in-1..5 ladder and both repetition fixtures are our own
  construction**, built for these tests and verified by searching them. ⚠️ A
  composed 詰将棋 is not covered by the argument above: the argument is that a
  *position* is a fact about a board rather than an expression, and a problem
  set is exactly the case where that stops being true. So the suite constructs
  its own, the way the licensing rule requires tables and books to be
  generated rather than pasted.
