# rinsai — progress ledger

Where the implementation is, what the next step is, and what a session needs to
know before touching the code. [DESIGN.md](./DESIGN.md) is the plan,
[CLAUDE.md](./CLAUDE.md) is the rules, [CONVENTIONS.md](./CONVENTIONS.md) is
what is frozen and [DECISIONS.md](./DECISIONS.md) is why; **this file is the
state, and the only home for a measured number.** It is not the story:
`## What step N delivered` is deleted when step N+1 merges, so at most one
exists at a time — the step in flight. What survives a step is a *number*, and
a number survives as a row in the tables below beside the fixture it belongs
to, not as a section. A section title with a past-tense verb in it is the
smell.

## E0 — baseline: play legal moves and don't hang pieces

E0 is split into sub-steps, one per pull request — except step 3, which became
3a/3b, and steps 5–7, which land as two with the harness first (DECISIONS.md,
2026-08-12).

| # | Step | Status |
|---|---|---|
| 1 | Skeleton + USI shell — workspace, CI, shunsai wiring, protocol loop, `bestmove` = a legal move | **done** |
| 2 | Material evaluation + iterative-deepening negamax αβ — PV, `info` output, mate scoring | **done** |
| 3a | Quiescence search — captures only, `seldepth` | **done** |
| 3b | TT + transposition-move ordering — `hashfull`, `USI_Hash`, `rinsai bench` | **done** |
| 4 | Repetition (千日手) + 連続王手 — history *queries*, mate-in-1..5 suite | **done** |
| 5 | Time management — byoyomi / Fischer / movetime, `stop` responsiveness | next — the last E0 step |
| 6 | `openings-v1` extractor — floodgate CSA → balanced opening lines (`crates/xtask` arrives here) | **done** |
| 7 | Match harness + SPRT — own Rust harness refereeing on `crates/rinsai-game`, `tools/opponents.toml.example` | **done** |
| — | `openings-v2` — the balance score read from an iteration that finished, plus `completed_depth` on `BestMove` | **done** |

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

- **A hand count no shogi set contains** (`position sfen … b 19P 1`). It parses,
  and the nineteenth piece is an out-of-bounds panic **on the protocol thread**.
  `Game::from_partial` bounds the total per kind, board and hand together.
- **A non-UTF-8 byte** — a CP932 path from a Japanese Windows GUI, and E3 asks
  for an `EvalFile` path. It reached the loop as an I/O error and the engine
  exited silently with status 0. Lines are read as bytes and converted lossily.
- **An out-of-range SFEN move number.** `PartialPosition::from_usi` saturates and
  discards rather than rejecting. Validated in `split_root` instead.
- **Two kings of one colour** (`position sfen 4k4/…/3KK4 b - 1`). The king is the
  one kind that never changes sides, so its bound is **per colour**, and it is
  *at most* one rather than exactly one: a 詰将棋 diagram routinely omits the
  attacking king, and shunsai's `king_square` returns `Option` with `None`
  documented as legal.

### Still unbounded: the length of one line

`usi::run` reads with `read_until(b'\n')` and no cap, so a peer that never sends
a newline grows the buffer until the process dies. Harmless at E0 — the peer is
a local GUI — but **E2's CSA client takes its input from a network socket**, and
that is the phase to bound it in: the bound wants choosing against the longest
line a CSA server really sends, not guessing now.

## Deliberately not built, each with its owner

- **Time-based games.** The harness sends `go nodes` and nothing clock-shaped;
  its only clocks are hang detectors, which can turn a wedged engine into a
  recorded loss but never change a move. The wire margin is step 5's whole
  subject (CONVENTIONS.md, Time control).
- **The first real SPRT** — finished E0 against the step-3b engine at
  elo0=−5/elo1=0 — runs when step 5 completes E0, as the instrument that
  retroactively audits both batch PRs.
- **`fetch-net` / `release` subcommands** — later phases (DESIGN.md §4).
- **`rinsai-game` publication and tuishogi's adoption of it** — deferred until
  the API has its second consumer in hand (DECISIONS.md, 2026-08-12).
