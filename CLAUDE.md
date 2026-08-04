# CLAUDE.md — rinsai project instructions

This file defines project rules that every implementation session (Claude Code) **must follow**. For background and detail, see [DESIGN.md](./DESIGN.md).

## Project overview

`rinsai` (凛彩) is a Rust **shogi engine** — search, evaluation, and the protocols to play real games. The goal is to place well on **floodgate** and in the **世界コンピュータ将棋選手権 (WCSC)**. It is built on [`shunsai`](https://github.com/sugyan/shunsai) for legal move generation and position management, and on [`shogi_core`](https://github.com/rust-shogi-crates/shogi_core) (MIT) for the fundamental types.

- **Route**: **NNUE + αβ first**; DL/MCTS is a conditional later option, not a rejected one. The conditions are written down in DESIGN.md E6 — do not start DL work without checking them.
- **Scope**: search, evaluation, NNUE inference and training, USI/CSA, time management, repetition (千日手), declaration (入玉宣言), self-play, the match harness. **Move generation is not in scope** — it belongs to shunsai. Neither is SFEN parsing (`shogi_usi_parser`) or mate solving (`tsumeshogi-solver`).
- **Phases**: staged **E0–E6** (numbered so as not to collide with shunsai's M0–M7). See DESIGN.md §5. Know which phase a piece of work belongs to before starting it.

## ⚠️ Top rule: licensing (stay permissive, no GPL reuse)

The project licence is **`MIT OR Apache-2.0`**, for the engine **and** the training pipeline. Obey the following when generating code.

**May reference / reuse (permissive)**
- MIT: [haitaka](https://github.com/tofutofu/haitaka), [cozy-chess](https://github.com/analog-hors/cozy-chess), the [`shogi_core`](https://github.com/rust-shogi-crates/shogi_core) family, `shogi_usi_parser`, the `usi` and `csa` crates
- Apache-2.0: [Ayane](https://github.com/yaneurao/Ayane) (the USI match runner) — vendoring and modifying it is allowed
- Public write-ups: CPW, the NNUE papers, the AlphaZero-family papers, the Qugiy appeal document
- When reusing from permissive sources, **retain the copyright notices**

**Must not reference / copy (GPL)**
- **YaneuraOu / dlshogi / tanuki- / cshogi / 技巧2 / shogi-server / Stockfish / Fairy-Stockfish / the old yasai**
- Understanding a technique and **writing it yourself** is fine. **Do not read-and-copy or port** — that inherits GPL.
- ⚠️ The old [yasai](https://github.com/sugyan/yasai) is sugyan's own work but is **GPL-3.0** (derived from apery_rust). Porting from it is **also forbidden**.

**⚠️ run-vs-link — the distinction that makes this project practical**
- **Running** a GPL program as a separate process creates **no obligation here**: nothing GPL is linked, nothing GPL is distributed. Sparring ladders, CSA bridges and local match servers are therefore all available.
- GPL checkouts and binaries live **only** in the local-only, unpublished `../benchmarks` repository — **never in this one**. The harness here spawns processes and nothing more; opponent paths come from a gitignored local config (`.example` is committed).
- **There is no major permissive reference for search code** (the strong chess engines are GPL too). Search is written from CPW, papers and first principles. Accept the cost; do not shortcut it.

**Other**
- **Training is written from scratch in PyTorch.** YaneuraOu's learner, nnue-pytorch and tanuki- are GPL — read them for ideas, never port. The data format is in-house and deliberately not PackedSfen-compatible.
- **Generate tables and books with our own generators** — never paste from elsewhere.
- Run a **provenance scan before any release**, applied to the engine *and* the trainer. Trained networks must be demonstrably the product of own data and own trainer via the manifest chain.

## The measurement loop — this is the project's spine

Nothing is adopted on argument. `patch → bench → fixed-node paired games → SPRT → merge`.

- **`bench`** (fixed positions × fixed depth) is the search analogue of perft: node-count agreement is a regression test, and a patch that changes node counts unintentionally is a bug, not an improvement.
- **SPRT**: gain tests at elo0=0 / elo1=5; non-regression gates at elo0=-5 / elo1=0; α=β=0.05. **Paired openings with colours swapped are mandatory.** Feature patches run at fixed nodes; speed and time-management patches run in real time.
- **One feature = one SPRT.** Do not bundle.
- **Record the rejected numbers too.** This is shunsai's habit and it carries over: a measured loss is a result worth keeping, in the DESIGN.md decision log.
- Benchmarking needs a quiet machine. A noisy run is not a result.

## Correctness baseline

- **Fixed-depth node counts** on a committed position set, as a regression test.
- **Mate suites** (mate-in-1 through 5).
- **Repetition and perpetual check**: 千日手 is engine-side — shunsai holds no game history by design. Stack `(key(), Hand, in_check())` per ply; `key()` filters, hand equality confirms, and the `in_check()` history decides the perpetual-check case (the checking side loses). Scenario tests are mandatory, from E0.
- **Declaration (入玉宣言, 27-point rule)**: scenario tests, from E2.
- **USI conformance dialogue tests** — the protocol loop is our own code, so it needs its own tests.
- Move generation correctness is **shunsai's** responsibility, held there by differential testing against `shogi_legality_lite`. Do not re-test it here.

## Depending on shunsai

- rinsai depends on a **released version** (`shunsai = "0.1"`), not a git pin.
- To add an API: prototype on a shunsai branch → **measure it on shunsai's own bench** → adopt → **release shunsai** → raise the requirement here. Never work around a missing API with a slow local reimplementation without saying so.
- Several planned additions unlock a re-measurement that shunsai's decision log deliberately parked for this consumer — see DESIGN.md §6. When adding one, say which re-measurement it unlocks.
- **E0 requires no shunsai change at all**, deliberately: it is the layering's field test.
