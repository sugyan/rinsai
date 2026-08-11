# rinsai — progress ledger

Where the implementation is, what the next step is, and what a session needs to
know before touching the code. [DESIGN.md](./DESIGN.md) is the plan,
[CLAUDE.md](./CLAUDE.md) is the rules, [CONVENTIONS.md](./CONVENTIONS.md) is
what is frozen and [DECISIONS.md](./DECISIONS.md) is why; **this file is the
state, and the only home for a measured number.** Update it at the end of every
step.

## E0 — baseline: play legal moves and don't hang pieces

E0 is split into seven sub-steps, one per pull request — except step 3, which
became two when building it showed that quiescence alone changes three frozen
conventions and rebaselines every committed node count (DECISIONS.md).

| # | Step | Status |
|---|---|---|
| 1 | Skeleton + USI shell — workspace, CI, shunsai wiring, protocol loop, `bestmove` = a legal move | **done** |
| 2 | Material evaluation + iterative-deepening negamax αβ — PV, `info` output, mate scoring | **done** |
| 3a | Quiescence search — captures only, `seldepth` | **done** |
| 3b | TT + transposition-move ordering — `hashfull`, `USI_Hash`, `rinsai bench` | **done** |
| 4 | Repetition (千日手) + 連続王手 — history *queries*, mate-in-1..5 suite | **done** |
| 5 | Time management — byoyomi / Fischer / movetime, `stop` responsiveness | next |
| 6 | `openings-v1` extractor — floodgate CSA → balanced opening SFENs (`crates/xtask` arrives here) | |
| 7 | Match harness + SPRT — Ayane vendored, `tools/opponents.toml.example` | |

### E0 exit criteria not yet met

- **`TODO(shunsai-0.1-release)`** — the dependency is a git rev, which DESIGN.md
  §2 and shunsai's own DESIGN.md both say it should not be. E0 cannot be
  declared finished with this outstanding. ⚠️ `git grep
  'TODO(shunsai-0.1-release)'` finds every mention, which is more than the four
  things that actually change: the dependency line in `Cargo.toml`, the
  `allow-git` entry in `deny.toml`, the deviation footnote in DESIGN.md §2, and
  an appended DECISIONS.md entry. The rest are prose that refers to them.

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
- CI: fmt, clippy `-D warnings`, tests, `cargo doc` with `-D warnings` (it
  catches a broken intra-doc link and nothing else — ⚠️ it does **not** see a
  false claim, which is the defect class CLAUDE.md §4 exists for), an MSRV job
  on 1.88, `cargo deny check advisories licenses bans sources`, and two CI
  guards, both described under Traps below.

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

| Position | Depth | Nodes, step 2 | Nodes, step 3a | Nodes, step 3b | seldepth, 3b | Time, step 2 |
|---|---|---|---|---|---|---|
| initial | 6 | 585 568 | 376 695 | **128 167** | 29 | 23 ms |
| drop-heavy middlegame | 4 | 838 976 | 7 030 029 | **4 049 636** | 29 | 40 ms |
| drop-heavy middlegame | 5 | 13 888 371 | 378 188 510 | **33 416 692** | 31 | 445 ms |
| two lone kings | 6 | 1 431 | 1 431 | **747** | 6 | — |

⚠️ **The step-3b column is at the advertised default table size (256 MiB) and
is not the count at another size.** Table size moves a node count on its own —
a bigger table evicts less — which is why `bench` fixes its own size rather than
reading `USI_Hash`, and why a count quoted without a size is not a result. Each
row was taken three times in one process and reproduces to the digit.

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
table is not by itself a result. Note that the spread does not track the
magnitude: as a fraction the three rows sit at about ±2%, ±4% and ±3%, so the
*widest* is a short row and a threshold cannot be read off row length either
way. That is the whole reason
a quiet machine is a precondition for the SPRT loop (CLAUDE.md §3): noise
cannot move a node count and can move a time far enough to invent an
improvement.