- **`Ply::kifu` computed on demand.** It costs ~95% of `Game::play` — 79 µs
  per ply against 3.9 µs without it, because the notation library brute-forces
  every candidate move to pick a disambiguation character — and the harness
  never reads it. Making it lazy is an API change to a crate whose second
  consumer is tuishogi, which *does* read it, so it goes with the publication
  above rather than ahead of it.
- **Killing an engine's whole process group, and deadlines on writes to a
  child.** `Drop` sends `SIGKILL` to the child it spawned, which is the engine
  itself for every binary in the roster; a launcher *script* that does not
  `exec` would leave the real engine behind. And a child that stops draining
  its stdin can block a write with no timeout. Both are latent — nothing in
  the roster is a script, and no engine has wedged that way — and both want
  care rather than a quick patch.
- **Unused `pub` surfaces on `rinsai-game`** (`from_position`, `last_move`,
  `check_squares`, `destinations`, `drop_destinations`, `promotion`, `undo`,
  `resign`). CONVENTIONS.md's named-caller rule asks for a specific caller in
  each doc; tuishogi is the caller for all of them and is a plan until the
  crate publishes. Either name its screens or delete them, at publication.

## Measurements the conventions rest on

[CONVENTIONS.md](./CONVENTIONS.md) carries the rules these produced and
[DECISIONS.md](./DECISIONS.md) the arguments; **the numbers live here and only
here.** Release build, one quiet machine: node counts are deterministic and
reproduce anywhere, times are that machine's.


### Search cost, and what moves a node count

Release build, one quiet machine. Step 2's column was re-run untouched at the
start of step 3a and reproduced to the digit, which is the check that says the
columns can be compared at all.

| Position | Depth | Nodes, step 2 | Nodes, step 3a | Nodes, step 3b | seldepth, 3b | Time, step 2 |
|---|---|---|---|---|---|---|
| initial | 6 | 585 568 | 376 695 | **128 167** | 29 | 23 ms |
| drop-heavy middlegame | 4 | 838 976 | 7 030 029 | **4 049 636** | 29 | 40 ms |
| drop-heavy middlegame | 5 | 13 888 371 | 378 188 510 | **33 416 692** | 31 | 445 ms |
| two lone kings | 6 | 1 431 | 1 431 | **747** | 6 | — |

⚠️ **The step-3b column is at the advertised default table size (256 MiB).**
Table size moves a node count on its own, so a count quoted without a size is
not a result. Settled, the three time rows sit at 22–23 / 38–41 / 444–469 ms
across two sessions; a time that differs from this table is not by itself a
result.

⚠️ **Exec the binary once after a build, before measuring anything.** The first
run of a freshly linked binary costs about 20–25 ms more than the settled
figure, and it is an *absolute* amount rather than a proportional one — +87% on
the 23 ms row and +5% on the 445 ms one. Two experiments pin it: reversing the
row order after a rebuild moves the penalty to whichever row now goes first,
not to the shortest; and a throwaway `rinsai --version` removes it entirely
(40 / 33 / 38 ms without against 23 / 23 / 24 ms with, three alternating
pairs). What is left is the first *execution* of a newly written binary image.

**Node rate, and why it justifies nothing.** These rows imply 8–10 M nodes/s
against step 2's 20–30 M, and the gap is not a regression: step 2's nodes were
mostly depth-zero leaves that only evaluated. ⚠️ The rate is **not** an argument
for the 1024-node poll interval, which is untuned; step 5's SPRT is where it
stops being inherited.

**Two lone kings did not move between steps 2 and 3a**, byte-identically over
six iterations — the cheapest check that a capture-only quiescence is entered
only where a capture is reachable.

**The depth-parity fingerprint is gone.** Step 2 reported +215 at odd depths
and 0 at even ones from the initial position (depths 1–8: 0, 0, +215, 0, +215,
0, +215, **−215**). After quiescence all eight report 0.
`the_horizon_effect_fingerprint_is_gone` asserts against the recorded
magnitude, so the effect returning fails it whatever depth it returns at. ⚠️ An
upper bound and nothing more: a search reporting ±90 would pass.

