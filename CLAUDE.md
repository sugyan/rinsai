# CLAUDE.md — rinsai project instructions

This file defines project rules that every implementation session (Claude Code) **must follow**. For background and detail, see [DESIGN.md](./DESIGN.md).

## 1. Project overview

`rinsai` (凛彩) is a Rust **shogi engine** — search, evaluation, and the protocols to play real games. The goal is to place well on **floodgate** and in the **世界コンピュータ将棋選手権 (WCSC)**. It is built on [`shunsai`](https://github.com/sugyan/shunsai) for legal move generation and position management, and on [`shogi_core`](https://github.com/rust-shogi-crates/shogi_core) (MIT) for the fundamental types.

- **Route**: **NNUE + αβ first**; DL/MCTS is a conditional later option, not a rejected one. The conditions are written down in DESIGN.md E6 — do not start DL work without checking them.
- **Scope**: search, evaluation, NNUE inference and training, USI/CSA, time management, repetition (千日手), declaration (入玉宣言), self-play, the match harness. **Move generation is not in scope** — it belongs to shunsai. Neither is SFEN parsing (`shogi_usi_parser`) or mate solving (`tsumeshogi-solver`).
- **Phases**: staged **E0–E6** (numbered so as not to collide with shunsai's M0–M7). See DESIGN.md §5. Know which phase a piece of work belongs to before starting it.
- **Read [PROGRESS.md](./PROGRESS.md) first.** It is the ledger: which sub-step is next, the gates, the numbers, and the surveyed shunsai API and its traps. Update it at the end of every pull request — which includes *deleting* the previous step's section. A step's narrative lives in its pull request description and is never copied back.

**Each document has one job, and nothing is written in two of them.**

| File | Holds | Shape |
|---|---|---|
| [DESIGN.md](./DESIGN.md) | the plan — goal, scope, route, roadmap E0–E6 | forward-looking; edited when the plan changes |
| CLAUDE.md | the rules a session must follow | this file |
| [CONVENTIONS.md](./CONVENTIONS.md) | what is frozen and already built on, by subject | the rule, not its argument; adding one retires a DECISIONS.md entry |
| [DECISIONS.md](./DECISIONS.md) | what was decided, what was rejected and why it lost, and what would reopen it | one entry per decision, **live** until the rule freezes, then **retired in place** to a line. Conclusions are superseded, never revised |
| [PROGRESS.md](./PROGRESS.md) | the state, the next action, **and every measured number** | present tense only; `## What step N delivered` is deleted when step N+1 merges |
| — how something was found, which review caught it, what a draft said, the order events happened in | **the pull request description**, and nowhere in the repository | `git log` is the index |

**One test, before writing any of it.** *Would a session holding this checkout,
`git log` and the pull request descriptions get something **wrong** without this
paragraph?* If it would only be less informed about how the work went, that is
history, and history is already stored — do not write it. ⚠️ **The volume is
mine to keep down; it is not a thing to measure or gate on.** The five documents
above reached 250 KB against ~9 500 lines of Rust before this rule existed,
because two of them had no stopping condition written into their shape.

## 2. ⚠️ Top rule: licensing (stay permissive, no GPL reuse)

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

## 3. The measurement loop — this is the project's spine

Nothing is adopted on argument. `patch → bench → fixed-node paired games → SPRT → merge`.

- **`bench`** (fixed positions × fixed depth) is the search analogue of perft: node-count agreement is a regression test, and a patch that changes node counts unintentionally is a bug, not an improvement.
- **SPRT**: gain tests at elo0=0 / elo1=5; non-regression gates at elo0=-5 / elo1=0; α=β=0.05. **Paired openings with colours swapped are mandatory.** Feature patches run at fixed nodes; speed and time-management patches run in real time.
- **One feature = one SPRT.** Do not bundle. ⚠️ The rule governs **strength patches** — changes whose intended effect is Elo. Correctness and infrastructure work is gated by its own deterministic suites (scenario, conformance, parity, `bench`) and may land batched; DECISIONS.md (2026-08-12) draws the line.
- **Record the rejected numbers too.** This is shunsai's habit and it carries over: a measured loss is a result worth keeping. **The number goes in a PROGRESS.md table; the decision it supports goes in DECISIONS.md and points at the table.** Never both — two copies of a measurement drift, and this project has already caught one pair 2× apart and a second pair that had quietly diverged in the docs.
- Benchmarking needs a quiet machine. A noisy run is not a result. ⚠️ This binds **times and real-time games**: a fixed-node game between deterministic engines is decided by the opening and the budgets, not by load, so the fixed-node SPRT queue may run beside other work — timing measurements and real-time SPRTs may not.

## 4. Prose has an address

A doc comment answers three questions and no others: **what is this**, **how do I use it correctly**, **what will bite me**. Everything else has an address, and it is not the source file.

| The sentence is… | Where it goes |
|---|---|
| the contract — what the item is, what a caller must guarantee | the doc comment |
| a trap — a wrong use that compiles and stays silent | the doc comment, marked ⚠️ |
| provenance — clean-room evidence, or an upstream citation with `file:line` | the doc comment |
| a number | a PROGRESS.md table, and nowhere else |
| what was tried, measured, rejected, or predicted wrongly | DECISIONS.md |
| a rule something is already built on | CONVENTIONS.md |
| what a later step will do | PROGRESS.md's next-step section, or DESIGN.md's roadmap |
| a restatement of the signature | deleted; it moves nowhere |

**Why volume is the lever, and not care.** Three review passes established the same finding at increasing strength: a *measurement* in a comment goes stale silently; an *invariant* goes stale faster — six of them were false on the day they were written, against code in the same commit; and one step's review found **eleven false claims in its prose and zero defects in its code**. `cargo doc -D warnings` sees none of this and no CI job can, because an invariant is prose. The instrument that works is reading every claim in a diff against the code beneath it, and it has found new false claims every time it has been run. **A claim that is not written cannot be false.** That is the only instrument that scales, so the rule is to write less, not to check harder.

Consequences worth stating outright:

- **A comment that narrates history is a pull request description in the wrong file.** "It used to be X", "this was re-measured", "the note said otherwise" — none of that is a decision, and routing it to DECISIONS.md is how that file reached 113 KB in five days. Only the *conclusion* is an entry: do not do X, because Y was measured. ⚠️ The one exception is a prediction measurement **refuted** — that keeps a live entry, because losing it costs somebody the experiment again.
- **A forward reference stays only if it is load-bearing.** Naming the caller that keeps an unused surface alive (CONVENTIONS.md's named-caller rule) is load-bearing and fits on one line. "E1 will add killers here" is not; it belongs in the roadmap.
- **A test's doc says what the test would catch, not what it caught last time.** A sabotage note is the deliberate exception to "the argument lives elsewhere": it has to sit on the test it describes.
- **Do not write a comment correcting an earlier version of itself.** Fix the comment; the correction, if it is interesting, is a DECISIONS.md entry.
- **A doc comment may only assert things about the item it documents.** Anything about code elsewhere is a pointer or it is nothing — never a restatement, however short and however true it looks. This is the rule the table's rows are instances of, and it is the one that predicts where the false claims are: every claim step 4's review refuted was about something the writer could not check from the same screen — a store a hundred lines down, another file, a test run not yet made, a sum. Not one contract sentence, whose subject *was* the item, was wrong.

## 5. Correctness baseline

- **Fixed-depth node counts** on a committed position set, as a regression test.
- **Mate suites** (mate-in-1 through 5).
- **Repetition and perpetual check**: 千日手 is engine-side — shunsai holds no game history by design. Stack `(key(), Hand, in_check())` per ply; `key()` filters, hand equality confirms, and the `in_check()` history decides the perpetual-check case (the checking side loses). Scenario tests are mandatory, from E0.
- **Declaration (入玉宣言, 27-point rule)**: scenario tests, from E2.
- **USI conformance dialogue tests** — the protocol loop is our own code, so it needs its own tests.
- Move generation correctness is **shunsai's** responsibility, held there by differential testing against `shogi_legality_lite`. Do not re-test it here.

## 6. Depending on shunsai

- rinsai depends on a **released version** (`shunsai = "0.1"`), not a git pin. ⚠️ **Not true during E0**: shunsai v0.1.0 is not on crates.io yet, so the workspace pins a commit. Knowing, temporary, and tracked — `git grep 'TODO(shunsai-0.1-release)'`, PROGRESS.md's E0 exit criteria, DESIGN.md §2. **No SPRT number may be attributed to a git rev that is not a release.**
- To add an API: prototype on a shunsai branch → **measure it on shunsai's own bench** → adopt → **release shunsai** → raise the requirement here. Never work around a missing API with a slow local reimplementation without saying so.
- Several planned additions unlock a re-measurement that shunsai's decision log deliberately parked for this consumer — see DESIGN.md §6. When adding one, say which re-measurement it unlocks.
- **E0 requires no shunsai change at all**, deliberately: it is the layering's field test.
