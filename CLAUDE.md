# CLAUDE.md — rinsai project instructions

Rules an implementation session must follow. The plan is [DESIGN.md](./DESIGN.md),
what is frozen is [CONVENTIONS.md](./CONVENTIONS.md), why is
[FAQ.md](./FAQ.md). **What is next, deferred or broken is a GitHub
issue** — `gh issue list`. What landed is `git log` and the pull requests, and
**what the games said is [STRENGTH.md](./STRENGTH.md)** — the one record that
cannot be re-derived from a checkout.

## 1. What rinsai is

A Rust **shogi engine** — search, evaluation and the protocols to play real
games — aiming at floodgate and the 世界コンピュータ将棋選手権 (WCSC). Built on
[`shunsai`](https://github.com/sugyan/shunsai) for move generation and on
`shogi_core` for the fundamental types. Route: **NNUE + αβ first**; DL/MCTS is
deferred behind the conditions written in DESIGN.md's E6, not rejected. Work is
staged **E0–E6**; know which phase a task belongs to before starting it.

**In scope**: search, evaluation, NNUE inference and training, USI/CSA, time
management, 千日手, 入玉宣言, self-play, the match harness.

**Not in scope, and do not reimplement**: move generation and position
management (shunsai), SFEN/USI parsing (`shogi_usi_parser`), mate solving
([`tsumeshogi-solver`](https://github.com/sugyan/tsumeshogi-solver)).

## 2. ⚠️ Top rule: licensing — stay permissive, no GPL reuse

The licence is **`MIT OR Apache-2.0`**, for the engine *and* the training
pipeline.

**May reuse, retaining the copyright notices.** MIT:
[haitaka](https://github.com/tofutofu/haitaka),
[cozy-chess](https://github.com/analog-hors/cozy-chess), the `shogi_core`
family, `shogi_usi_parser`, the `usi` and `csa` crates. Apache-2.0:
[Ayane](https://github.com/yaneurao/Ayane). Public write-ups: CPW, the NNUE and
AlphaZero-family papers, the Qugiy appeal document.

**Must not copy or port.** YaneuraOu, dlshogi, tanuki-, cshogi, 技巧2,
shogi-server, Stockfish, Fairy-Stockfish, and the old
[yasai](https://github.com/sugyan/yasai) — sugyan's own work, but GPL-3.0 and
derived from apery_rust, so porting from it is forbidden too. Understanding a
technique and **writing it yourself** is fine; read-and-copy inherits GPL.

⚠️ **Run-vs-link is what makes this project practical.** *Running* a GPL program
as a separate process creates no obligation — nothing GPL is linked or
distributed — so sparring ladders, CSA bridges and local match servers are all
available. GPL checkouts and binaries live **only** in the local-only,
unpublished `../benchmarks` repository, never here. The harness spawns
processes and nothing more; opponent paths come from a gitignored local config
(`tools/opponents.toml`, with an `.example` committed).

**Training is written from scratch in PyTorch.** YaneuraOu's learner,
nnue-pytorch and tanuki- are GPL — read them for ideas, never port. The data
format is in-house and deliberately not PackedSfen-compatible. **Generate
tables and books with our own generators**; never paste from elsewhere. Run a
**provenance scan before any release**, over the engine and the trainer alike.

## 3. The measurement loop

Nothing is adopted on argument: `patch → bench → fixed-node paired games → SPRT
→ merge`.

- **`bench`** (fixed positions × fixed depth) is the search analogue of perft. A
  patch that moves a node count unintentionally is a bug, not an improvement.
- **SPRT**: gains at elo0=0 / elo1=5; non-regression gates at elo0=−5 / elo1=0;
  α=β=0.05. **Paired openings with colours swapped are mandatory.** Feature
  patches run at fixed nodes; speed and time-management patches run in real time.
- **One feature = one SPRT.** Do not bundle. ⚠️ This governs **strength
  patches** — changes whose intended effect is Elo. Correctness and
  infrastructure work is gated by its own deterministic suites (scenario,
  conformance, parity, `bench`) and may land batched.
- **Record the rejected numbers too.** A measured loss is a result worth
  keeping. Write a measurement **once**, beside the thing it justifies — and
  when the thing it justifies is a patch rather than a line of code, that
  beside is [STRENGTH.md](./STRENGTH.md), which carries the admission test.
- **Timing needs a quiet machine.** A fixed-node game between deterministic
  engines is decided by the opening and the budgets, so that queue may run
  beside other work. Timing measurements and real-time SPRTs may not.
- **Never run engine matches above `--concurrency 3`** on the development
  machine — each worker is two engine processes at full CPU. Say what will be
  spawned before launching a fleet, and give every long-running process a bound.

## 4. Prose

**Write less.** No compiler and no test reads these sentences, so the lever is
volume, not care. A review that found eleven false claims in one step's prose
found zero defects in its code.

- A comment says **what the item is and what a caller must guarantee**, or
  **what breaks silently** (mark it ⚠️). Nothing else.
- **Do not write a number, a history, a plan, or a description of code
  elsewhere.** A number goes in a constant or an assertion, or — when nothing
  executable can hold it, which is true of a result that took games to produce —
  in [STRENGTH.md](./STRENGTH.md); history goes in the pull request; a plan goes
  in an issue; other code gets a pointer or nothing.
- **⚠️ Never reference a repository document from an artifact** — source,
  manifest, generated file or CI. It does not resolve in `cargo doc` and it
  ships with a published crate. State the rule where it is needed, or not at all.
- **A false claim is deleted, not rewritten.** Try in order: delete the
  sentence; replace it with a pointer; write the test that checks it.
- **One copy or none.** A fact worth stating twice was worth stating once.
- A test's doc says what the test would catch. A sabotage note may only state
  the mutation actually made and the test that actually went red.

## 5. Correctness baseline

- **Fixed-depth node counts** on the committed position set, as a regression test.
- **Mate suites**, mate-in-1 through 5.
- **千日手 and 連続王手** are engine-side — shunsai holds no game history by
  design. Stack `(key(), Hand, in_check())` per ply; `key()` filters, hand
  equality confirms, and the `in_check()` history decides perpetual check, where
  the checking side loses. Scenario tests are mandatory, from E0.
- **入玉宣言 (27-point rule)**: scenario tests, from E2.
- **USI conformance dialogues** — the protocol loop is our own code.
- Move generation correctness is **shunsai's**, held there by differential
  testing against `shogi_legality_lite`. Do not re-test it here.

**Writing one.**

- **No test may name a move the engine chose** — shunsai's generation order is
  unspecified. Assert "this is legal here" by replaying the move into a position
  the test builds itself.
- **A test of an optimisation has to run on input the optimisation helps.** "It
  is the obvious fixture" is not evidence that it does. Where the fixture is
  what makes the test able to fail, say so in its doc and assert the property it
  was chosen for, so the day it stops holding the test says so.
- **A sabotage note is written only after the mutation was made at that site and
  that test went red**, and it may state what that run showed and no more. ⚠️
  "In either run", "in both loops", "every row but the first" are
  generalisations past the mutation applied, and each has shipped as a false
  note. If a neighbouring site was not tried, try it or say so. **After a
  change, re-run the sabotages in the files the diff touches**; full-tree sweeps
  are for phase gates.
- **A surface with no caller stays only if a *specific* caller can be named, and
  the name goes in its doc comment.** Otherwise it goes.
- **A module with children is `foo.rs` beside `foo/`, never `foo/mod.rs`** — the
  family layout, shunsai's too.

## 6. Depending on shunsai, and the other consumer

- rinsai depends on a **released version** (`shunsai = "0.1"`), never a git pin.
  **No SPRT number may be attributed to a rev that is not a release.**
- To add an API: prototype on a shunsai branch → measure it on shunsai's own
  bench → adopt → **release shunsai** → raise the requirement here. Never work
  around a missing API with a slow local reimplementation without saying so.
  Several additions unlock a re-measurement shunsai parked for this consumer —
  see DESIGN.md's API catalogue, and say which one you are unlocking.
- **`rinsai-game` has a second consumer**,
  [tuishogi](https://github.com/sugyan/tuishogi), which depends on it by git
  rev. ⚠️ An issue raised "from the other consumer" is only answerable by
  reading tuishogi's actual call sites — the issue text has been wrong about
  which API shape the consumer can use.

## 7. What runs where

The development machine is an Apple Silicon Mac; sessions also run in the cloud,
where the checkout is all there is.

**Available anywhere**: `cargo test` / `clippy` / `doc`, and `rinsai bench` —
its node counts are deterministic, so they are a valid result from any machine.

**Local only, and a cloud session must not claim otherwise**:
- any timing measurement, and any real-time SPRT — a shared cloud runner is not
  a quiet machine;
- sparring and SPRT against GPL engines, which live in `../benchmarks`;
- reading the sibling shunsai or tuishogi checkouts. shunsai arrives as a
  crates.io dependency, and ⚠️ **the published crate ships `src/` only** — its
  `benches/` and `examples/` are in the repository and nowhere in the vendored
  source.
