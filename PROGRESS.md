# rinsai — progress ledger

Where the implementation is, what the next step is, and what a session needs to
know before touching the code. [DESIGN.md](./DESIGN.md) is the plan and
[CLAUDE.md](./CLAUDE.md) is the rules; **this file is the state**. Update it at
the end of every step.

## E0 — baseline: play legal moves and don't hang pieces

E0 is split into seven sub-steps, one per pull request.

| # | Step | Status |
|---|---|---|
| 1 | Skeleton + USI shell — workspace, CI, shunsai wiring, protocol loop, `bestmove` = a legal move | **done** |
| 2 | Material evaluation + iterative-deepening negamax αβ — PV, `info` output, mate scoring | **done** |
| 3 | TT + quiescence search — `rinsai bench`, fixed-depth node-count regression | next |
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

| Position | Depth | Nodes | Time (this machine) |
|---|---|---|---|
| initial | 6 | 585 568 | 23 ms |
| drop-heavy middlegame | 4 | 838 976 | 40 ms |
| drop-heavy middlegame | 5 | 13 888 371 | 445 ms |

**The two columns are not the same kind of fact, and the difference matters from
step 3 on.** The node counts are deterministic: every run reproduces all three to
the digit, which is why they are what `bench` freezes as a regression test. The
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

**Expect the scores to alternate by depth parity, and do not go looking for the
bug.** From the initial position, depths 3 and up report +215 at odd depths and
0 at even ones (depth 1 is 0: the initial position has no capture one ply in).
215 is a pawn's board value plus a pawn's hand value — after `7g7f`, every White
reply leaves Black a free pawn on ply 3, `8h×3c` if the diagonal is open and
`8h×4d` if `4c4d` blocks it, and at depth 4 White recaptures with `2b` so the
score falls back to 0. That is the horizon effect, it is what DESIGN.md means by
"material evaluation without qsearch fails", and step 3 is the answer.

And it holds a whole game: 70 plies against the local material-only YaneuraOu
(`NodesLimit 10000` against `go byoyomi 200`), ending in rinsai being mated and
answering `resign`. No illegal move, no protocol stall, nothing on stderr. Step
1's game ended at 22 plies, and **the comparison means nothing** — different
time controls, one game each, and E0 has no instrument for strength. It is
recorded as "it plays", which is all it is.

## Step 3 — what to do next

TT + quiescence search + `bench`. Two things to carry in from step 2:

1. **The transposition move must be tried first, in the same PR as the table.**
   DESIGN.md's E1 list puts "TT move ordering" at item 1 of a *later* phase,
   which would leave steps 3–7 — the whole SPRT harness bring-up — running on a
   search with no ordering at all, and a table whose move nobody tries is half a
   table. MVV-LVA, killers and history stay at E1: those are interior-node
   heuristics that each want their own SPRT.
2. **`bench` freezes node counts, so the counting convention below is now
   load-bearing.** Note that step 2's counts will change wholesale when
   quiescence lands; freeze after, not before.

Also waiting at step 3: `seldepth` and `hashfull` in the `info` line (both are
meaningless until there is a qsearch and a table), a `Stack` struct for per-ply
state (step 2 has only the principal variation there, so a one-field struct
would have been premature), and whether `Game::clone`'s Zobrist cross-check
should stop being a `debug_assert`.

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
  because quiescence runs at negative depth from step 3.
- **`MAX_PLY = 128`**, sizing the search stack, the mate band and (from step 2)
  the move buffer together.
- **Node counting: one node per `search`/`qsearch` entry, including the root,
  excluding bulk-counted leaves.** Fixed now because step 3 freezes those
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

- **`DEFAULT_DEPTH = 4`, and it must stay even.** It answers "no budget of any
  kind was named" and only that — applying it as a ceiling over a stated budget
  as well was a real bug in this step, caught by driving the release binary
  rather than by any test, and `the_depth_ceiling_follows_the_budget_that_was_named`
  now pins it. Even because the root is the engine's own move, so an odd depth
  ends the line on a capture of ours nobody answers; even ends after the reply,
  which is the safe side while there is no quiescence search.

- **The poll interval is 1024 nodes**, checked at the top of the interior node
  and once per deepening iteration — *not* per root move, which would break the
  guarantee below. Measured rather than assumed: a release build runs at roughly
  20–30 M nodes/s at this step, because most nodes are depth-zero leaves that
  only evaluate, so 1024 nodes is about 40 µs. Expect that to grow as nodes get
  more expensive; step 5's SPRT is where the number stops being inherited.

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

- **Depth 1 always completes.** The poll only fires on a multiple of 1024 and a
  depth-1 iteration is `1 + N ≤ 594` nodes, so every `go` emits at least one
  `info` line and returns a move the search actually chose rather than the seed.
  This is what keeps `go movetime 1` honest instead of degenerate.

- **Each iteration re-seeds the root list with its own answer.** Without it,
  deepening is only repeated work: an iteration cut short reports the best of
  whatever prefix of the root list it reached, which can be a *worse* move than
  the last completed iteration chose — measured at 1 590 cp worse in the
  middlegame fixture before the three lines existed. This is root-only and is
  not the interior move ordering E1 is about.

- **The `info` line carries `depth time nodes nps score pv`, and nothing else.**
  `seldepth` would be a constant equal to `depth` until there is a quiescence
  search; `hashfull` needs a table; `multipv` and `currmove` have no consumer.
  A field that cannot be filled honestly is not printed.

- **`MoveBuf::get` returns a `Move` by value, not a slice**, and that is what
  makes the recursion compile: a slice would borrow the buffer across the
  recursive `&mut self` call. **The root move list lives outside the buffer**,
  because it persists across iterations and gets reordered, which is not what a
  ply-threaded buffer is for.

- **A surface with no caller stays if — and only if — its caller can be named.**
  This replaces the rule the step-1 exception wrote for itself ("if step 2 does
  not reach for it, delete it"). See the section below.

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
step 3's empty transposition-table slot, the arithmetic is E1's aspiration
widening and step 3's mate-score-by-ply adjustment, `clamp_to_eval` is E3's
network scale conversion — so all six stay, each carrying its name.

`Score::NONE` additionally carries a warning, because it is an active trap: it
compares as +32 002, above everything including `INFINITE`, so using it to seed
a maximum means `if score > best` never fires and the search silently keeps its
first candidate. `Option<Score>` is the right shape and the search uses it.

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
it starts at E0. In practice `bench` arrives at step 3, the opening set at step
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
inconsistent — from step 3 that is a transposition table. `usinewgame` clears it,
and one bad game beats a dead engine, but if step 3 finds a way for a corrupt TT
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
  not one a live game does. Step 3 is where to reconsider: a key mismatch with a
  transposition table in play has somewhere much worse to go.

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