### Quiescence: the check cap, and what it costs

⚠️ **The first implementation capped total quiescence plies, and that was
wrong.** Drop-heavy fixture, depth 4, with the total-ply cap:

| cap | nodes, depth 4 | nodes, depth 5 |
|---|---|---|
| 32 | 8 138 236 | 1 485 819 624 |
| 16 | 7 416 958 | — |
| 8 | 4 735 764 | — |
| 4 | 1 870 111 | — |

At cap 4 the floor fired on 22% of quiescence nodes — a cap low enough to cut
the cost is cutting *capture chains* mid-way, which is the horizon effect
reintroduced two plies down. Counting **checks** instead (drop-heavy depth 4 /
initial depth 6 / 頭金 at depth 1):

| checks allowed | nodes, depth 4 | nodes, depth 6 | mate in 1 at depth 1 |
|---|---|---|---|
| 0 | 30 824 360 | 221 351 | **`cp 1260` — wrong** |
| 1 | 22 131 829 | 239 094 | `mate 1` |
| **2** | **7 030 029** | **376 695** | `mate 1` |
| 4 | 7 681 868 | 459 396 | `mate 1` |

⚠️ **Two predictions refuted here.** Zero is not a cheap approximation but
*incorrect* — it evaluates while in check and misses a mate one ply away. And
**the node count is not monotone in the cap**: two checked plies cost a third
of what one costs, because resolving a check returns a score the search above
can cut on. "Search less, spend less" is not safe anywhere near a check.

**What a captures-only quiescence leaves for E1's staged generation** — moves
generated against moves kept, over a whole search:

| Position | Depth | Quiescence share of nodes | Generated | Kept | Ratio |
|---|---|---|---|---|---|
| initial | 6 | 91.5% | 18 655 208 | 385 110 | **48×** |
| drop-heavy middlegame | 4 | 99.3% | 788 675 461 | 9 216 400 | **86×** |

### The transposition table

**The stored move searched first, against the same search without that one
line** — node-count ratios, one MiB, release:

| Position | d=2 | d=3 | d=4 | d=5 |
|---|---|---|---|---|
| initial | 1.00× | 1.00× | 1.01× | 1.00× |
| initial + `7g7f 3c3d` | 1.00× | 1.46× | 1.39× | 1.44× |
| initial + `2g2f 8c8d 2f2e 8d8e` | 1.00× | 1.00× | 1.94× | **14.02×** |
| an open middlegame | 1.00× | 1.53× | 1.57× | 2.13× |
| drop-heavy middlegame | 1.00× | 1.12× | 1.55× | **8.08×** |

⚠️ **The initial position gains nothing at any depth measured**, which is why
`the_transposition_move_is_searched_first` uses another fixture and names it as
load-bearing. The effect is strongly super-linear in depth.

⚠️ **Quiescence probing the table was expected to lose and does not — it halves
the tree.** Measured by patching it in and out; not shipped at 3b, and E1 item
11 owns it. The conventional argument (quiescence is 91–99% of nodes, so it
would thrash the table) is refuted on every row:

| Position | Depth | Nodes, table in interior nodes | Nodes, in quiescence too | Ratio | ms | ms |
|---|---|---|---|---|---|---|
| initial | 6 | 128 167 | 99 267 | 0.77× | 15 | 17 |
| drop-heavy middlegame | 4 | 4 049 636 | 2 036 973 | **0.50×** | 432 | 419 |
| drop-heavy middlegame | 5 | 33 416 692 | 15 649 522 | **0.47×** | 3 379 | 2 926 |

**Allocating the default table costs about as much as an E0-depth search** —
20.6 / 12.1 / 6.3 ms for 256 MiB in one process, against 13–20 ms for a whole
depth-6 search from the initial position. Paid once per searcher, in `main` and
again on `setoption name USI_Hash`. ⚠️ A change that moved allocation onto the
`go` path would look harmless and would not be.

