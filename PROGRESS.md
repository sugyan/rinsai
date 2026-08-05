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
| 2 | Material evaluation + iterative-deepening negamax αβ — PV, `info` output, mate scoring | next |
| 3 | TT + quiescence search — `rinsai bench`, fixed-depth node-count regression | |
| 4 | Repetition (千日手) + 連続王手 — history *queries*, mate-in-1..5 suite | |
| 5 | Time management — byoyomi / Fischer / movetime, `stop` responsiveness | |
| 6 | `openings-v1` extractor — floodgate CSA → balanced opening SFENs (`crates/xtask` arrives here) | |
| 7 | Match harness + SPRT — Ayane vendored, `tools/opponents.toml.example` | |

### E0 exit criteria not yet met

- **`TODO(shunsai-0.1-release)`** — the dependency is a git rev, which DESIGN.md
  §2 and shunsai's own DESIGN.md both say it should not be. E0 cannot be
  declared finished with this outstanding. `git grep 'TODO(shunsai-0.1-release)'`
  finds all four places that change together.

## What step 1 delivered

`rinsai` starts, holds a USI conversation, and plays legal games. Verified
against the local material-evaluation YaneuraOu (`../benchmarks`, `NodesLimit
10000`): 22 plies to a resignation, no illegal move, no protocol stall, no
diagnostics. It is extremely weak on purpose — there is no search.

- `crates/rinsai-search` — `Game` (board + lockstep SFEN + history), `is_legal`,
  `Score`/`Depth`, the `Searcher` seam (`SearchJob`, `Limits`, `SearchSignals`,
  `BestMove`, `InfoSink`, `SearchDriver`), and `PlaceholderSearcher`.
- `crates/rinsai` — the USI protocol loop, its state machine, the option table
  and the single output sink.
- CI: fmt, clippy `-D warnings`, tests, an MSRV job on 1.88, `cargo deny`, and
  two guards described below.

**Deliberately not built**, so step 2 does not inherit guesses: no evaluation,
no search, no move buffer, no transposition table, no repetition *queries* (the
history is recorded, nothing reads it), no time management, no `bench`.

## Step 2 — the first three things to do

1. **Material evaluation** behind `Score`, in centipawns. The sign convention is
   already fixed: negamax, positive = good for the side to move. Keep a naive
   from-scratch evaluator as a permanent oracle (shunsai's habit) even after an
   incremental one arrives.
2. **A move buffer.** Step 1 has none, because until recursion exists there is
   no caller. The analysis is below — do not re-derive it, and do not adopt
   `ArrayVec` without re-reading it.
3. **Iterative-deepening negamax** implementing `Searcher`, replacing
   `PlaceholderSearcher` (delete `placeholder.rs`). Fill `SearchInfo`-style
   `info` lines through the existing `InfoSink`. **Nothing in `crates/rinsai`
   should need to change** — if it does, step 1's layering was wrong, and that
   is worth recording.

Then: `cargo run --release -- bench` is step 3, and the node counts it freezes
depend on the convention below.

## Conventions frozen in step 1

Changing any of these needs a DESIGN.md §9 entry, because code already assumes
them.

- **A bare `Position` means `shunsai::Position`.** `shogi_core::Position` is
  never imported — we replay moves ourselves, so it is never needed.
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
- **No test may name a move the engine chose.** shunsai's DESIGN.md:64 refuses
  to guarantee generation order. Assert "this is legal here" by replaying the
  move into a position the test builds itself.

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
- **`shogi_core::Position::from_usi` must not be used.** It parses
  `startpos moves …` in one call and *silently swallows* moves it cannot make
  (`position.rs:67-72`), returning `Ok` with a truncated position.
- **Never enable a shunsai feature.** `slider-naive` wins its backend priority
  order and is 5–8× slower; `bench-internals` is documented "never enable as a
  dependency". CI greps every manifest for `"shunsai/…"`, which is also what
  makes `--all-features` safe to run.
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

### The move buffer — decide at step 2, with the first real caller

A **shared, ply-threaded `Vec<Move>`**, reserved once at
`MAX_LEGAL_MOVES * MAX_PLY` (593 × 3 B × 128 ≈ **222 KiB per thread**, one
allocation), sliced per ply: generation appends, the caller records the base and
truncates back to it on the way out. This is shunsai's own `perft_materialize`
shape (`examples/perft.rs:71`).

Why not the alternatives:

- **`ArrayVec<Move, 593>` per ply on the stack** — same 1 779 B but *per node*.
  `Move` has no `Default`, so an `unsafe`-free version needs a dummy fill: a
  1 779-byte memset per node, which at 10 M nodes/s is ~17 GB/s of pure memset.
  Avoiding it means `MaybeUninit`, and the workspace denies `unsafe_code`.
- **`Vec<Move>` per ply** — one malloc/free per node.
- **A ply-indexed array inside the search stack** — attractive, and it is where
  E3's accumulator stack goes, but it makes the move list borrowed while
  recursing on `&mut stack`. Keeping the buffer a separate `&mut` argument means
  the two never alias.

At E1 the element becomes scored — either a parallel score array indexed
identically, or a `ScoredMove`. Having a domain type rather than `ArrayVec` is
what makes that change local.

### `[patch.crates-io]` for an unpublished crate — checked, and rejected anyway

It **does** work: cargo's own testsuite covers it
(`tests/testsuite/patch.rs::nonexistent`), and the Cargo book says sources can
be patched with versions of crates that do not exist. Rejected on three grounds:
`shunsai = "0.1"` in the manifest would assert something false; `[patch]` is
honoured only at the workspace root, so `rinsai-search/Cargo.toml` would stop
saying where shunsai comes from; and a patch would keep silently overriding the
real crate once it is published, so the switch we are trying not to forget would
never go red. Recorded so it is not re-opened.

### A `quit` watchdog — not yet

Nothing can hang on `quit` at step 1: the placeholder always returns promptly.
When a real search lands (step 2), `shutdown` should gain a **bounded** wait
rather than a `process::exit` watchdog — an unconditional exit would make a
hung in-process conformance test *pass*.

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