**Node rate, and why it justifies nothing.** These rows imply roughly 8–10 M
nodes/s. Step 2 measured 20–30 M, and the gap is not a regression: most of step
2's nodes were depth-zero leaves that only evaluated, and quiescence replaced
them with nodes that generate. ⚠️ The rate is therefore **not** an argument for
the 1024-node poll interval. That interval is untuned, and step 5's SPRT is
where it stops being inherited (CONVENTIONS.md).

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
small" — the threshold *is* the fingerprint's magnitude, so the effect returning
fails it whatever depths it returns at. ⚠️ It remains an upper bound and nothing
more: a search that broke in some new way and reported ±90 would pass. Only the
fingerprint is pinned, not the scores.

And step 2 held a whole game: 70 plies against the local material-only YaneuraOu
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
(E1 item 8, beside SEE), checks (E1 item 8 as well — they need `gives_check`,
which shunsai does not have), SEE (E1 item 8 — needs `attackers_to`), delta
pruning (E1 item 9). DESIGN.md's item 8 groups all three of the first in one
sentence; item 7 is check *extension*, which is a main-search feature and not
this.
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

## What step 3b delivered

A transposition table, the transposition move searched first, `hashfull` on the
`info` line, an honoured `USI_Hash`, and `rinsai bench`. The column above is the
result; what follows is what building it changed or found out.

### The table, and what it is worth

16-byte slots, a power-of-two count indexed by `key & mask`, the **full** 64-bit
key stored so a hit is never an index accident, and depth-preferred replacement
with entries from earlier searches giving way first. The move is stored as a
`CompactMove` — `Option<CompactMove>` is two bytes *guaranteed*, being a
`#[repr(transparent)] NonZeroU16`, where `Move` carries no `#[repr]` at all.

**The transposition move being searched first is worth more than the scores are,
and by a lot.** Node counts with the stored move swapped to the front of the
list against the same search without that one line, one MiB, release:

| Position | d=2 | d=3 | d=4 | d=5 |
|---|---|---|---|---|
| initial | 1.00× | 1.00× | 1.01× | 1.00× |
| initial + `7g7f 3c3d` | 1.00× | 1.46× | 1.39× | 1.44× |
| initial + `2g2f 8c8d 2f2e 8d8e` | 1.00× | 1.00× | 1.94× | **14.02×** |
| an open middlegame | 1.00× | 1.53× | 1.57× | 2.13× |
| drop-heavy middlegame | 1.00× | 1.12× | 1.55× | **8.08×** |

Two things worth reading rather than skimming:

- **The initial position gains nothing at all**, at any depth measured. It is the
  one fixture where the ordering is worth 1.00×, and the first draft of
  `a_repeated_search_reuses_what_the_last_one_proved` used it — so deleting the
  swap left that test green. This is why
  `the_transposition_move_is_searched_first` uses a different fixture, and why
  the fixture is named as load-bearing in its doc.
- **The effect is strongly super-linear in depth**, 1.00× → 1.94× → 14.02× on one
  fixture across two plies. E1's ordering work (MVV-LVA, killers, history) is
  measured against a search that already has this.

### Quiescence does **not** probe the table, and the reason is not the measured one

Whether quiescence should probe and store as well was left to measurement rather
than decided up front. It was measured, by patching it in and taking it out
again — the instrumentation was throwaway and is not in the tree.

| Position | Depth | Nodes, table in interior nodes | Nodes, in quiescence too | Ratio | ms | ms |
|---|---|---|---|---|---|---|
| initial | 6 | 128 167 | 99 267 | 0.77× | 15 | 17 |
| drop-heavy middlegame | 4 | 4 049 636 | 2 036 973 | **0.50×** | 432 | 419 |
| drop-heavy middlegame | 5 | 33 416 692 | 15 649 522 | **0.47×** | 3 379 | 2 926 |

**The expectation was that it would lose, and it does not — it halves the tree.**
The argument for expecting a loss is the ordinary one: quiescence is 91–99% of
all nodes, so probing there would thrash the table with shallow entries and evict
the interior ones that pay. That argument is refuted on every row, and it is
recorded here because it is what would otherwise have been written down as the
*reason*, unmeasured, and believed.

It is nonetheless **not shipped in 3b**, and DECISIONS.md carries why. The number
is here so that E1 does not re-derive it.

### `bench`, and what fixes a baseline