**`hashfull` reads 0, 2, 6, 0 permille at 256 MiB** on the four fixtures above:
real data, but an E0-depth search fills almost none of a quarter-gigabyte
table. ⚠️ Consequence: `hashfull_reports_the_table_filling_up` needs the 1 MiB
table its helper builds — the same assertion at the default is a test that
cannot **pass**.

### `bench`

| | |
|---|---|
| positions | 7 |
| depth | 4 |
| table | 16 MiB, fixed — **not** `USI_Hash` |
| total nodes | 4 133 205 |
| reproducibility | three runs, identical to the digit |
| debug and release | identical counts |
| release, one run | 451 ms, 9.16 M nodes/s — ⚠️ **not a result**: the machine was not certified quiet |

**Step 4 did not move it, and the prediction that it would was wrong** — it
could not have been right, since a depth-4 search from a history of at most
seven plies cannot reach a fourth occurrence:

| | step 3b | step 4 |
|---|---|---|
| total nodes | 4 133 205 | **4 133 205** |
| alternating pairs, ms | 427 / 435 / 432 | 431 / 443 / 430 |
| runs of three, ms | 437 / 434 / 431 | 444 / 435 / 432 |

⚠️ **Not a result, and one of the three alternating pairs goes the other way**
— the `in_check()` per interior `do_move` and the three-comparison scan are
below what this instrument can separate from noise.

### Repetition

**Reading the history costs 1–8%, and the cost falls as the search deepens** —
the same position through its whole 121-ply move list against from its own
SFEN. Node counts identical on every row, so this is the scan and nothing else:

| Depth | Nodes | With a 121-ply history | From a bare SFEN | Cost |
|---|---|---|---|---|
| 4 | 16 300 | 1.58 / 1.60 / 1.61 ms | 1.47 / 1.49 / 1.47 ms | **+8%** |
| 5 | 103 912 | 11.46 / 11.54 / 11.62 ms | 11.14 / 11.20 / 11.24 ms | **+3%** |
| 6 | 2 019 944 | 208.4 / 208.6 / 210.0 ms | 205.4 / 205.8 / 206.7 ms | **+1.5%** |

The shape is the opposite of a per-node overhead's because the path is scanned
at interior nodes only, and quiescence — 91–99% of all nodes — pays nothing.

⚠️ **`bench` cannot see this and is not evidence about it.** Its seven
positions carry histories one to seven plies long. A set with real game
histories would be `bench-v2`, and no consumer is asking for one.

⚠️ **One end-to-end case is not covered: the engine declining the check that
loses.** To discriminate, the repeating check must be strictly best without the
verdict and refused with it — and material cannot supply that, because a check
forces the reply. A drawing alternative would, and that needs two four-fold
repetitions reachable from one position, which one line of history cannot
offer. Left undone rather than written weakly.

### The mate suite

Dev profile. ⚠️ **The estimate that mate-in-4 and mate-in-5 would be too
expensive for `cargo test` was wrong by two orders of magnitude** — depth 9
costs under six milliseconds. The last column is why: the mate is found several
iterations before the depth that nominally contains it, because quiescence
resolves a forced checking sequence and a forced sequence is what alpha-beta
prunes best.

| Mate in | Depth limit | Nodes | Iteration that found it |
|---|---|---|---|
| 1 | 1 | 20 | 1 |
| 2 | 3 | 31 | 1 |
| 3 | 5 | 130 | 2 |
| 4 | 7 | 1 678 | 4 |
| 5 | 9 | **18 924** | 6 |

**A ladder built with golds in White's hand answered `mate 5` where `mate 9`
was intended**, at 8 443 nodes: Black captures whatever is interposed and drops
it straight back, and a gold beside the bare king is mate at once. A pawn there
is 打ち歩詰め and illegal, which is what makes the pawn version hold its length.

### The opening sets

`openings-v1`, and the two regenerations that froze it:

| | |
|---|---|
| source days | 2026-06-01..07 — 2 577 records |
| qualifying games | 1 422 (both rates ≥ 3000, %TORYO/%KACHI, ≥ 60 plies) |
| rejected on replay | **0 of 1 422** — every replayed move (each game through ply 24) legal for `shogi_legality_lite` |
| lines | 256 — ≤ 2 per game, plies 12..=24 outside check, own eval within ±100 cp |
| seed / rev | `0x52494E5341492D31` / the rev in the file's own header |
| regeneration | twice, same cache, `--rev` as recorded — **byte-identical both times** |

