# rinsai

**A shogi engine in Rust — Rust Implementation, NNUE Search, for AI.**

`rinsai` is a [shogi](https://en.wikipedia.org/wiki/Shogi) engine aiming at strong results on [floodgate](https://wdoor.c.u-tokyo.ac.jp/) and in the [世界コンピュータ将棋選手権](https://www.computer-shogi.org/) (World Computer Shogi Championship). It is built on [`shunsai`](https://github.com/sugyan/shunsai) for legal move generation and position management.

> ⚠️ **Status: very early.** rinsai speaks USI and searches: iterative-deepening negamax αβ over a material evaluation, with a quiescence search that resolves captures. There is **no transposition table, no move ordering, no repetition detection and no time allocation yet**. It plays legal games; that is not a strength claim, and E0 has no instrument for strength — the SPRT harness is step 7. The plan is in [DESIGN.md](./DESIGN.md) (route, repository structure, the staged roadmap E0–E6), the current state and every measured number in [PROGRESS.md](./PROGRESS.md), what is frozen in [CONVENTIONS.md](./CONVENTIONS.md), and why in [DECISIONS.md](./DECISIONS.md). E0 is a USI shell with iterative-deepening αβ, a transposition table, quiescence search, material evaluation and repetition detection, in eight steps of which three are done.

## Concept

- **NNUE + αβ first.** Both routes reach the top of the championship — DL won WCSC32/33/36, NNUE won WCSC34/35, and a DL+NNUE consultation hybrid placed 2nd in WCSC36 — so for a solo developer the deciding constraint is training budget, and that points at NNUE: its data generation is CPU-bound, and its training fits a single mid-tier GPU. DL/MCTS is deferred behind explicit written conditions, not rejected. See [DESIGN.md](./DESIGN.md) §3 and E6.
- **Nothing is adopted on argument.** `patch → bench → fixed-node paired games → SPRT → merge`, one feature at a time — and the rejected measurements are kept, the same habit `shunsai` runs on.
- **Clean-room, permissively licensed.** Engine *and* training pipeline. See below.
- **Layered on `shunsai`.** Move generation, position management, do/undo, Zobrist and check/pin information belong there; search, evaluation and protocols belong here. SFEN parsing stays external on [`shogi_usi_parser`](https://crates.io/crates/shogi_usi_parser); mate solving belongs to [`tsumeshogi-solver`](https://github.com/sugyan/tsumeshogi-solver).

## The name

**rinsai = 凛彩 (りんさい).**

- As an initialism, it says what the engine is: **R**ust **I**mplementation, **N**NUE **S**earch, for **AI**.
- **凛彩** reads as "crisp brilliance" — 凛 is the 凛 of 凛とした, bracing and composed; 彩 is colour, the 彩 of 光彩 and 彩る.
- It ends in **"-sai"**, continuing [`yasai`](https://github.com/sugyan/yasai) (野菜, "vegetables" — *Yet Another Shogi library, for AI*) and [`shunsai`](https://github.com/sugyan/shunsai) (旬菜・俊才, "seasonal vegetables at their peak" / "a swift prodigy" — *SHogi's Ultra-fast Next-gen Successor, for AI*). Each name in the family is a Japanese word read against an initialism ending in AI.
- One constraint governed the choice: **the name must not claim strength.** An engine that turns out weak should not be embarrassed by its own name, so candidates meaning "genius" or "victory" were dropped in favour of one that describes a quality rather than a rank.

## Licence

`MIT OR Apache-2.0` (permissive), for the engine and the training pipeline alike.

This project **does not reuse GPL-licensed code**. YaneuraOu, dlshogi, tanuki-, cshogi, 技巧2, shogi-server, Stockfish, Fairy-Stockfish and the old `yasai` are GPL and are read-to-understand only, never ported. The list that governs is [CLAUDE.md](./CLAUDE.md) §2; this one is a digest. **Running** them as separate processes — sparring, match servers, CSA bridges — carries no obligation and is how this project gets its opposition. See [DESIGN.md](./DESIGN.md) §7 and [CLAUDE.md](./CLAUDE.md).