`rinsai bench [depth]`, a binary subcommand following the Stockfish convention
rather than a criterion target. The position set is `positions/bench-v1.sfen`,
compiled in with `include_str!` so the counts cannot depend on the working
directory, and every line carries its provenance in the file's header.

⚠️ **The per-position counts live in `crates/rinsai/src/bench.rs`, not here, and
that is the one deliberate exception to "a number goes in a PROGRESS.md table
and nowhere else".** A regression baseline has to be executable to be a
regression test; a copy of it here would be a second copy that drifts. What
belongs here is the total and the conditions:

| | |
|---|---|
| positions | 7 |
| depth | 4 |
| table | 16 MiB, fixed — **not** `USI_Hash` |
| total nodes | 4 133 205 |
| reproducibility | three runs, identical to the digit |
| debug and release | identical counts |
| release, one run | 451 ms, 9.16 M nodes/s — ⚠️ **not a result**, see below |

⚠️ **The time is the one figure here that is not evidence of anything.** The
machine was not certified quiet — this session had been building and running
tests throughout — so it is recorded as "what the thing did", the same standing
step 3a gave its own times. The node count is the measurement; the same rule as
everywhere else in this file, and the reason CLAUDE.md §3 makes a quiet machine
a precondition for the SPRT loop rather than for this.

**Three inputs are fixed rather than inherited, and each of them would otherwise
be a hidden variable in the baseline**: the table size (an operator's option
must not move a regression count), the position order together with a
`new_game` between positions (or position *n* is searched against whatever
1..n−1 left in the table), and the depth.

### Allocating the default table costs about as much as an E0-depth search

Three measurements of allocating a 256 MiB table alone, in one process: **20.6
ms, 12.1 ms, 6.3 ms** — the spread is the usual first-touch effect, not noise
between runs. A whole depth-6 search from the initial position is 13–20 ms on
the same machine and in the same session, which is what makes the two
comparable despite neither being a certified-quiet figure. It is paid **once per
searcher**: in `main`, before the protocol loop starts, and again on the worker
for a `setoption name USI_Hash`. ⚠️ Not at `isready`, whatever that method's
comment used to say. Recorded because the two figures being the same order is
not obvious, and because a future change that moved allocation onto the `go`
path would look harmless and would not be.

### `hashfull` is honest and, at E0 depths, uninformative

Permille of the first thousand slots holding an entry from the current search.
At the advertised 256 MiB it reads **0, 2, 6, 0** on the four fixtures above —
an E0-depth search fills almost none of a quarter-gigabyte table. It is real
data (it rises with depth, and reads 0 in the depth-1 iteration because the root
dispatches straight into quiescence, which stores nothing), but the field only
becomes interesting when E1 and E2 search deeper.

⚠️ Consequence for tests: `hashfull_reports_the_table_filling_up` needs the
1 MiB table its helper builds. The same assertion at the default would be a test
that cannot **pass**.

### Findings carried in from step 3a's planning, and how each turned out

- **`Option<CompactMove>` is 2 bytes, guaranteed** — held, and used.
- **Validate the transposition move by list membership, not `is_legal`** — held.
  `is_legal`'s doc has been corrected: it named a second caller "from E0 step 3b"
  that does not arrive.
- **An Exact-bound hit may cut only when its score falls outside the window** —
  implemented, and **the end-to-end hazard it predicts could not be
  demonstrated.** Letting the `Bound::Exact` arm cut unconditionally leaves every
  published line byte-identical across three fixtures at depths 1–8, moving only
  the node count — not at all through depth 5, and by at most 1.2% after that.
  The restriction is kept, because the path it
  guards is reachable and the cost of guarding it is that 1%; but the sabotage
  note that claimed a shortened line was **false**, and the test that claimed to
  catch it could not. Both are replaced by a unit test on the rule itself.
- **Route `USI_Hash` through the existing FIFO** — held (`Command::SetHash`).
- **The test suite must not allocate 256 MB per searcher** — held.
  `NegamaxSearcher::with_hash_mb` exists for it and every test uses 1 MiB.
- **`shogi_core`'s `hash` and `ord` features are off in this graph** — still
  true. The table needed neither: it compares keys as `u64` and moves by
  `PartialEq`. ⚠️ **It was also named as step 4's first obstacle, and it was not
  one.** Repetition compares keys as `u64` and hands by the `PartialEq`
  `HistoryEntry` derives, so nothing wanted `Hash` or `Ord` either. Two steps
  have now been predicted to need those features and neither did.

