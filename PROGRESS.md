# rinsai — progress ledger

Where the implementation is, what the next step is, and what a session needs to
know before touching the code. [DESIGN.md](./DESIGN.md) is the plan and
[CLAUDE.md](./CLAUDE.md) is the rules; **this file is the state**. Update it at
the end of every step.

## E0 — baseline: play legal moves and don't hang pieces

E0 is split into seven sub-steps, one per pull request — except step 3, which
became two when building it showed that quiescence alone changes three frozen
conventions and rebaselines every committed node count (DESIGN.md §9).

| # | Step | Status |
|---|---|---|
| 1 | Skeleton + USI shell — workspace, CI, shunsai wiring, protocol loop, `bestmove` = a legal move | **done** |
| 2 | Material evaluation + iterative-deepening negamax αβ — PV, `info` output, mate scoring | **done** |
| 3a | Quiescence search — captures only, `seldepth` | **done** |
| 3b | TT + transposition-move ordering — `hashfull`, `USI_Hash`, `rinsai bench` | next |
| 4 | Repetition (千日手) + 連続王手 — history *queries*, mate-in-1..5 suite | |
| 5 | Time management — byoyomi / Fischer / movetime, `stop` responsiveness | |
| 6 | `openings-v1` extractor — floodgate CSA → balanced opening SFENs (`crates/xtask` arrives here) | |
| 7 | Match harness + SPRT — Ayane vendored, `tools/opponents.toml.example` | |

### E0 exit criteria not yet met

- **`TODO(shunsai-0.1-release)`** — the dependency is a git rev, which DESIGN.md
  §2 and shunsai's own DESIGN.md both say it should not be. E0 cannot be
  declared finished with this outstanding. `git grep 'TODO(shunsai-0.1-release)'`
  finds all four places that change together.

## Input the engine must survive

Everything below reached the engine from stdin and is now rejected or absorbed,
each with a regression test. They are listed because the next protocol surface —
the CSA client at E2 — has to survive the same shapes.

- **A hand count no shogi set contains** (`position sfen … b 19P 1`). It parses:
  `shogi_usi_parser` accepts a two-digit count and `Hand::added` has no cap.
  shunsai then indexes a `[u64; 19]` Zobrist table by held count and the
  nineteenth piece is an out-of-bounds panic **on the protocol thread**, so one
  line from a GUI or a server ended the process. `Game::from_partial` now bounds
  the total per kind, which also closes the indirect route (18 pawns in hand plus
  one on the board, then capture it).
- **A non-UTF-8 byte** (a CP932 path from a Japanese Windows GUI — and E3 is
  going to ask for an `EvalFile` path). `BufRead::lines` reports it as
  `InvalidData`, which the loop could not tell from a real I/O error, so the
  engine exited silently with status 0. Lines are now read as bytes and
  converted lossily, with a diagnostic.
- **An out-of-range SFEN move number.** `PartialPosition::from_usi` saturates and
  discards — `0` becomes 1, 65536+ becomes 65535 — while rejecting a
  *non-numeric* field. Now validated in `split_root` instead.
- **Two kings of one colour** (`position sfen 4k4/…/3KK4 b - 1`). The piece-count
  bound was per *set* — two kings total — which admits two black kings and no
  white one, a position no set can produce. The king is the one kind that never
  changes sides, so its bound is **per colour**, and it is *at most* one rather
  than exactly one: a 詰将棋 diagram routinely omits the attacking king, and
  shunsai's `king_square` returns `Option` with `None` documented as legal.

### Still unbounded: the length of one line

`usi::run` reads with `read_until(b'\n')` and no cap, so a peer that never sends
a newline grows the buffer until the process dies. Harmless at E0 — the peer is
a local GUI — but **E2's CSA client takes its input from a network socket**, and
that is the phase to bound it in. Recorded here rather than fixed now for the
same reason as the move buffer: there is no consumer yet, and the bound wants to
be chosen against the real longest line a CSA server sends, not guessed.

## What step 1 delivered

`rinsai` starts, holds a USI conversation, and plays legal games. Verified
against the local material-evaluation YaneuraOu (`../benchmarks`, `NodesLimit
10000`): 22 plies to a resignation, no illegal move, no protocol stall, no
diagnostics. It is extremely weak on purpose — there is no search.

- `crates/rinsai-search` — `Game` (board + lockstep SFEN + history), `is_legal`,
  `Score`/`Depth`, the `Searcher` seam (`SearchJob`, `Limits`, `SearchSignals`,
  `BestMove`, `InfoSink`, `SearchDriver`), and `PlaceholderSearcher` (deleted at
  step 2, as planned).
- `crates/rinsai` — the USI protocol loop, its state machine, the option table
  and the single output sink.
- CI: fmt, clippy `-D warnings`, tests, `cargo doc` with `-D warnings` (the doc
  comments carry the design record and link into it; a link that stops resolving
  is drift `cargo test` cannot see), an MSRV job on 1.88, `cargo deny check
  advisories licenses bans sources`, and two CI guards, both described under
  Traps below.

**Deliberately not built**, so step 2 does not inherit guesses: no evaluation,
no search, no move buffer, no transposition table, no repetition *queries* (the
history is recorded, nothing reads it), no time management, no `bench`.

## What step 2 delivered

`rinsai` searches. Iterative-deepening negamax with alpha-beta, material
evaluation, a principal variation, `info` lines and mate scoring;
`PlaceholderSearcher` is gone.

- `crates/rinsai-search/src/eval.rs` — the value tables and `evaluate`, plus a
  `#[cfg(test)]` square-scan oracle it is differentially tested against.
- `crates/rinsai-search/src/negamax.rs` — `NegamaxSearcher`, `Window`, `Budget`,
  the deepening loop, the root and the interior node.
- `crates/rinsai-search/src/info.rs` — `SearchInfo` and its USI `Display`,
  testable without running a search.
- `crates/rinsai-search/src/moves.rs` — `MoveBuf`, the shape decided in step 1.
- `Game::search_board()` — the board a searcher does its do/undo on.

**The step-1 seam held.** `Searcher::search` takes `&SearchJob`, `Game` hands
out no `&mut Position`, and neither had to change: the searcher asks `Game` for
a board of its own. Step 1's prediction was that only the lines *naming* the
searcher would **need** to change, and that is what happened — six of them,
every one naming it or pointing at where it lives:

| File | Lines forced | What they are |
|---|---|---|
| `src/main.rs` | 2 | the `use`, and the `usi::run` call |
| `tests/usi_conformance.rs` | 4 | the `use`, `dialogue`, its doc line, the module doc's pointer |
| `tests/usi_process.rs` | 0 | — |

Every pre-existing conformance and process test passed unmodified.

**Not the same claim as "nothing else in the crate changed", which is false**:
step 2 also added a helper and three tests to `usi_conformance.rs`, about eighty
lines. Nothing forced those — they are new assertions about a search that did
not exist before — so they say nothing either way about the seam. Keeping the
two apart is the whole content of the result: a seam is verified by what a
change *compelled*, not by a diffstat. (The looser wording shipped in this
branch's first draft and was caught in review.)

Measured, release build. Not a strength claim — E0 has no instrument for that
(see below) — just what the thing does. **This table is the only home for these
numbers**; nothing in the source repeats them, because the first draft of step 2
put a timing in a doc comment and the two copies had already drifted apart by a
factor of two when the branch was reviewed.

| Position | Depth | Nodes, step 2 | Nodes, step 3a | seldepth | Time, step 2 |
|---|---|---|---|---|---|
| initial | 6 | 585 568 | **376 695** | 29 | 23 ms |
| drop-heavy middlegame | 4 | 838 976 | **7 030 029** | 29 | 40 ms |
| drop-heavy middlegame | 5 | 13 888 371 | **378 188 510** | 32 | 445 ms |
| two lone kings | 6 | 1 431 | **1 431** | 6 | — |

Step 3a's counts were taken three times and reproduce to the digit. **Its times
are not recorded, and that is deliberate**: the machine was not quiet — a
criterion run belonging to another checkout held a core for the whole session —
so the figures observed (about 45 ms, 770 ms and 36.8 s) are what the engine did
under load and are not results. Re-measure them on a quiet machine before
anything depends on them. The step-2 column stands; it was re-run untouched at
the start of this step and reproduced to the digit, which is the check that says
the two columns can be compared at all.

Three things in that table are worth reading rather than skimming:

- **The initial position got *cheaper*, by 36%.** Quiescence adds nodes at the
  leaves and removes far more above them, because a leaf that has resolved its
  captures returns a score alpha-beta can actually cut on. This is the direction
  nobody predicts and it is the ordinary one.
- **The drop-heavy middlegame got 8.4× and then 27× more expensive**, and that is
  the honest cost of a quiescence search with **no ordering, no SEE and no delta
  pruning** — E1 items 2, 8 and 9, in that order, each with its own SPRT. This
  row is their baseline. Depth 5 at 378 M nodes is not a search anyone should run
  under a clock; `DEFAULT_DEPTH` is 4 and every clocked search is bounded by its
  budget, so nothing in the engine reaches it by accident.
- **Two lone kings did not move at all.** A capture-only quiescence adds exactly
  nothing in a position where no capture is reachable, so all six iterations are
  byte-identical to step 2's. That is the cheapest possible check that the new
  node type is only entered where it should be, and it is why the fixture is now
  in the node-counting test.

**The two columns are not the same kind of fact, and the difference matters from
step 3b on.** The node counts are deterministic: every run reproduces all of them
to the digit, which is why they are what `bench` freezes as a regression test. The
times are one machine on one day — settled, the three rows sit at 22–23 ms,
38–41 ms and 444–469 ms across two sessions — so a time that differs from this
table is not by itself a result. Note that the spread is not proportional: the
short rows hold to about ±2% and the long one to about ±3%, so a threshold read
off the short rows would be too tight for the long one. That is the whole reason
a quiet machine is a precondition for the SPRT loop (§8): noise cannot move a
node count and can move a time far enough to invent an improvement. The node
rate these imply is used once, under "the poll interval is 1024 nodes" below.

⚠️ **Exec the binary once after a build, before measuring anything.** The first
run of a freshly linked binary costs about 20–25 ms more than the settled
figure, and that is an *absolute* amount rather than a proportional one, so it
is +87% on the 23 ms row and +5% on the 445 ms one.

Two experiments pin it down, and each rules out an explanation the numbers
otherwise fit:

- **Reverse the order of the three rows after a rebuild** and the penalty moves
  to whichever row now goes first — not to whichever row is shortest. So it is
  not "short measurements are noisy".
- **Run `rinsai --version` once after the build**, then measure. The penalty
  disappears: 40 / 33 / 38 ms without it against 23 / 23 / 24 ms with it, three
  alternating pairs. A process that prints one line and exits cannot warm a CPU
  up or warm the search up, so it is neither of those either — and the earlier
  wording here said "the CPU coming up to clock", which this is the experiment
  that removes.

What is left is the first *execution* of a newly written binary image: paged in
from disk, and — **a candidate, not a finding, because it has not been
instrumented** — on macOS the first-exec code-signature validation of a
just-linked binary, which the kernel caches per file. Either way it is paid once
per build rather than once per process, which is why the second and third
processes of a batch pay nothing.

The rule is written as "exec it once" rather than "discard the first
measurement" because a throwaway `--version` costs nothing and can go in a
script, where discarding costs a whole measurement. The reason to write it down
at all rather than shrug: the E0 measurements that matter are tens of
milliseconds long, so this one effect is larger than most of what step 5's
time-management work will be looking for.

**The depth-parity fingerprint is gone, and that is step 3a's headline result.**

Step 2 recorded it as a prediction to be falsified: from the initial position,
depths 3 and up reported +215 at odd depths and 0 at even ones. 215 is a pawn's
board value plus a pawn's hand value — after `7g7f`, every White reply left Black
a free pawn on ply 3, and at depth 4 White recaptured so the score fell back to
0. Re-measured before touching anything, it reproduced exactly, and one row
beyond what step 2 had recorded: depths 1–8 gave 0, 0, +215, 0, +215, 0, +215,
**−215**. The last is the same effect seen from the other side and step 2 never
went deep enough to notice it.

After quiescence, **depths 1 through 8 all report 0**. The initial position is
symmetric and the engine now says so at every depth. `the_horizon_effect_fingerprint_is_gone`
asserts it against the recorded magnitude rather than against "the score is
small", so a search that broke in some new way and reported ±90 cannot pass it by
being differently wrong.

And it holds a whole game: 70 plies against the local material-only YaneuraOu
(`NodesLimit 10000` against `go byoyomi 200`), ending in rinsai being mated and
answering `resign`. No illegal move, no protocol stall, nothing on stderr. Step
1's game ended at 22 plies, and **the comparison means nothing** — different
time controls, one game each, and E0 has no instrument for strength. It is
recorded as "it plays", which is all it is.

## What step 3a delivered

Quiescence search, captures only, plus `seldepth` on the `info` line. The
measurement table above is the result; what follows is what building it changed
or found out.

### Quiescence searches captures, and the shape of the node

At a leaf, instead of evaluating, the search plays out the captures. In check it
generates everything — shunsai restricts generation to evasions there — and does
**not** stand pat, because declining a check is not a legal option and a score
that assumes otherwise describes a position that does not exist. Otherwise it
stands pat first (fail-soft, so a stand-pat above β returns immediately) and then
searches only moves whose destination holds an enemy piece.

Captures are selected by **intersecting two bitboards per `MoveSet`**, not by
`piece_at` per generated move. shunsai hands destinations over as `Bitboard`s, so
a `Move` is only ever constructed for a capture, and `MoveSet::Drop` is discarded
whole — a drop can never capture, and drops are most of what a shogi move list
is. Where promotion is optional a capture is emitted **both ways**; both are
legal and choosing between them is ordering, which is E1's.

**Deliberately absent, each with the step that owns it**: non-capture promotions
(E1, beside SEE), checks (E1 item 7 — they need `gives_check`, which shunsai does
not have), SEE (E1 item 8 — needs `attackers_to`), delta pruning (E1 item 9).
The と金作り argument E1 inherits, with its number: in rinsai's own table a pawn
is 100 and a promoted pawn 600, so 歩→と is a **500 cp event** that a capture-only
quiescence is blind to. That is the largest single thing this design gives up,
it is shogi-specific rather than inherited from chess practice, and E0 has no
instrument to decide it — which is exactly why it goes to the phase that does.

### ⚠️ Counting *checks*, not plies — the one design decision that was got wrong first

The first implementation capped total quiescence depth (`QS_MAX_PLIES = 32`).
It was wrong, and the numbers say so plainly. Measured on the drop-heavy fixture
at depth 4, with the total-ply cap:

| cap | nodes, depth 4 | nodes, depth 5 |
|---|---|---|
| 32 | 8 138 236 | 1 485 819 624 |
| 16 | 7 416 958 | — |
| 8 | 4 735 764 | — |
| 4 | 1 870 111 | — |

The depth-5 run took over two minutes and hit the floor **24.4 million times**;
at cap 4 the floor fired on 22% of quiescence nodes. That last figure is the tell:
a cap low enough to cut the cost is cutting *capture chains* off in the middle,
which is the horizon effect reintroduced two plies further down — the search
paying full price for quiescence and getting a worse version of the thing it
replaced.

**The two kinds of quiescence ply are not alike.** A capture chain is
self-limiting: every capture moves a piece off the board and into a hand,
quiescence plays no drops, so occupancy strictly decreases and no position
affords more than about forty in a row. A check-evasion chain has no such
argument — an evasion may give check back, an evasion need not be a capture, and
an evasion list is *every* legal move including drops, so the node is an order of
magnitude wider. So the counter counts checks. `QS_MAX_CHECK_PLIES` bounds how
many times one quiescence line may be checked; capture chains run to exhaustion,
which is what they are for.

Choosing the value, measured (drop-heavy depth 4 / initial depth 6 / 頭金 at
depth 1):

| checks allowed | nodes, depth 4 | nodes, depth 6 | mate in 1 at depth 1 |
|---|---|---|---|
| 0 | 30 824 360 | 221 351 | **`cp 1260` — wrong** |
| 1 | 22 131 829 | 239 094 | `mate 1` |
| **2** | **7 030 029** | **376 695** | `mate 1` |
| 4 | 7 681 868 | 459 396 | `mate 1` |

Two results, and neither was predicted:

- **Zero is not a cheap approximation, it is incorrect.** It evaluates while in
  check, so the engine misses a mate one ply away and reports a material score
  instead. One is the correctness floor, and `quiescence_does_not_stand_pat_in_check`
  is what says so.
- **The node count is not monotone in the cap.** Allowing two checked plies costs
  a third of what allowing one costs. Resolving a check properly returns a score
  the search above can cut on; refusing to resolve it returns a number that
  defeats pruning for the rest of the tree. "Search less, spend less" is not a
  safe assumption anywhere near a check.

Two is the measured minimum and it is correct. Revisit at E1, where the check
extension and SEE arrive together and there is an SPRT to decide it with.

### The cost this leaves on the table for E1's staged generation

Every quiescence node runs shunsai's **full legal generation** and keeps a
handful of moves, because there is no captures-only generator (DESIGN.md §6 puts
staged generation at E1). Measured over a whole search, moves generated against
moves kept:

| Position | Depth | Quiescence share of nodes | Generated | Kept | Ratio |
|---|---|---|---|---|---|
| initial | 6 | 91.5% | 18 655 208 | 385 110 | **48×** |
| drop-heavy middlegame | 4 | 99.3% | 788 675 461 | 9 216 400 | **86×** |

That is the size of the prize DESIGN.md §6 assigns to staged generation, as a
number rather than a prediction, and it is the caller that finally justifies it.
The instrumentation that produced it was throwaway and is not in the tree — it
had no caller once the number existed.

A second, smaller API want fell out of the same place: **every quiescence node
calls `in_check()`**, which recomputes `attackers_to(king)` that `generate_moves`
computes internally one line later and does not expose. That is shunsai's own
parked "expose rather than recompute" (its log, 2026-07-29), and step 3a is the
consumer it was waiting for.

### It still plays

84 plies against the local material-only YaneuraOu (`NodesLimit 10000` against
`go byoyomi 200` both ways), ending in rinsai being mated and answering
`resign`. No illegal move, no protocol stall, nothing on stderr. Step 2's game
ran 70 plies and step 1's 22, and **the comparison still means nothing** — one
game each, different opponents' settings, and E0 has no instrument for strength.
Recorded as "it plays", which is all it is.

## Step 3b — what to do next

TT + transposition-move ordering + `rinsai bench`. Carried in unchanged from
step 2's list:

1. **The transposition move must be tried first, in the same PR as the table.**
   A table whose move nobody tries first is half a table. MVV-LVA, killers and
   history stay at E1: those are interior-node heuristics that each want their
   own SPRT.
2. **`bench` freezes node counts.** It is now safe to freeze them — quiescence
   has landed and the counts above are the post-quiescence ones — but the table
   will move once more when the transposition table does, so `bench` belongs
   last in the branch, after both.

Also waiting: `hashfull` on the `info` line, honouring `USI_Hash` (whose
`planned` disclosure is a promise now one step overdue), and whether
`Game::clone`'s Zobrist cross-check should stop being a `debug_assert`.

Findings from step 3a's planning that 3b should not re-derive:

- **`Option<CompactMove>` is 2 bytes, guaranteed.** `shogi_core::CompactMove` is
  `#[repr(transparent)] NonZeroU16` and the `Move ⇄ CompactMove` round trip is
  total and exhaustively tested upstream. `Move` itself carries **no `#[repr]`**,
  so its size is unspecified and unstable across compiler versions — the same
  rule `moves.rs` already wrote for the move buffer's reservation. Store the
  compact form.
- **Validate the transposition move by list membership, not `is_legal`.** With no
  staged generation an interior node generates its whole list anyway, so scanning
  it and swapping the move to the front *is* the legality check, and costs one
  pass instead of a second `generate_moves` walk. ⚠️ Consequence: `is_legal`'s
  doc names a second caller "from E0 step 3" that **does not arrive**, and 3b
  owes that doc a correction rather than leaving a promise standing.
- **An Exact-bound hit may cut only when its score falls outside the window.** An
  in-window Exact cut returns after `pv[ply].clear()` and before the move loop,
  so the parent raises alpha, does not cut, and publishes a truncated line.
- **Route `USI_Hash` through the existing FIFO** (`Command::SetHash`, mirroring
  `Command::NewGame`), never a blocking acknowledgement — the worker is one
  queue and `isready` during `go infinite` would hang the protocol.
- **The test suite must not allocate the advertised 256 MB per searcher.**
  `dialogue()` builds around twenty-five of them and `negamax.rs`'s helpers a
  dozen more.
- **`shogi_core`'s `hash` and `ord` features are off in this graph**, so `Hand`
  is not `Hash` and `Move` is not `Ord`. Step 4 will meet that first.

## Conventions frozen in step 1

Changing any of these needs a DESIGN.md §9 entry, because code already assumes
them.

- **A rationale has one home, and it is this file.** Step 1 shipped several
  arguments copied into four or five places at once — why USI drop notation
  needs a colour rewrite, why `shogi_core::Position::from_usi` is not used for
  the `moves` list, why no shunsai feature may be enabled — each restated in the
  implementation, in a test doc, in a second module, and here. One of them then
  went stale exactly as that predicts: `parse_line`'s doc still described
  `BufRead::lines` stripping the CRLF after the loop had moved to `read_until`,
  because the fix updated the copy next to the code and not the copy next to the
  parser. **So: the argument lives in PROGRESS.md or DESIGN.md §9; the code
  carries the conclusion and, where it earns it, a pointer.** A sabotage note is
  the deliberate exception — it has to sit on the test it describes to be worth
  anything.

- **A bare `Position` means `shunsai::Position`** — the board a search walks,
  and the only one of the two with unmake and an incremental Zobrist key.
  `shogi_core::Position` is a *record* (root, current position, moves played) and
  `Game` delegates that half to it under the alias `Record`; it is never
  imported unqualified. Its `from_usi` is not used at all, for the reason under
  "Traps" below — **do not confuse "we cannot parse with it" with "we cannot
  store in it"**, which is the mistake the first draft of `Game` made.
- **Evaluation is negamax from the side to move.** Positive is good for whoever
  is to move; a parent takes `-child`. At the root the side to move is the
  engine, so USI's `score cp` needs no flip.
- **Centipawns, pawn = 100.** E3's network emits its own scale, and the
  conversion at the inference boundary is exactly where silent strength drift
  lives.
- **`Depth` is signed whole plies.** No fractional `ONE_PLY` scheme. Signed
  because quiescence runs at negative depth from step 3a — which it does, counting
  down from zero towards `QS_MAX_CHECK_PLIES`.
- **`MAX_PLY = 128`**, sizing the search stack, the mate band and (from step 2)
  the move buffer together.
- **Node counting: one node per `search`/`qsearch` entry, including the root,
  excluding bulk-counted leaves.** Fixed now because step 3b freezes those
  numbers as a regression test, and a convention changed afterwards invalidates
  the whole committed set. shunsai's own HEAD commit is *"Measure the leaf
  convention the cross-engine table had been mixing"* — the precedent is one
  repository over.
- **No test may name a move the engine chose.** shunsai's public documentation
  says nothing about generation order either way, so it is an unspecified
  implementation detail. Assert "this is legal here" by replaying the move into
  a position the test builds itself. (An earlier draft of this file cited
  "shunsai DESIGN.md:64" for an explicit non-guarantee; **no such statement
  exists** — the citation was carried over from a survey and never checked.)

## Conventions frozen in step 2

- **Piece values are a starting point for SPSA at E4, not a measurement.** The
  ordering comes from the rules of shogi — empty-board destination counts,
  slider or stepper, colour-bound or not, promotion potential — the magnitudes
  are interpolated on that ordering, and everything is rounded to a multiple of
  five so that nobody reads them as fitted. Pawn = 100 anchors it. Two
  modelling choices worth naming: a **promoted minor is exactly a gold on the
  board**, because it moves exactly as a gold and anything else would be a claim
  about the position rather than the material; and **a piece in hand is worth
  10–15% more than the same piece on the board**, most for the kinds whose board
  placement is most constrained. "と金は金以上" then falls out on its own —
  taking a と gains 600 + 115, taking a gold gains 600 + 660. The one known
  simplification: 成銀 is genuinely worse than 銀 sometimes (a silver retreats
  diagonally, a promoted one does not) and this model says it is always 50
  better. Left unmodelled deliberately; E3 replaces the whole table anyway.

- **`movetime` and `byoyomi` are honoured; `btime`/`wtime`/`binc`/`winc` are
  ignored.** The line is *a budget the engine was told* versus *a budget it
  would have to decide*. `movetime n` means "spend n ms" and has no judgement in
  it; turning a remaining clock into a per-move allowance — with the fail-low
  extension, the early cut-off and the delay margin that go with it — is step
  5's whole subject and is not approximated. `byoyomi 0` means "this time
  control has no byoyomi", not a zero-millisecond budget.

  ⚠️ **"Honoured" means the search stops on time, not that the move arrives on
  time**, and the difference is a lost game rather than a lost tempo. The
  deadline is `search()`'s entry plus the stated budget exactly: everything
  before that instant — the server sending `go`, the pipe, the protocol thread
  parsing it, the channel handing it to the worker — and everything after it —
  up to two poll intervals of overshoot, then building and flushing `bestmove`,
  then the wire back — falls outside the budget. Locally that is microseconds
  and the step-2 game at `go byoyomi 200` never noticed. Over a network it is
  not, and a byoyomi overrun is an immediate loss. The margin that covers this
  is named in step 5's list above and stays there; **until step 5, do not enter
  rinsai anywhere the clock is enforced across a network — floodgate included.**
  That route does not open until E2 gives it a CSA client, so this is written
  forward rather than describing anything reachable today; it is here because
  here is where someone about to open it would be reading.

- **`DEFAULT_DEPTH = 4`.** It answers "no budget of any kind was named" and only
  that — applying it as a ceiling over a stated budget as well was a real bug in
  this step, caught by driving the release binary rather than by any test, and
  `the_depth_ceiling_follows_the_budget_that_was_named` now pins it.
  ~~It must stay even~~ — **retired at step 3a** (DESIGN.md §9). The evenness
  rule existed because an odd depth ended the line on a capture of ours nobody
  answers, which is what quiescence removes; the assertion went with its reason
  rather than being left to pin a rule nobody could still justify. The value
  stays at four.

- **The poll interval is 1024 nodes**, checked at the top of the interior node,
  **at the top of every quiescence node from step 3a**, and once per deepening
  iteration — *not* per root move, which would break the guarantee below. Step
  2 measured roughly 20–30 M nodes/s because most nodes were depth-zero leaves
  that only evaluated; that reason is gone and the rate with it (the step-3a
  figures are not a measurement — see the table's note about the machine).
  Step 5's SPRT is where the number stops being inherited.

  ⚠️ **Quiescence has to poll, and the reason is not tidiness.** The test is
  `nodes.is_multiple_of(1024)` — an *exact* multiple — which is safe only while
  every increment is seen by something that polls. A quiescence search that
  counted nodes without polling would leave the values seen at interior-node
  entries non-consecutive, and they could then step clean over every multiple of
  the interval: `stop`, the deadline and the node limit all missed, and missed
  unpredictably rather than reliably. The mutation was made; the test that goes
  red is `a_deep_search_still_answers_a_node_limit`.

  ⚠️ **`self.nodes` accumulates across iterations and must not be reset per
  iteration**, or the poll starves in a sparse position. Measured on two lone
  kings (`4k4/9/9/9/9/9/9/9/4K4 b - 1`, about five legal moves a side): the
  nodes spent *within* each iteration for depths 1–6 are 6, 15, 53, 120, 386,
  851 — every one under the interval — against cumulative 6, 21, 74, 194, 580,
  1 431. Reset per iteration and the poll does not fire once through depth 6, so
  `stop` and any deadline go unnoticed for that whole stretch; accumulating, it
  first fires partway through depth 6. This is **not** what makes depth 1
  complete — that is the ≤ 594-node bound below, which stands on its own, since
  `self.nodes` is 0 when depth 1 starts either way. Two separate properties of
  one counter, and welding them together with a "because" is a mistake this file
  made once already (DESIGN.md §9, 2026-08-07).

- **The search always answers with a move it actually searched.** Restated at
  step 3a (DESIGN.md §9), because quiescence removed the proof that used to carry
  it. Step 2's version was arithmetic: the poll only fires on an exact multiple
  of 1024 and a depth-1 iteration is `1 + N ≤ 594` nodes, so it could not fire.
  With quiescence a depth-1 iteration is `1 + N` plus whatever the quiescence
  trees under those N children cost, and that has no tight bound.

  What replaced it is structural: **the first iteration runs against
  `Budget::without_limits`**, the same budget with the clock and the node limit
  suspended. Without it, a poll landing inside the first root move's subtree
  leaves `negamax_root` returning `None`, the deepening loop breaking, and the
  answer sitting at the unsearched seed — a move in shunsai's unspecified
  generation order, with no `info` line emitted at all. ⚠️ **`signals.stopped()`
  stays live** through it, which is why it is a second budget rather than a flag
  that skips the poll: `stop` means quit, and the poll is where it is read.

  The failure is real but rare, and finding a fixture that shows it took work
  worth recording. Depth-1 costs measured: initial position 31 nodes, drop-heavy
  middlegame 280, an open middlegame 422, the 593-move position 634 — every one
  of them below the poll interval, so none can fail. Searching the drop-heavy
  fixture's descendants for the most expensive depth-1 iteration turned up one at
  **49 006 nodes**, and that is the fixture
  `the_first_iteration_is_never_abandoned` uses. On it, removing
  `without_limits` gives zero `info` lines and an unsearched answer; on the other
  four it changes nothing. A test written against any of the first four would
  have been a test that could not fail.

- **Each iteration re-seeds the root list with its own answer.** Without it,
  deepening is only repeated work: an iteration cut short reports the best of
  whatever prefix of the root list it reached, which can be a *worse* move than
  the last completed iteration chose — measured at 1 590 cp worse in the
  middlegame fixture before the three lines existed. This is root-only and is
  not the interior move ordering E1 is about.

- **The `info` line carries `depth seldepth time nodes nps score pv`, and nothing
  else.** `seldepth` joined at step 3a, in the slot immediately after `depth`,
  because quiescence is what gave it a meaning — it was a constant equal to
  `depth` before. `hashfull` joins before `score` at step 3b; `multipv` and
  `currmove` still have no consumer. A field that cannot be filled honestly is
  not printed, and a field that can is printed **unconditionally**, including
  when it is uninteresting: a token that comes and goes makes the line's shape
  depend on the position.

- **`seldepth` resets per iteration; `nodes` accumulates across them.** The
  asymmetry is deliberate and is worth stating so it does not read as an
  oversight. USI prints `seldepth` beside `depth` and it means the selective
  depth *of that iteration*, so it is measured from zero each time rather than
  seeded from `depth`. `nodes` accumulates for an unrelated reason — resetting it
  starves the poll in a sparse position, measured above.

- **`MoveBuf::get` returns a `Move` by value, not a slice**, and that is what
  makes the recursion compile: a slice would borrow the buffer across the
  recursive `&mut self` call. **The root move list lives outside the buffer**,
  because it persists across iterations and gets reordered, which is not what a
  ply-threaded buffer is for.

- **A surface with no caller stays if — and only if — its caller can be named.**
  This replaces the rule the step-1 exception wrote for itself ("if step 2 does
  not reach for it, delete it"). See the section below.

## Conventions frozen in step 3a

- **A child gives the move buffer back exactly as it found it, and that is
  asserted at the boundary rather than left to a test.** Forgetting a `truncate`
  is a leak, not a wrong answer — every ply reads its own base, so the search
  still plays correctly — and step 2 recorded that
  `negamax::tests::the_move_buffer_comes_back_empty` was the test that caught it.
  It is not, once there are two node types: an interior node's own `truncate`
  restores the buffer on the way out and destroys the evidence of anything its
  quiescence children left behind. **The mutation was made — quiescence's
  `truncate` deleted outright — and the whole suite stayed green.** So the
  invariant is now a `debug_assert_eq!` on either side of every child call, and
  the same mutation now fails twenty-one tests. This is the shape step 1's audit
  named: a note that cannot fire is worse than none, because it is trusted.

- **Every inherited measurement in a test doc was re-run, and one of them had
  become false.** `the_reported_pv_is_playable` searches at depth 5 because step
  2 *measured* that reversing `update_pv` at depth 3 left it green — a
  three-move line read backwards still replayed. With quiescence that is no
  longer true: the reported line runs well past the nominal depth, so depth 3
  now fails too. The depth is kept and the doc says why it is no longer
  load-bearing. Also re-measured: the 300 000-node budget in
  `a_stated_budget_deepens_past_the_default` reaches **depth 6**, so its
  `> DEFAULT_DEPTH` assertion is not on a knife edge.

- **The drop-heavy middlegame fixture has a provenance, and it is written down
  here because it had not been.** `l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1`
  is the "matsuri" position, taken from shunsai (MIT, same author),
  `benches/suite/common.rs`, where its perft values are cross-checked against
  nine independent implementations. It is a diagram from the shogi perft
  literature — a fact about a board, not an expression — and the same diagram
  appears in other engines' suites, GPL ones included; that is not reuse, because
  no code is copied. rinsai has used it in four test files since step 2 with no
  note at all, and step 3b builds a committed bench position set, which is where
  the licensing rule bites hardest. The 593-legal-move position
  `R8/2K1S1SSk/4B4/9/9/9/9/9/1L1L1L3 b RBGSNLP3g3n17p 1` comes from the same file
  with the same standing. `the_first_iteration_is_never_abandoned`'s fixture is
  two plies on from the matsuri position, reached by rinsai's own search.

## The shunsai constraint sheet

Surveyed against `shunsai` @ `e58c16f`, `shogi_core` 0.1.5, `shogi_usi_parser`
0.1.0. **Read this instead of re-surveying.**

### What exists

- The entire public surface is four `pub use` lines (`src/lib.rs:13-16`):
  `Bitboard`, `MoveSet`/`MoveSetIter`, `Position`, and `pub use shogi_core`.
- `Position`: `new(PartialPosition)`, `startpos()`, `piece_at`, `side_to_move`,
  `ply` (u16, starts at 1, wrapping), `hand(Color)`, `king_square` (→ `Option`,
  `None` is legal), `key()`, `occupied()`, `player_bb(Color)`,
  `piece_kind_bb(PieceKind)` (**both colours** — AND with `player_bb`),
  `do_move`, `undo_move`.
- `generate_moves(&self, impl FnMut(MoveSet) -> ControlFlow<()>)`,
  `legal_moves() -> Vec<Move>`, `has_legal_moves()`, `in_check()`.
- `Bitboard` **is** the `Iterator` (`Item = Square`) and is `Copy`, so
  `for sq in bb` works without consuming the original. Gotcha: `bb.count()` is
  the inherent `u32` popcount, `bb.len()` is `ExactSizeIterator`'s `usize`.

### What does not exist

`attackers_to`, `checkers`, `pinned`, `gives_check`, `do_null_move`, SEE, staged
generation, any public attack table, `is_legal`, SFEN in or out, and any game
history. All of the first group are scheduled for **E1** and each arrives as a
shunsai release (DESIGN.md §6). **E0 needs none of them** — that is the layering
test, and step 1 confirmed it.

### Traps

- **`generate_moves` takes `&self`**, so nothing can `do_move` inside the
  callback. Anything recursive must collect first.
- **`MoveSet::{promotions, non_promotions}` overlap** where promotion is
  optional. A square in `promotions` only is a *compulsory* promotion.
  `MoveSet::Drop.piece` carries the colour, which is what makes `is_legal`
  reject a wrong-colour drop.
- **`do_move` validates nothing** — its documented `expect`s are reachable from
  a bad move. `Game::push_move` gates every move with `is_legal`, which is what
  keeps them unreachable from anything a GUI or server can send.
- **`Position::clone()` deep-copies the undo stack.** `Game`'s `Clone` rebuilds
  from the lockstep `PartialPosition` instead — cheaper, gives the search an
  empty stack, and cross-checks the incremental key for free.
- **USI drop notation carries no colour.** `shogi_usi_parser`'s `Move::from_usi`
  hard-codes every drop to Black (`mv.rs:5-7`); the colour is rewritten from the
  side to move in `Game::push_usi_move`.
- **`shogi_core::Position::from_usi` must not be used for the `moves` list** —
  but the reason is narrower than "it never errors", which is false: a
  *malformed* token does produce `Err`. What it does instead is (a) **silently
  drop** a well-formed move that cannot be made — nothing on the from square,
  wrong side to move — and report success, and (b) **apply** a move that is
  structurally fine but illegal, 二歩 and moving into check included, because
  `make_move` documents that it never checks legality. Measured, not read:
  `crates/rinsai-search/tests/shogi_core_from_usi.rs`, which fails if
  `shogi_core` ever changes so the decision gets revisited. Its **SFEN prefix**
  half (`PartialPosition::from_usi`) is sound and *is* used.
- **Never enable a shunsai feature.** `slider-naive` wins its backend priority
  order and is 5–8× slower; `bench-internals` is documented "never enable as a
  dependency". CI checks the **resolved graph**
  (`cargo tree --locked -e features -i shunsai`, fail on anything but
  `default`), which is also what makes `--all-features` safe to run. It does
  *not* grep manifests: that catches only a `[features]` forward and misses
  `features = ["slider-naive"]` on the dependency line, which is the likelier
  mistake and enables the backend just as effectively.
- **`shogi_core` must appear exactly once in `Cargo.lock`**, or `Move` and
  `Square` silently become two incompatible types. rinsai's requirement has to
  stay compatible with shunsai's; CI asserts the count.
- **Effective MSRV is 1.88** (a let-chain at `movegen.rs:589`) and shunsai does
  not declare it. rinsai's `msrv` CI job is the only check on it anywhere.
- Max legal moves in any shogi position is **593**.

### Local prototyping against a shunsai branch

While the dependency is a git rev, the local override is keyed on the git URL,
not on `crates-io`:

```toml
[patch."https://github.com/sugyan/shunsai"]
shunsai = { path = "../shunsai" }
```

Note that a git worktree under `.claude/worktrees/` is two levels deeper, so
`../shunsai` does not resolve from one — use an absolute path there.

## Decisions recorded but deliberately not built

CLAUDE.md forbids building for a consumer that does not exist yet, and requires
recording the condition instead. These are those records.

### …and the one exception, and the rule that replaced its test

`score.rs` went into step 1 with no caller at all. The exception was deliberate
and narrow: **a type that exists to freeze a convention is not speculative
building, because the convention is what the next step is about to depend on.**
Negamax sign, centipawn scale, the mate band and `MAX_PLY` are decisions step 2
would otherwise have made implicitly, and the cost of getting one wrong is a
class of sign bug that SPRT reads as "that patch was bad". That half held: step
2 uses every one of them.

The *test* step 1 wrote for itself did not. It said: if a step-1 type has API
that no step-2 caller uses, it was built too early — so delete `clamp_to_eval`,
`Score::NONE` and the four arithmetic impls at step 2. Step 2 uses none of the
six. **The rule is now:**

> A surface with no caller stays if — and only if — a *specific* caller can be
> named, and the name goes in its doc comment. Otherwise it goes.

"It'll probably be useful" is what produces the state this audit exists to
prevent; a named caller is a record rather than a wish, and re-adding three
lines later is not the expensive part. All six can be named — `Score::NONE` is
step 3b's empty transposition-table slot, the arithmetic is E1's aspiration
widening and step 3b's mate-score-by-ply adjustment, `clamp_to_eval` is E3's
network scale conversion — so all six stay, each carrying its name.

`Score::NONE` additionally carries a warning, because it is an active trap: it
compares as +32 002, above everything including `INFINITE`, so using it to seed
a maximum means `if score > best` never fires and the search silently keeps its
first candidate. `Option<Score>` is the right shape and the search uses it.

**A named caller that does not turn up is also a result, and step 3a produced
one.** `MAX_PLY`'s doc said the search's per-ply state would gain a static
evaluation "from step 3", which is why step 2 declined to write the `Stack`
struct for what was still a one-field array. It did not happen: quiescence
computes its stand-pat as a local and nothing reads it again, so per-ply state is
*still* exactly the principal variation. The `Stack` struct is deferred to E1 —
killers (item 3) or futility's per-ply static evaluation (item 9), whichever
lands first — and the trigger is written as "the first *second* field" rather
than as a step number, since a step number is what went stale here. The doc has
been corrected; the prediction is kept because a prediction that failed is worth
as much as one that held.

Rationale for the change is in DESIGN.md §9.

### The move buffer — decided in step 1, built in step 2 as recorded

Built as specified: a **shared, ply-threaded `Vec<Move>`** reserved once at
`MAX_LEGAL_MOVES * MAX_PLY` elements, sliced per ply, generation appending and
the caller truncating back. shunsai's own `perft_materialize` shape
(`examples/perft.rs:71`). The rejected alternatives, still the reasons:

- **`ArrayVec<Move, 593>` per ply on the stack** — same size but *per node*.
  `Move` has no `Default`, so an `unsafe`-free version needs a dummy fill, and
  avoiding that means `MaybeUninit`, which the workspace denies.
- **`Vec<Move>` per ply** — one malloc/free per node.
- **A ply-indexed array inside the search stack** — where E3's accumulator stack
  goes, but it makes the move list borrowed while recursing on `&mut stack`.

Two things the decision did not say, learned from building it. The buffer hands
out **moves by value, never a slice** — a slice borrows the buffer across the
recursive `&mut self` call and does not compile. And **forgetting to truncate is
a leak, not a wrong answer**: every ply reads its base before generating, so no
ply ever sees another's moves. `MoveBuf`'s own tests therefore cannot catch a
missing truncate; `negamax::tests::the_move_buffer_comes_back_empty` is what
does, and the sabotage was run to confirm it.

The original note put the buffer at 593 × 3 B × 128 ≈ 222 KiB. `Move`'s layout
is not something Rust guarantees, so the reservation and its test are both in
**elements**.

At E1 the element becomes scored — either a parallel score array indexed
identically, or a `ScoredMove`. `get` keeps its signature either way, so the
search's loop does not change shape.

### `[patch.crates-io]` for an unpublished crate — checked, and rejected anyway

It **does** work: cargo's own testsuite covers it
(`tests/testsuite/patch.rs::nonexistent`), and the Cargo book says sources can
be patched with versions of crates that do not exist. Rejected on three grounds:
`shunsai = "0.1"` in the manifest would assert something false; `[patch]` is
honoured only at the workspace root, so `rinsai-search/Cargo.toml` would stop
saying where shunsai comes from; and a patch would keep silently overriding the
real crate once it is published, so the switch we are trying not to forget would
never go red. Recorded so it is not re-opened.

### A `quit` watchdog — still not yet, and now for a better reason

The step-1 note said the open risk was "a step-2 search that ignores `stop` — an
infinite loop with no poll". Step 2's search polls, `an_infinite_search_stops_when_told`
proves it, and removing the poll is one of the sabotage mutations that goes red.
So the specific hazard is closed and a watchdog is still not needed. If one is
ever added it should be a **bounded wait** in `shutdown` rather than a
`process::exit`: an unconditional exit would make a hung in-process conformance
test *pass*.

### `quit` stops the search, and that shapes how the engine can be tested

`usi::run` reads its input as fast as it arrives, so in `printf … | rinsai` the
`quit` reaches `shutdown` microseconds after the `go` and the search returns
whatever depth 1 gave it. That is correct — `quit` means quit — but it has two
consequences worth writing down, because both were discovered the hard way:

- **Every dialogue in `tests/usi_conformance.rs` is really answered from the
  depth-1 iteration**, `go movetime 1` included. A conformance test cannot
  exercise a search of a stated size. Anything that needs one belongs in
  `rinsai-search`'s own tests or in `usi_process.rs` over real pipes.
- **Driving the release binary by hand needs a reader that waits for
  `bestmove`** before sending `quit`, the way a GUI does. A plain pipe measures
  nothing.

### `go ponder` starts its clock too early

A `go ponder movetime n` starts its deadline when the search starts, not at
`ponderhit`. Harmless at step 2 — the driver holds the answer back anyway and
the search simply finishes early and waits — but wrong, and E2 is where ponder
becomes real and this gets fixed.

### There is no instrument for strength during E0, and that is structural

CLAUDE.md calls the measurement loop the project's spine and DESIGN.md §8 says
it starts at E0. In practice `bench` arrives at step 3b, the opening set at step
6 and the SPRT harness at step 7, so **steps 2 to 5 cannot be measured at all**.
"One feature = one SPRT" is a rule about SPRT hygiene and it does not bite yet;
what the E0 step boundaries actually buy is reviewable diffs and isolated risk.
Worth stating plainly so that nobody reads E0's merges as measurement debt, and
so that nobody reaches for an SPRT number that cannot exist. Games against
`../benchmarks` during E0 are "it plays", never "it is stronger".

### Threads, not async — and no `tokio`

Asked and answered rather than assumed. The concurrency here is two threads: one
blocked on stdin, one running a search flat out. Async runtimes exist to wait on
many I/O sources cheaply, and this has exactly two (stdin, stdout) and one
long-running **CPU-bound** task — which under `tokio` would have to go to
`spawn_blocking`, i.e. back to a thread, having gained nothing but a dependency.

It gets less appealing with each phase, not more. E2's Lazy SMP is N OS threads
each searching at full tilt over a shared transposition table: the shape async
is specifically not for. E2's CSA client is **one** TCP connection, which a
blocking socket on its own thread handles with less machinery. E3's self-play
drives the search library in-process with no protocol at all. And `tokio` is a
large dependency in a project where every one of them is provenance-scan surface
(CLAUDE.md §7) — the whole tree is three third-party crates today.

The condition that would reopen it: something that has to wait on *many*
independent I/O sources at once inside the engine process. Nothing on the E0–E6
roadmap does. (The match harness at step 7 runs many engine processes, but that
is Ayane, in Python.)

### `usi.rs`, not `usi/mod.rs`

Modules with children are `foo.rs` beside `foo/`, not `foo/mod.rs`. This is
shunsai's layout too (`src/sliders.rs` beside `src/sliders/`), so it is the
family convention rather than a preference — and it keeps editor tabs and greps
distinguishable, which is the reason the ecosystem moved.

### What a caught panic leaves behind

The worker catches a panic from `Searcher::search` and answers `resign`, which
keeps the engine playing rather than silently dead. The residual risk is named
rather than solved: a searcher that panicked may have left its own state
inconsistent — from step 3b that is a transposition table. `usinewgame` clears it,
and one bad game beats a dead engine, but if step 3b finds a way for a corrupt TT
to survive into the next game, this is the place to revisit.

### Two branches with no test, named rather than left to be discovered

- **`Engine::go`'s fallback for a gone worker.** `submit` returns `false` and the
  protocol layer answers `bestmove resign` itself, because it has already taken
  the `go` and owes exactly one answer. `SearchDriver` has a unit test that
  `submit` reports the failure; the *protocol* half has none, and with the outer
  `catch_unwind` now keeping the worker alive through any panic in the loop
  body, there is no longer a way to reach it from a dialogue. It is defensive
  code guarding an invariant, kept for that reason and untested for the same one.
- **`Game::clone`'s Zobrist cross-check is `debug_assert`.** It runs under
  `cargo test` and in a debug build, and is compiled out of the release binary
  that plays. The lockstep record is therefore a property the *suite* checks,
  not one a live game does. Step 3b is where to reconsider — and the framing it
  inherits from step 2 ("a key mismatch with a transposition table in play has
  somewhere much worse to go") is worth checking before acting on, because it may
  be wrong: the searcher's board is the one `Game::search_board` **rebuilds**
  from the record, and `Position::new` recomputes the key from scratch, so a
  drifted incremental key does not reach the table. What the check protects is
  the *record*, whose consumers are `sfen()`, step 4's repetition history and
  E2's CSA client. Weigh it there rather than here.

## Where the sparring opposition is

GPL binaries live only in the local-only `../benchmarks` repository and are only
ever *run* as separate processes (CLAUDE.md §7, run-vs-link). What is built and
runnable there today:

| Engine | Path | Note |
|---|---|---|
| YaneuraOu | `YaneuraOu/source/YaneuraOu-by-gcc` | **MaterialLv1** — no NNUE, but it honours `NodesLimit`, which is what an E0 ladder needs |
| Apery | `apery_rust/target/release/apery` | also material-only (no eval files present) |
| Fairy-Stockfish | `Fairy-Stockfish/src/stockfish` | USI dialect |

**Not present anywhere**: Lesserkai, 技巧2, shogi-server, Ayane, and any
floodgate CSA archive. Step 6 has to download the game records; step 7 has to
fetch Ayane (Apache-2.0) and vendor it.
