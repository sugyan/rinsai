# rinsai — frozen conventions

What the code already assumes, and **something is built on every line of it** —
that is the admission test. A rule for writing new code is not one of these and
is in [CLAUDE.md](./CLAUDE.md); the argument behind a rule is in
[FAQ.md](./FAQ.md); a rule the item's own doc comment already states is in
neither, because one copy or none.

Organised by subject. A rule here is stated in a few lines: if it takes a
paragraph, what is being written is the argument.

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
- **Three score bands, in this order: evaluations, then a repetition won or
  lost, then the mates**, asserted as one chain by
  `the_three_bands_do_not_overlap`. `clamp_to_eval` clamps below the *lowest* of
  the two upper bands, so clearing it clears both.

## Evaluation

- **A piece in hand is worth more than the same piece on the board**, by the
  widest margin for the kinds whose board placement is most constrained. The
  margins are the table in `eval`, which carries the rest of the derivation.
- The one known simplification: **成銀 is genuinely worse than 銀 sometimes** (a
  silver retreats diagonally, a promoted one does not) and this model says it is
  always 50 better. Left unmodelled deliberately; E3 replaces the whole table.

## Search

- **Node counting: one node per `search`/`qsearch` entry, including the root,
  excluding bulk-counted leaves.** Frozen because these numbers are a
  regression test, and a convention changed afterwards invalidates the whole
  committed set.
- **The budget is polled at the top of the interior node, at the top of every
  quiescence node, and once per deepening iteration — *not* per root move**,
  which would break the first-root-move guarantee. `POLL_INTERVAL_NODES` says
  why every increment must be seen by a poll.

  ⚠️ **`self.nodes` accumulating across iterations is not what makes the first
  root move complete** — that is `Budget::without_limits`, which stands on its
  own. Two separate properties of one counter; welding them with a "because"
  is a mistake this project made once already.

- **The search answers with a move it actually searched, unless `stop` cut the
  first root move short**, and the mechanism is `Budget::without_limits` rather
  than a rule anybody has to remember.

- **Each iteration re-seeds the root list with its own answer.** Root-only; not
  the interior move ordering E1 is about.

- **`MoveBuf::get` returns a `Move` by value, not a slice**, and that is what
  makes the recursion compile: a slice would borrow the buffer across the
  recursive `&mut self` call. **The root move list lives outside the buffer**,
  because it persists across iterations and gets reordered.

- **千日手 is decided where a child is dispatched, not at the top of the
  interior node**, and the verdict reaches the transposition table by way of
  the parent. `NegamaxSearcher::child` carries both and what they cost.

- **The repetition path is extended at interior nodes only, and quiescence is a
  deliberate hole in it**, narrow rather than closed. The argument and what it
  does not cover are on the `path` field.

- **A child gives the move buffer back exactly as it found it, asserted at the
  boundary rather than left to a test.** Forgetting a `truncate` is a leak, not
  a wrong answer, so no ordinary test sees it.

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
- **`USI_Hash` is queued through the search FIFO, never acknowledged.** The
  worker drains one queue, so waiting for a resize would hang the protocol
  thread behind whatever search is in front of it — an `isready` during
  `go infinite` would never return.
- **Table size is an input to every node count.** A bigger table evicts less, so
  a count quoted without a size is not a result, and `bench` fixes its own size
  rather than reading `USI_Hash`.

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

## The `info` line

- **It carries `depth seldepth time nodes nps hashfull score pv`, and nothing
  else.** `multipv` and `currmove` still have no consumer. A field that cannot
  be filled honestly is not printed, and a field that can is printed
  **unconditionally**, including when it is uninteresting: a token that comes
  and goes makes the line's shape depend on the position, which is how a
  reader's bug becomes one that reproduces on some positions and not others.
- **`seldepth` resets per iteration; `nodes` accumulates across them**, for two
  unrelated reasons the `seldepth` field carries.

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

## Fixture provenance

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