### It still plays

104 plies against the local material-only YaneuraOu (`NodesLimit 10000` against
`go byoyomi 200`), ending in rinsai being mated and answering `resign`. Every
move was replayed through rinsai's own parser afterwards, so "no illegal move"
is checked rather than assumed; no protocol stall, nothing on stderr from either
side. Step 3a's game ran 84 plies, step 2's 70 and step 1's 22, and **the
comparison still means nothing** — one game each, and E0 has no instrument for
strength. Recorded as "it plays", which is all it is.

## What step 4 delivered

千日手 and 連続王手の千日手 — the first thing in the project to *read* the
history `Game` has been recording since step 1 — and the mate-in-1..5 suite.
The protocol layer is unchanged: USI has no draw claim, the GUI or the server
adjudicates 千日手, and what the engine owes is a score that stops it walking
into a repetition it loses. That score is the whole feature.

### The rule, and the one place it is asked

`repetition.rs` holds it as a pure function over `&[HistoryEntry]` — no board,
no shunsai call — so the rule is testable without a search. Four occurrences of
one position, `key` filtering and hands confirming; the perpetual-check window
is the four occurrences themselves, and the side whose every move left the
opponent in check loses.

The searcher carries the game's history and extends it as it descends, and the
verdict is asked for **once, where a child is dispatched**. CONVENTIONS.md
carries why that site and not the top of the interior node; the short version is
that the depth-1 iteration dispatches straight into quiescence, and depth 1 is
the iteration that always completes.

⚠️ **One end-to-end case is not covered: the engine declining the check that
loses.** The verdict's *win* direction is asserted through a search
(`score cp 30000` from a side that is a rook down), and both directions are
asserted on the rule itself; the sign at the search seam is pinned by the
offset-swap sabotage. What has no search-level test is the behaviour that
actually loses games — the perpetual checker refusing the move that makes the
fourth occurrence.

The obstacle is fixture construction rather than effort, and it is worth writing
down because it will be met again. To discriminate, the repeating check must be
**strictly** best without the verdict and refused with it. Material cannot
supply that: a check forces the reply, so any threat the alternatives run into
is postponed by the check rather than avoided, and every quiet alternative
scores the same. Making the alternatives *lose* needs the losing side to be
otherwise mated — but a mate scores below the repetition band, so the engine
prefers the repetition either way, which is correct and non-discriminating. A
drawing alternative would do it, and that needs two four-fold repetitions
reachable from one position, which one line of history cannot offer. Left
undone rather than written weakly: a version that passes without the verdict is
the failure mode this log records more often than any other.

### Reading the history costs 1–8%, and it shrinks with depth

The same position searched two ways — through its whole 121-ply move list, and
from its own SFEN with no history at all. Node counts are identical on every
row, so this is the scan and nothing else. Release, three runs each:

| Depth | Nodes | With a 121-ply history | From a bare SFEN | Cost |
|---|---|---|---|---|
| 4 | 16 300 | 1.58 / 1.60 / 1.61 ms | 1.47 / 1.49 / 1.47 ms | **+8%** |
| 5 | 103 912 | 11.46 / 11.54 / 11.62 ms | 11.14 / 11.20 / 11.24 ms | **+3%** |
| 6 | 2 019 944 | 208.4 / 208.6 / 210.0 ms | 205.4 / 205.8 / 206.7 ms | **+1.5%** |

**The cost falls as the search deepens, which is the opposite of the shape a
per-node overhead usually has.** The reason is that the path is extended and
scanned at interior nodes only: quiescence is 91–99% of all nodes and pays
nothing, so the deeper the search the smaller the fraction of it that scans.

⚠️ **`bench` cannot see this and is not evidence about it.** Its seven positions
carry histories one to seven plies long, which is a scan of about three
comparisons. A position set with real game histories would be `bench-v2`, and
there is no consumer asking for one yet.

### `bench` did not move, and the prediction that it would was wrong