⚠️ **v1's `eval=` came from whichever iteration the node cap interrupted, not
from a completed one** — a lower bound over a prefix of the root move list, and
against a two-sided window that errs both ways. `openings-v2` is the correction
(CONVENTIONS.md carries the rule):

| how v1's emitted `eval=` was produced | lines |
|---|---|
| a completed depth-6 search, as its header's wording implies | **24** |
| a completed iteration at depth 2–5 | 82 |
| an iteration cut off at the cap | **150** |

All 256 of v1's lines re-scored at the frozen settings — depth 6, a
2 000 000-node cap, a 16 MiB table, one cleared searcher per line. Completed
depth: 1 line at 2, 48 at 3, 42 at 4, 141 at 5, 24 at 6.

| | |
|---|---|
| lines whose last iteration was interrupted | 150 |
| of those, whose completed score differs from the partial one | **6** |
| of those, whose ±100 cp verdict differs | **1** |

⚠️ **That middle figure is 6 where this file previously recorded 8**, against
the same comparison; both stand, since neither reconciles from the page. The
two passes agree on 150, on 1, and on the line it names.

| | v1 | v2 |
|---|---|---|
| candidates searched to fill the target | 309 | **305** |
| rejected on balance | 53 | 49 |
| lines shared with the other set | — | **252 of 256** |
| regeneration at the recorded rev and seed | byte-identical | **byte-identical, twice** |

The eight lines the sets disagree about — the four gained are the reverse
direction, which v1's prose said could not happen. The three that left with no
verdict change are the lazy pass: four candidates kept sooner ends the walk at
305, so the tail v1 reached is never visited.

| | lines | completed verdict | partial verdict |
|---|---|---|---|
| gained by v2 | **4** | keep — `+15, 0, 0, 0` | **reject** — `−200, −115, −115, −215` |
| lost from v1 | 1 | reject — `+115` | keep — `0` |
| lost from v1 | 3 | keep | keep — no verdict change at all |

⚠️ **The committed fixture corpus cannot tell the two balance rules apart, at
any cap tried** — the interrupted and completed scores are equal on every
candidate it holds, which is why the gate is a second corpus of one game chosen
for a position that diverges (0 on the depth-2 iteration it completes, −1380 on
the depth-3 one a 5 000-node cap interrupts):

| candidates | nominal depth | node caps | divergent |
|---|---|---|---|
| 4 | 3 | 100 … 50 000, nine values | **0** |
| 14 | 4, 5, 6 | 2 000 … 500 000, eight pairs | **0** |

**What the balance searches measured about the engine.** Not a strength claim,
a cost: real floodgate middlegames at plies 12–24 cost **8.8 M to over 40 M
nodes** for a depth-6 search, against 13–20 ms from the initial position. That
is the gap E1's items 2, 8 and 9 exist to close, measured on positions the
engine will actually be asked about.

### The harness's own gates

**The mirror gate.** Fresh processes and fixed nodes make each game
deterministic, so a pair's two games are one game with the seats exchanged —
any deviation from exact symmetry would be a harness bug, not noise. Every pair
scored exactly ½, and the zero-variance guard reported "no evidence" instead of
dividing by zero for the whole run:

| pairs | nodes | score | pentanomial 0/¼/½/¾/1 | candidate W-D-L | LLR |
|---|---|---|---|---|---|
| 200 | 10 000 both | **exactly 50.00%** | 0/0/200/0/0 | 157-86-157 | 0.000 |

**The ladder.** rinsai at 100 000 nodes against the node-limited material
YaneuraOu, 100 pairs per rung from `openings-v1`, seed 1:

