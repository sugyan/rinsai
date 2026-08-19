# rinsai

**A shogi engine in Rust — Rust Implementation, NNUE Search, for AI.**

`rinsai` is a [shogi](https://en.wikipedia.org/wiki/Shogi) engine aiming at strong results on [floodgate](https://wdoor.c.u-tokyo.ac.jp/) and in the [世界コンピュータ将棋選手権](https://www.computer-shogi.org/) (World Computer Shogi Championship). It is built on [`shunsai`](https://github.com/sugyan/shunsai) for legal move generation and position management.

> ⚠️ **Status: very early.** rinsai speaks USI and searches: iterative-deepening negamax αβ over a material evaluation, with quiescence, a transposition table and 千日手/連続王手 detection. The repository also carries its own measurement loop — frozen floodgate-derived opening sets, a match harness refereeing on an independent rules library — fixed nodes or a real clock — and a pentanomial SPRT driver, which has run its first real test and passed it. There is **no move ordering beyond the transposition move and no pruning yet**; it plays legal games, which is not a strength claim. The plan is in [DESIGN.md](./DESIGN.md) (route, repository structure, the staged roadmap E0–E6), what is frozen in [CONVENTIONS.md](./CONVENTIONS.md), and why — with the measurements each decision rests on — in [DECISIONS.md](./DECISIONS.md). What is open and what is next are the issues.

## Concept

- **NNUE + αβ first.** Both routes reach the top of the championship — DL won WCSC32/33/36, NNUE won WCSC34/35, and a DL+NNUE consultation hybrid placed 2nd in WCSC36 — so for a solo developer the deciding constraint is training budget, and that points at NNUE: its data generation is CPU-bound, and its training fits a single mid-tier GPU. DL/MCTS is deferred behind explicit written conditions, not rejected. See [DESIGN.md](./DESIGN.md) — the route section and E6.
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

This project **does not reuse GPL-licensed code**. YaneuraOu, dlshogi, tanuki-, cshogi, 技巧2, shogi-server, Stockfish, Fairy-Stockfish and the old `yasai` are GPL and are read-to-understand only, never ported. The list that governs is [CLAUDE.md](./CLAUDE.md)'s licensing rule; this one is a digest. **Running** them as separate processes — sparring, match servers, CSA bridges — carries no obligation and is how this project gets its opposition. See [DESIGN.md](./DESIGN.md) and [CLAUDE.md](./CLAUDE.md).