This file's step-4 note said to expect `the_frozen_counts_are_what_the_search_produces`
to fire. It did not — every per-position count is identical and so is the total,
`counts match`, in debug and release alike. It could not have fired: a depth-4
search from a history of at most seven plies cannot reach a fourth occurrence.
DECISIONS.md carries what the non-event is evidence *for*.

The time either side of the change, on one machine in one session, alternating
the two binaries so that drift cannot masquerade as a difference:

| | step 3b | step 4 |
|---|---|---|
| total nodes | 4 133 205 | **4 133 205** |
| alternating pairs, ms | 427 / 435 / 432 | 431 / 443 / 430 |
| runs of three, ms | 437 / 434 / 431 | 444 / 435 / 432 |

⚠️ **Not a result, and the machine was not certified quiet** — this session had
been building and running tests throughout. One of the three alternating pairs
goes the other way, which is the honest summary: the `in_check()` per interior
`do_move` and the three-comparison scan are below what this instrument can
separate from noise.

### The mate suite reaches five, and the estimate that said it would not was out by two orders of magnitude

The plan reasoned from branching — nine plies of full-width search with only
transposition-move ordering — and expected mate-in-4 and mate-in-5 to be too
expensive for `cargo test`. Measured, in the **dev** profile:

| Mate in | Depth searched | Nodes | Iteration that found it |
|---|---|---|---|
| 1 | 1 | 20 | 1 |
| 2 | 3 | 31 | 1 |
| 3 | 5 | 130 | 2 |
| 4 | 7 | 1 678 | 4 |
| 5 | 9 | **18 924** | 6 |

Depth 9 costs under six milliseconds. **The last column is why**: the mate is
found several iterations before the depth that nominally contains it — mate 3
at depth 1, mate 9 at depth 6 — because quiescence resolves a forced checking
sequence, and a forced sequence is the input alpha-beta prunes best. The
branching estimate was not wrong about full-width search; it was wrong about
which tree this is.

**The ladder built with golds in White's hand answered `mate 5` where `mate 9`
was intended, at 8 443 nodes.** Black captures whatever is interposed and may
drop it straight back, and a gold beside the bare king is mate at once. A pawn
there is 打ち歩詰め and illegal, which is what makes the pawn version hold its
length — recorded because it is a shogi rule doing load-bearing work in a test
fixture, and because the collapse is what found it.

### It still plays

142 plies against the local material-only YaneuraOu (`NodesLimit 10000` against
`go byoyomi 200` both ways), ending in rinsai being mated and answering
`resign`. Every move was replayed through rinsai's own parser afterwards, so "no
illegal move" is checked rather than assumed; no protocol stall, and nothing on
stderr from either side. Step 3b's game ran 104 plies, step 3a's 84, step 2's 70
and step 1's 22, and **the comparison still means nothing** — one game each, and
E0 has no instrument for strength. Recorded as "it plays", which is all it is.

⚠️ **This game reached no repetition, so it is not evidence about the feature.**
That is the ordinary case and the reason the rule needed constructed fixtures:
against an opponent playing to win, a fourfold repetition at these strengths is
rare, and waiting for one to occur naturally is not a test.

## Measurements the conventions rest on

[CONVENTIONS.md](./CONVENTIONS.md) carries the rules these produced and
[DECISIONS.md](./DECISIONS.md) the arguments; **the numbers live here and only
here.** Release build, one quiet machine, same caveat as the table above.

**Poll starvation in a sparse position** — two lone kings,
`4k4/9/9/9/9/9/9/9/4K4 b - 1`, about five legal moves a side. The nodes spent
*within* each iteration, depths 1 to 6, against the cumulative count:

| Depth | 1 | 2 | 3 | 4 | 5 | 6 |
|---|---|---|---|---|---|---|
| within the iteration | 6 | 15 | 53 | 120 | 386 | 851 |
| cumulative | 6 | 21 | 74 | 194 | 580 | 1 431 |

Every per-iteration figure is under the 1024-node poll interval, so a counter
reset per iteration never fires the poll through depth 6. Accumulating, it first
fires partway through depth 6.

**Depth-1 cost, and the fixture that can actually fail.** The first iteration
runs unclocked because a poll landing inside root move 0's subtree abandons it.
None of the four fixtures already in the suite can show that:

| Fixture | Depth-1 nodes |
|---|---|
| initial position | 31 |
| drop-heavy middlegame (matsuri) | 280 |
| an open middlegame | 422 |
| the 593-move position | 634 |
| matsuri + 2 plies — `the_first_iteration_is_never_abandoned` | **49 006** |

Only on the last does removing `Budget::without_limits` change anything: zero
`info` lines and an unsearched answer. A test written against any of the first
four would have been a test that could not fail.

**What the unclocked first iteration costs.** On the 49 006-node fixture,
`go movetime 1` takes **5–6 ms** over three runs — the whole depth-1 iteration —
against 1 ms on the ordinary drop-heavy position. So the worst case observed is
about 5 ms of overrun, not a proportional blow-up.

**Root re-seeding is worth 1 590 cp in the worst case seen.** Before the three
lines existed, an iteration cut short in the drop-heavy middlegame reported a
move scoring 1 590 cp below what the last completed iteration had chosen.

**`a_stated_budget_deepens_past_the_default`'s 300 000-node budget reaches depth
6**, so its `> DEFAULT_DEPTH` assertion is not on a knife edge.

**The sabotage re-run at step 4: 59 mutations, three false notes and one with
the wrong symptom.** Every note in the tree, applied at the site the note names
— which is what step 3b's review said the rule had been missing — with the whole
suite run against each and the failing tests recorded.

| | |
|---|---|
| mutations applied | 59 |
| fired on the test their note names | 55 |
| expected not to fire, and did not | 1 |
| notes that were **false** | 3 |
| notes naming the wrong **symptom** | 1 |

The one expected not to fire is `negamax`'s own `pv[ply].clear()`, still
unfalsifiable and recorded below. The other four:

- ⚠️ **`negamax`'s `mated_in(ply)` has no test at all, and three notes claimed
  otherwise.** Scoring it `mated_in(0)` leaves the **entire suite green, `bench`
  included**; the same mutation in `qsearch` fires all three tests the notes
  name. The reason is structural and is the same one that makes the mate ladder
  cheap: quiescence runs past the nominal depth, so a mate that reaches a
  reported line is resolved there first, and the deepening loop stops at the
  first iteration that returns a mate score. The interior arm still *runs* —
  every refuted line with a mate in it enters it — but nothing surfaces whether
  it used `ply` or `0`. **This is the second reachable-but-unfalsifiable line in
  `negamax`, beside the `pv[ply].clear()` below, and both have the same cause.**
- **"Make `generate_captures` return nothing and this falls back to exactly
  `1 + N`" is false.** A check is reachable one ply into that fixture, and a
  quiescence node in check takes the evasion branch, which generates every legal
  move and recurses whatever the capture filter returns. The note's other half —
  `child` evaluating instead of dispatching — does fire it.
- **"Drop the `us`/`us.flip()` distinction in either loop" is false for one of
  the two loops.** The fixture holds a rook in hand and nothing but kings on the
  board, so only the *hand* loop's copy is reachable; the board loop's is caught
  by the oracle comparison instead.
- **"Drop the deadline arm and this hangs rather than failing" names the wrong
  symptom.** It fails, in ten seconds, because that test searches off-thread
  behind a `recv_timeout` precisely so it cannot wedge the suite. What *does*
  wedge on that mutation is `the_first_iteration_is_never_abandoned`, which
  searches on the calling thread and carries no note.

⚠️ **Four of the sweep's own mutations were defective on the first pass**, and
finding that out is the reason the tally is worth anything: two did not compile,
and two were accidental no-ops — one replaced a constant with the literal it
already equals, the other only added a comment. A no-op mutation reports GREEN
and reads exactly like a false note. **The sweep needs its own check that the
mutation changed the behaviour**, not only that it changed the text.

**The sabotage re-run at step 3b: 35 mutations, three findings.** CONVENTIONS.md
requires re-running every sabotage after a change, previously verified ones
included. Step 3b is the second step to actually execute it. Each mutation was
applied to the tree, the whole suite run, and the failing tests recorded.

| | |
|---|---|
| mutations applied | 39 |
| fired on the test their note names | 34 |
| expected not to fire, and did not | 1 |
| notes that were **false** | 2 |
| behaviour with **no test at all** | 1 |
| pre-existing tests found **hollowed out** | 1 |