| YaneuraOu nodes | rinsai score | candidate W-D-L | pentanomial |
|---|---|---|---|
| 1 000 | **50.00%** | 97-6-97 | 21/4/51/2/22 |
| 10 000 | 2.50% | 4-2-194 | 94/2/4/0/0 |
| 100 000 | 0.00% | 0-0-200 | 100/0/0/0/0 |
| 1 000 000 | 0.00% | 0-0-200 | 100/0/0/0/0 |

**rinsai's E0 search at 100k nodes measures level with YaneuraOu's material
search at 1k** — a 100× node handicap, which E1 exists to close. The measuring
stick saturates from the 10k rung up, so those rows say the rungs are above
rinsai, not how far apart they are. Rung against rung, 50 pairs each, seed 1:

| pairing (nodes) | higher rung's score | W-D-L | pentanomial 0/¼/½/¾/1 |
|---|---|---|---|
| 10 000 vs 1 000 | **99.00%** | 99-0-1 | 0/0/1/0/49 |
| 100 000 vs 10 000 | **100.00%** | 100-0-0 | 0/0/0/0/50 |

⚠️ **The 1 000 000-vs-100 000 pairing is deliberately unmeasured**: it is the
one expensive pairing, and nothing consumes its calibration yet — the E1 ladder
work happens at 1k–10k. Take it overnight at low concurrency the day something
needs it.

### Review and sabotage tallies

**The steps 6+7 review**, over a batch whose own gates all passed:

| | |
|---|---|
| findings reported | 15 |
| confirmed by running the failure, not by reading | 13 |
| defects in the harness's *decision* path | 5 |
| dropped tests, i.e. rules with no remaining guard | 4 |
| false claims in prose | 9 |

| | mutations | fired on the named test | notes that were **false** |
|---|---|---|---|
| steps 6+7 | 6 | 6 | 0 |

**The sabotage re-run at step 4** — every note in the tree, applied at the site
the note names, the whole suite run against each:

| | |
|---|---|
| mutations applied | 59 |
| fired on the test their note names | 55 |
| expected not to fire, and did not | 1 |
| notes that were **false** | 3 |
| notes naming the wrong **symptom** | 1 |

The one expected not to fire is `negamax`'s own `pv[ply].clear()`. This sweep
found a second line of that kind in the same node, `mated_in(ply)`; both are
below.

⚠️ **Four of the sweep's own mutations were defective on the first pass** — two
did not compile, two were no-ops, and a no-op reports GREEN and reads exactly
like a false note (DECISIONS.md, 2026-08-11).

**The sabotage re-run at step 3b**, same method:

| | |
|---|---|
| mutations applied | 39 |
| fired on the test their note names | 34 |
| expected not to fire, and did not | 1 |
| notes that were **false** | 2 |
| behaviour with **no test at all** | 1 |
| pre-existing tests found **hollowed out** | 1 |

The hollowed-out test is `each_iteration_starts_from_the_last_answer`, and what
hollowed it is which fixtures discriminate: **only the drop-heavy middlegame
answers with a move other than the one shunsai generates first, of five
measured**, so on the other four the test says nothing.

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

### The budget and the poll

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

### The documents themselves

**Prose volume**, either side of the 2026-08-09 cleanup: bytes of comment lines
over `crates/**/*.rs` against bytes of non-blank non-comment lines.

| | before | after |
|---|---|---|
| comment bytes | 127 617 | 97 099 |
| code bytes | 138 206 | 138 206 |
| comments as a share of non-blank text | 48.0% | 41.3% |

**What the per-step ritual cost** (2026-08-12). ⚠️ A proxy — operations and
bytes, not minutes — and one pass, not a maintained series.

| | |
|---|---|
| sessions, 2026-08-04 → 08-11 | 14 (85 human turns, 3 789 assistant turns) |
| Edit/Write operations targeting `.md` files | 33% of 530 |
| transcript bytes, review-dedicated worktree vs the implementation worktree it began by reviewing | ~12 MB vs ~6.7 MB |
| `main` commits since code exists that are substantively prose-only | 2 of 6 |

**The consolidation that followed**, at the pull request that gave DECISIONS.md
and PROGRESS.md a stopping condition. ⚠️ The reduction is a one-off; the rule
change beside it is what keeps the slope down, and the slope is what mattered —
over the six pull requests before it, DECISIONS.md grew 8.8 KB each and
PROGRESS.md 7.0 KB, against CONVENTIONS.md's 1.8 KB.