The one expected not to fire is `negamax`'s own `pv[ply].clear()`, recorded
below and still unfalsifiable with a transposition table in the search. The one
with no test was the transposition move being searched first — deleting the
`swap` left the suite green, and `the_transposition_move_is_searched_first` is
what closed that. The other three are each a different failure mode:

⚠️ **This sweep mutated the two mate-adjustment *functions* and not their call
sites, and missed the largest hole in the step because of it.** A review
afterwards deleted the `to_table` call from `store` and the `from_table` call
from `probe` and found the whole suite green — DECISIONS.md, 2026-08-11. A
mutation has to be made where the note is attached, not merely somewhere the
note's subject appears.

- **A note naming a mutation the test cannot see.** "Store a `Move` instead of a
  `CompactMove` and the size assertion fires" — it does not. `Option<Move>`
  happens to fit the same sixteen bytes on today's compiler, so the suite stays
  green. The rule is still right and the *reason* is now stated correctly: `Move`
  is forbidden because its size is **unpromised**, and "unpromised" is exactly
  what no size assertion can test.
- **A note predicting an end-to-end symptom that does not occur.** Above, under
  the Exact-bound cut.
- **A pre-existing test hollowed out by this step's own change.**
  `each_iteration_starts_from_the_last_answer` compares the search's answer
  against the head of the root list, which says nothing unless the answer differs
  from the move shunsai generates first. At the initial position it used to; with
  a transposition table in the search it no longer does, so deleting the
  re-seeding left the test green. Only the drop-heavy fixture still disagrees, of
  five measured, and the test now asserts that disagreement out loud *before* the
  assertion that matters — so the day that fixture stops discriminating, it says
  so instead of going quiet.

⚠️ **`negamax`'s own `pv[ply].clear()` cannot be shown to matter, and
[`qsearch`]'s can.** Deleting the clear at the top of the *interior* node leaves
the whole workspace green, and a sweep over 9 fixtures × depths 1–6 — 101 `info`
lines — reproduces `depth`, `seldepth`, `score` and `pv` **byte-identically**.
Deleting the *quiescence* copy turns `finds_the_mate_in_one` and
`the_reported_pv_is_playable` red. The asymmetry has a reason: an interior node
that never raises alpha has failed low, so its parent fails high and never reads
its line, whereas a quiescence node can return an exact score without writing
one — a stand-pat that beats alpha but no capture, or an evaluation forced by
`ply >= MAX_PLY` or by `QS_MAX_CHECK_PLIES`. The line stays: it costs nothing,
and E1's aspiration windows give the root a real beta, which is exactly the
condition this argument assumes away. Recorded so a future "delete the dead
line" has something to check itself against.

⚠️ **`negamax`'s `mated_in(ply)` is the second line of that kind, found at step
4**, and both sit in the same node for related reasons. Scoring it `mated_in(0)`
leaves the whole suite green including `bench`, while `qsearch`'s copy fires
three tests. Here the cause is the deepening loop rather than the window:
quiescence searches past the nominal depth, so a mate that reaches a reported
line is proved there first and the loop breaks before any interior node meets an
empty move list on the principal variation. Also reachable, also unfalsifiable,
also kept.

**Prose volume, as of this step.** The figures the 2026-08-09 DECISIONS.md entry
rests on, measured over `crates/**/*.rs` by bytes of comment lines against bytes
of non-blank non-comment lines:

| | before | after |
|---|---|---|
| comment bytes | 127 617 | 97 099 |
| code bytes | 138 206 | 138 206 |
| comments as a share of non-blank text | 48.0% | 41.3% |

DESIGN.md was 63 426 bytes of which §9 was 40 482 (63.8%), and §9 accounted for
37 241 of the file's 39 785 bytes of lifetime growth (93.6%).

**⚠️ The wasm target compiles and the binary has no engine in it.** Measured on
rustc 1.94.1, `wasm32-unknown-unknown`, at `1dbf0cd`. `cargo check` for the
target passes unmodified; the release binary does not run:

| | |
|---|---|
| `rinsai.wasm`, release | 89 223 bytes |
| imports | **none** |
| `main()` | traps — `RuntimeError: unreachable` |
| exports | `main`, `memory`, the `__data_end` / `__heap_base` globals, and the `#[no_mangle]` C ABI of `shogi_core` and `shogi_usi_parser` |
| deepest surviving rinsai symbol | `rinsai_search::search::SearchDriver::spawn` |
| absent | `qsearch`, `generate_moves`, `undo_move`, `evaluate`, `legal_moves` |
| rinsai source paths referenced | one — `crates/rinsai-search/src/search.rs` |

`thread::Builder::spawn` returns `io::Result` and cannot succeed on this target,
so `search.rs`'s `.expect("the operating system can start a thread")` always
fires and everything downstream of it — the entire search — is dead code. That
panic message and that one path are what survive: the string `the operating
system can start a thread` is in the binary and the search is not. The std paths
it references say the rest — `unsupported/time.rs`, `sync/mutex/no_threads.rs`,
`thread_local/no_threads.rs`.

**So a green `cargo check` on this target is not evidence of portability**, and
the CI step that runs it is scoped accordingly — see the comment on it.

**NNUE weights against a browser download.** Arithmetic, not measurement:

| Feature transformer | Bytes | |
|---|---|---|
| HalfKP 125 388 × 256, int16 — the E3 plan | 64 198 656 | 61.22 MiB |
| P-only 1 548 × 256, int16 | 792 576 | 774 KiB |

The rest of `halfkp_256x2-32-32` is the 512→32→32→1 head, 17 440 int8 MACs and
about 17.5 KB — **99.97% of the file is the feature transformer**, so the
feature set is the only dimension that moves the download. One external figure,
attributed rather than measured here: `@mizarjp/yaneuraou.k-p` ships a playable
browser shogi engine with a small net embedded in about 2.6 MB, which is the
order a web build has to reach.

## Step 5 — what to do next

Time management: turning `btime` / `wtime` / `binc` / `winc` into a per-move
allowance, which CONVENTIONS.md currently records as *deliberately ignored* —
"a budget the engine was told" against "a budget it would have to decide".

1. **The margin between stopping on time and moving on time is the subject, not
   a detail.** CONVENTIONS.md's Time control rule says the deadline is
   `search()`'s entry plus the stated budget exactly, with nothing reserved for
   building and flushing `bestmove` or for the wire back. Locally that is
   microseconds; over a network a byoyomi overrun is an immediate loss, which is
   why that rule also forbids entering rinsai anywhere the clock is enforced
   across a network until this step lands.
2. **The 1024-node poll interval stops being inherited here.** It has been
   conventional and untuned since step 2, and this is the first step with a
   reason to move it.
3. **`Budget` wants an injectable clock, and two other things are waiting on
   the same seam.** A time-management SPRT cannot be written against a wall
   clock. The same injection is what CONVENTIONS.md's Portability rule needs —
   `NegamaxSearcher::search` reads `Instant::now()` unconditionally for the
   `info` line's `time` and `nps`, which is what makes a clock-free caller
   impossible today rather than merely unwritten.
4. **The unclocked first iteration overruns by the whole of it**, not just by
   root move 0, which is all the guarantee needs. Narrowing it is not hard and
   it changes what the engine does with a clock — so it is this step's, and the
   measured worst case is in the table below.

Also waiting, and not urgent: the unbounded input line above, which E2's CSA
client is the reason to bound and the phase that can choose the bound.

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

## Where the sparring opposition is

GPL binaries live only in the local-only `../benchmarks` repository and are only
ever *run* as separate processes (CLAUDE.md §2, run-vs-link). What is built and
runnable there today:

| Engine | Path | Note |
|---|---|---|
| YaneuraOu | `YaneuraOu/source/YaneuraOu-by-gcc` | **MaterialLv1** — no NNUE, but it honours `NodesLimit`, which is what an E0 ladder needs |
| Apery | `apery_rust/target/release/apery` | also material-only (no eval files present) |
| Fairy-Stockfish | `Fairy-Stockfish/src/stockfish` | USI dialect |

**Not present anywhere**: Lesserkai, 技巧2, shogi-server, Ayane, and any
floodgate CSA archive. Step 6 has to download the game records; step 7 has to
fetch Ayane (Apache-2.0) and vendor it.