| | before | after |
|---|---|---|
| DECISIONS.md | 108 813 | **46 133** |
| PROGRESS.md | 73 567 | **37 984** |
| the five documents together | 244 299 | **148 025** |
| against Rust, non-blank | 12 599 lines | 12 599 lines |

### Portability and the browser

**⚠️ The wasm target compiles and the binary has no engine in it.** Measured on
rustc 1.94.1, `wasm32-unknown-unknown`, at `1dbf0cd`. `cargo check` for the
target passes unmodified; the release binary does not run:

| | |
|---|---|
| `rinsai.wasm`, release | 89 223 bytes |
| imports | **none** |
| `main()` | traps — `RuntimeError: unreachable` |
| deepest surviving rinsai symbol | `rinsai_search::search::SearchDriver::spawn` |
| absent | `qsearch`, `generate_moves`, `undo_move`, `evaluate`, `legal_moves` |

`thread::Builder::spawn` cannot succeed on this target, so the `expect` on it
always fires and everything downstream — the entire search — is dead code.
**So a green `cargo check` on this target is not evidence of portability**, and
the CI step that runs it is scoped accordingly — see the comment on it.

**NNUE weights against a browser download.** Arithmetic, not measurement:

| Feature transformer | Bytes | |
|---|---|---|
| HalfKP 125 388 × 256, int16 — the E3 plan | 64 198 656 | 61.22 MiB |
| P-only 1 548 × 256, int16 | 792 576 | 774 KiB |

The rest of `halfkp_256x2-32-32` is about 17.5 KB of head — **99.97% of the file
is the feature transformer**, so the feature set is the only dimension that moves
the download. One external figure, attributed rather than measured here:
`@mizarjp/yaneuraou.k-p` ships a playable browser shogi engine with a small net
embedded in about 2.6 MB, which is the order a web build has to reach.

## After `openings-v2` — step 5 is E0's last, and it is two pull requests

⚠️ **Step 5's gate needs an instrument that does not exist.** Its gate is
fifty real-time games with zero flag falls, and **the harness has no notion of
a clock**: it sends `go nodes N` and nothing else, `Seat::bestmove` takes no
clock and returns no elapsed time, no per-game budget is tracked, and
`EndReason`'s only timeout is the hang detector — classified *abnormal*, so a
real flag fall would be reported as a degraded run rather than a loss. So step
5 lands as two pull requests, harness first, on the reasoning that put steps
6+7 ahead of it:

1. **The harness's byoyomi time control**, plus a normal-loss flag-fall
   result. Verifiable with the engine untouched, since rinsai already honours
   `byoyomi`. ⚠️ **The steps 6+7 mirror gate does not carry over**: a
   real-time game is not deterministic, so the exactly-50.00% check becomes a
   statistical one plus unit tests on the clock bookkeeping.
2. **The engine's time management** — the section below — with a gate that can
   be run.

The constants E0 inherited untuned — the poll interval, the allowance shape —
get their SPRTs immediately after, as real-time patches on a quiet machine,
and the harness's first real act is the **non-regression SPRT of the finished
E0 against the step-3b engine** (elo0=−5, elo1=0).

Then the E0 exit criterion above, unchanged: the shunsai v0.1.0 release. No
SPRT number may be attributed to a git rev that is not a release (CLAUDE.md
§6), so the release gates the first E1 ledger entry, not just "E0 done".

## Step 5 — what it has to get right

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
   measured worst case is in the table above.

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

**Not present anywhere**: Lesserkai, 技巧2, shogi-server. The floodgate
records behind both opening sets live in the gitignored `data/floodgate/` cache
(`cargo run -p xtask -- fetch-floodgate` refills it; re-runs cost nothing).
Ayane is not used — the harness is `crates/xtask`, own code (DECISIONS.md,
2026-08-12). Engine paths go in `tools/opponents.toml`, copied from the
committed `.example` — absolute paths only, the worktree trap again.
