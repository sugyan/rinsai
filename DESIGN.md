# rinsai — design

This document is the canonical plan for `rinsai`. For the movegen layer it stands on, see [`shunsai`](https://github.com/sugyan/shunsai)'s own `DESIGN.md`. The division of labour between the two is the scope section below.

## 1. Goal

A shogi engine that places well on **floodgate** and in the **世界コンピュータ将棋選手権 (WCSC)**.

`shunsai` was built as "the foundation for a search engine, and through it a strong shogi AI" — rinsai is that consumer. shunsai reached M5 (about 1.67× haitaka on the max-moves position) with perft as its only instrument; from here the instrument becomes game results, and the measurement loop becomes SPRT.

**Premises, confirmed with the author (2026-07-31):**

- **Licence**: engine *and* training pipeline stay **`MIT OR Apache-2.0`, clean-room**. GPL work may be read and reimplemented, never ported. **Running** GPL software as a separate process carries no obligation — see [CLAUDE.md](./CLAUDE.md) on run-vs-link.
- **Compute**: a cloud GPU budget on the order of tens of thousands of yen per month. The development machine is an Apple Silicon Mac.
- **Time**: no deadline. Milestones are feature-based. WCSC entry waits until the engine is strong enough (entry is usually Feb–Mar, the tournament in May; 電竜戦 in November is the dress rehearsal).

## 2. Scope, and the line against shunsai

**In rinsai**: search, evaluation, NNUE inference and training, USI and CSA protocol handling, time management, repetition detection (千日手), declaration (入玉宣言), opening books, self-play data generation, the match/SPRT harness.

**Not in rinsai — it belongs to shunsai**: legal move generation, position representation, do/undo, Zobrist keys, check and pin information.

**Not in either**: SFEN/USI *parsing* stays external, on [`shogi_usi_parser`](https://crates.io/crates/shogi_usi_parser) (MIT, same `shogi_core` types). shunsai deliberately does not add SFEN I/O, and rinsai does not reimplement it.

`go mate` is out of scope: that is [`tsumeshogi-solver`](https://github.com/sugyan/tsumeshogi-solver)'s territory.

**The dependency is a released version, not a git pin.** rinsai depends on `shunsai = "0.1"` from crates.io. Prototyping an API addition uses `[patch.crates-io]` with a path override; adopting it means **releasing shunsai** and raising rinsai's requirement. The loop is: try it on a shunsai branch → measure it on shunsai's own bench → adopt → release → bump. rinsai's **engine** crates are `publish = false` — nothing depends on a search engine as a library, and its artifact is a binary. The one publication-intended exception is `rinsai-game`, the rules library the match referee and [tuishogi](https://github.com/sugyan/tuishogi) share ([DECISIONS.md](./DECISIONS.md), 2026-08-12).

## 3. Route: NNUE + αβ first, DL/MCTS conditional

**Decision: NNUE + αβ search is the main line. DL/MCTS and consultation hybrids are a conditional later option, deferred rather than rejected.**

The championship record is genuinely split. DL won WCSC32/33 (dlshogi) and WCSC36 (氷彗, HEROZ, 2026-05); NNUE won WCSC34 (tanuki-) and WCSC35 (水匠). WCSC36's runner-up **Ryfamate** was a consultation hybrid of one DL and two NNUE engines, with dlshogi 3rd and 水匠 4th. Both routes reach the top, so the deciding constraint for a solo developer is not which is stronger in principle but which converts a fixed budget into rating fastest.

1. The DL winners are products of **corporate-scale training resources**. At tens of thousands of yen a month, training a DL network from scratch loses to NNUE on both Elo-per-yen and Elo-per-hour. NNUE is not a dead route — 水匠 placed 4th and the hybrid 2nd at the same WCSC36.
2. `halfkp_256x2-32-32` trains on **one mid-tier GPU in a few days**, and its data generation is **CPU-bound** — which is exactly where shunsai's movegen speed converts directly into training-data throughput.
3. shunsai is already shaped for αβ: make-unmake, an allocation-free callback, incremental Zobrist.
4. A Ryfamate-style hybrid presupposes a strong NNUE engine *and* a strong DL engine, so building the NNUE side first is the right order regardless.

The conditions under which DL is revisited are in E6.

## 4. Repository structure

One repository, a Cargo workspace, with the training pipeline alongside the engine.

```
rinsai/
├── CLAUDE.md              # rules for implementation sessions
├── DESIGN.md              # this file: the plan
├── DECISIONS.md           # what was decided, and what lost; retired in place
├── CONVENTIONS.md         # what is frozen, by subject
├── README.md              # what it is, and the name
├── NETS.md                # evaluation-file registry                    (E3)
├── Cargo.toml             # [workspace]
├── crates/
│   ├── rinsai-game/       # lib: the rules layer — legality-gated play, undo, 千日手 and
│   │                      #      連続王手 adjudication; the match referee, shared with tuishogi
│   ├── rinsai-search/     # lib: αβ + TT + qsearch + time management + repetition + eval
│   │                      #      the only crate that depends on shunsai
│   ├── rinsai/            # bin: the engine. USI on stdio by default, --csa for floodgate
│   └── xtask/             # bin: fetch-floodgate / gen-openings / sprt;
│                          #      fetch-net and release join later
├── train/                 # PyTorch: dataset, model, quantize, export   (E3)
├── positions/             # the frozen bench and opening sets
├── tools/                 # match harness config, ladder definitions
└── .github/workflows/     # fmt / clippy -D warnings / test / MSRV / cargo-deny
```

An entry marked with a phase arrives then; everything else exists today.

**The training pipeline lives in this repository, not its own.** The network architecture, the quantization scheme and the data format are each defined twice — once in Rust for inference, once in Python for training — and any drift between the two is a silent strength bug that SPRT reads as "that patch was bad". Being able to change both in one commit is what prevents it. A secondary benefit: the WCSC appeal document's claim of own-data-and-own-trainer is demonstrable from one repository's history.

**Crates are split when there is a reason, not up front** — the same rule shunsai's log applies to optimizations.

| Crate | Split off at | Because |
|---|---|---|
| `rinsai-game` | E0 (exists) | the match referee and tuishogi share one rules layer, and a crate boundary is what a second repository can adopt; moved from tuishogi rather than written |
| `rinsai-nnue` | E3 | SIMD backends (NEON / AVX2) and PyTorch-parity tests want their own test surface. **Draw the boundary carefully**: the accumulator stack parallels the *search* stack, so `nnue` owns pure inference and the `Accumulator` type, while pushing and popping it stays in the search. |
| `rinsai-protocol` | E2 | when the CSA client arrives and wants to share the **session layer** with USI: both drive the same `SearchDriver`, `Game` and time management, and both need "one answer per turn, structurally" to hold. **Not** the line-oriented loop, and **not** a codec — the two protocols share no grammar, no move notation (`+7776FU` against `7g7f`) and no state machine. |
| `rinsai-selfplay` | E3 | data generation drives the search library in-process rather than over USI, so it needs its own binary |

**One binary, not two.** USI on stdio is the default mode — that is what a GUI and [Ayane](https://github.com/yaneurao/Ayane) do: launch the executable and speak USI — and `--csa` selects the floodgate client. Two binaries would each embed the network and duplicate the time-management and search-driver glue.

**The network loads at runtime by default**, via the USI option `EvalFile`, and is embedded with `include_bytes!` only in release builds behind a feature flag. E3–E4 put many networks per generation through SPRT; recompiling per network does not scale.

### What is not in git

| Artifact | Where it lives | What is committed instead |
|---|---|---|
| Evaluation networks | **GitHub Releases**, named by content hash | `NETS.md`: hash → training run → SPRT result → URL |
| Self-play data | **Object storage** | the shard manifest: generator rev, seed, position count, opening set, label depth, checksum |
| Game records, SPRT logs | local + object storage | positions the engine misplayed, as test fixtures |

A `halfkp_256x2-32-32` network is almost entirely its feature transformer (125,388 × 256 × int16, 61 MiB), and E3–E4 produce dozens across generations; self-play data runs to tens of GB per generation. Neither belongs in git. The registry and the manifests do: they are the provenance chain that the pre-release scan and a WCSC appeal document both need. **The ledger is the artifact that has to be version-controlled — the weights are not.**

### Sparring opponents, and the run-vs-link boundary

GPL engines and servers used as sparring partners (YaneuraOu, 技巧2, Lesserkai, shogi-server) stay in the **local-only, unpublished `../benchmarks` repository**, which already isolates GPL checkouts for shunsai's cross-engine perft harness and already carries a pin policy. Its role widens from perft comparison to sparring; a second local-only repository is not created, because a second YaneuraOu checkout buys nothing.

The match harness and the SPRT scripts are **own code in this repository** and only ever *spawn processes*. The paths joining the two live in a gitignored local config, with an `.example` committed. So run-vs-link is not just a policy sentence — it is where the directory boundary falls.

## 5. Roadmap

Numbered **E0–E6** so as not to collide with shunsai's M0–M7. Rating targets are floodgate estimates.

### E0 — baseline: play legal moves and don't hang pieces (size M) — **done**

- USI shell (`position` / `go` / `stop` / `gameover`), iterative-deepening negamax αβ with a transposition table and quiescence search, time management, material evaluation.
- **Repetition (千日手) is engine-side, not shunsai's.** Stack `(key(), Hand, in_check())` per ply; `key()` filters, hand equality confirms, and the `in_check()` history decides perpetual check. shunsai holds no game history by design.
- Harness: **an own Rust USI match harness and SPRT driver in `crates/xtask`**, refereeing on `crates/rinsai-game`. Sparring by running GPL binaries: node-limited YaneuraOu → 技巧2.
- **shunsai API additions: none, deliberately.** E0 asked shunsai for nothing new — the layering's first contact with a real consumer.
- Infrastructure: repository skeleton, USI conformance dialogue tests, a `bench` command, CI, and the frozen opening sets — balanced positions extracted in-house from high-rated floodgate games.
- Verification: fixed-depth node counts as a regression test, a mate-in-1..5 suite, repetition and perpetual-check scenario tests.
- **The retroactive audit E0's batching bought has run and passed**: a non-regression SPRT of the finished E0 against the step-3b engine. The numbers are in the decision log.

### E1 — classical search, one feature at a time (size L, 1 feature = 1 SPRT)

Introduction order, with the shogi-specific caveats that differ from chess. TT
move ordering is absent because it landed with the table at E0.

1. MVV-LVA for captures, promotion-aware. **Baseline: E0's quiescence is deliberately unordered.**
2. Killers — **drops can be killers**
3. History — **butterfly boards are impossible, because drops have no `from`**. Index by (piece kind × side, `to`) to unify board moves and drops. Countermove and continuation history later.
4. **Null-move pruning — safer than in chess**, because drops make zugzwang effectively absent. Exclude only when in check, plus endgame verification. ⚠️ **It is the first item to meet E0's repetition path, and the collision is silent.** That path is scanned two entries at a time because it alternates side to move; a pass pushed onto it shifts the parity of the whole subtree below, and the scan then compares against the opponent's positions only and reports no repetition at all. Either keep the pass off the path or make the walk parity-agnostic — and no fixture can catch this, because every repetition test reaches its verdict through the game's history rather than through the tree.
5. LMR — checks, evasions and capture-promotions exempt
6. Check extension — **more important than in chess**; watch its interference with perpetual-check repetition, which is a search-explosion risk
7. SEE in qsearch — and with it the two things E0's quiescence leaves out: **non-capture promotions** (歩→と is a 500 cp event in rinsai's own table, so a capture-only quiescence is blind to と金作り — a shogi-specific gap with no chess analogue) and **checks**, which also need `gives_check`. ⚠️ **This item also owns extending the repetition path into quiescence**, whose exclusion rests on an argument that generating checks and quiet promotions ends. CONVENTIONS.md carries the rule and the condition.
8. Futility / razoring — hand value belongs in the margin. **Baseline: E0's quiescence is deliberately unpruned.** Also owns `QS_MAX_CHECK_PLIES`, which E0 set by measurement and without an instrument.
9. Aspiration windows. ⚠️ This item owes `negamax_root` the ability to tell a fail-low from an abandoned iteration; the decision log carries why it cannot today.
10. **Quiescence probes and stores in the transposition table.** Measured at E0 and **not** shipped there: it halves the tree on every fixture tried, which is the opposite of what the conventional argument predicts, and node count is not an instrument for strength. ⚠️ It rebaselines every `bench` count, so it wants a pull request of its own.
11. **Scoring the first repetition on the search path as a draw**, rather than the fourth occurrence the rule names. Standard practice, it prunes cycles, and it is a search heuristic rather than the rule. ⚠️ It rebaselines every `bench` count, unlike E0's version, which moved none.
12. Singular extensions, and the rest

**SPRT discipline** — the parameters are in [CLAUDE.md](./CLAUDE.md) and are not restated here. What is specific to E1 is the pacing: **one item on this list is one SPRT**, in list order, and an item that fails to pass is recorded in the decision log with its numbers rather than retried until it does.

**Measurement is serial; implementation is not.** Features are built ahead, each on its own branch with its unit tests and bench counts, while the harness drains one SPRT at a time — a fixed-node game between deterministic engines is noise-immune, so the queue runs beside development sessions. Merge on pass, rebase the queue behind it. The bottleneck is machine time, which is the correct one.

**This is where shunsai API additions start** — see the catalogue below.

### E2 — real-world infrastructure: resident on floodgate (size M)

- **Two stages**: short term, run a GPL bridge as a separate process to get onto floodgate immediately and start the rating series; medium term, an own Rust CSA client (also needed for WCSC). The `csa` crate is MIT and reusable as a record parser; integration-test against a locally hosted shogi-server. ⚠️ **The short-term stage is not sequenced after E1**: it starts as soon as step 5's wire margin exists and runs beside E1's queue — the rating series is calendar-bound, and every week it starts earlier is a week of baseline the later generations are compared against.
- **Declaration (入玉宣言, the 27-point rule)** implemented engine-side from shunsai's existing bitboard accessors — `bestmove win` / `%KACHI`. No shunsai API addition.
- Lazy SMP with a shared TT (`Position` is cloned once per thread at startup, so there is no allocation problem), ponder, and real time management (floodgate uses Fischer; confirm WCSC's rules for the year). ⚠️ **`go ponder movetime n` starts its deadline at search start rather than at `ponderhit`** — harmless while the driver holds the answer back, and this is the step that fixes it.
- Operations: a small resident cloud VM — **aarch64 keeps the NEON path live, and a Mac is unusable here because sleep and NAT corrupt the rating series** — version encoded in the account name so ratings become generation-comparison data, systemd with auto-reconnect, a game-record archive, a rating dashboard.

### E3 — NNUE, first generation (size L — the biggest single step)

- **The pipeline lands end-to-end as one batch, trusted on the deterministic checks in the verification line below before any strength claim** — until the loop closes, no component of it can be evaluated at all. Strength enters once, at the end: generation 0 over the material evaluation is a single large-margin SPRT.
- **Start at `halfkp_256x2-32-32`.** HalfKP: king 81 × BonaPiece 1548 = 125,388 dimensions per perspective, ×2 perspectives → 256×2 → 32 → 32 → 1. Larger networks do not pay at first-generation data quality; iteration speed is what matters.
- **Dirty pieces are computed engine-side** from `piece_at` reads before `do_move`, leaving shunsai untouched. A `DirtyPieces`-returning API is considered *with measurement* only if it shows up in a profile. The accumulator stack parallels the search stack; refresh on own-king moves.
- int16/int8 quantization with both SIMD backends (NEON for the development machine and aarch64 VMs, AVX2/VNNI for x86 production).
- **Training written from scratch in PyTorch.** YaneuraOu's learner, nnue-pytorch and tanuki- are all GPL — readable, not portable; the recipes are ideas and therefore free. **The data format is also in-house**: fixed-length records of about 40 B, zstd shards, a manifest carrying generator rev and seed. Deliberately *not* PackedSfen-compatible.
- **Data bootstrap**: generation 0 is self-play by the finished E1 engine (random openings, depth 6–9 labels), **100 M positions** (20–50 M floor), plus own re-labelling of floodgate game positions. Generation runs on CPU spot fleets (aarch64 spot is cheap and compounds with the NEON work); training on an **L4/A10G-class GPU for 1–3 days**, inside the monthly budget. MPS is for prototyping only.
- **Generational iteration**: new network → self-play → 300–500 M positions per generation at depth 8–10 → retrain. Two to four generations give a marked gain. **Mix in a few percent of entering-king and declaration positions** — a classic NNUE weakness, and E2's declaration code is reused to generate them.
- Experiment tracking: wandb with local JSONL duplication; evaluation files named by content hash, registered in `NETS.md`, one-to-one with a git tag.
- Verification: pre/post-quantization agreement within tolerance, Rust inference against PyTorch inference on fixed positions, an incremental-against-refresh accumulator differential over random game paths, SPRT per generation, floodgate rating.

### E4 — iteration at scale, and adapting to production hardware (size L, mostly compute-bound)

- Automate generate → train → SPRT → promote; scale to roughly 1 B cumulative positions. A/B larger networks (L1 512/1024, HalfKPE9 / HalfKA families). SPSA on search parameters. A shallow opening book.
- **Settle shunsai's recorded x86-64 re-measurements in one batch**: magic vs qugiy vs `pext` on a TT-pressured search bench, AVX2/VNNI inference throughput, and confirming that `pext` is avoided on Zen. Tournaments and cloud production are x86, so this is the first point at which production-architecture numbers exist at all.
- Re-adjudicate the conditional items shunsai parked (MoveSet size, `DirtyPieces`, incremental AttackInfo). **Target: R3800–4200.**

### E5 — WCSC preparation (size M)

- Entry threshold **R3800+**; below that, defer a year and continue E4. **電竜戦 in November is the dress rehearsal** — CSA connection, long-run operation, declaration wins under real conditions.
- Feature freeze 4–6 weeks before. Appeal document (the clean-room policy and own training are good material). Library declaration — `shogi_core`, `shunsai`, own crates; check that year's rules. Hardware: a large cloud instance with an on-site laptop fallback. Pre-release provenance scan, over the engine and the trainer alike.

### E6 — decision point: DL / MCTS / consultation (only if the conditions hold)

- **Start conditions, all three**: (1) NNUE + αβ has plateaued on floodgate for months with diminishing returns; (2) the training budget can be temporarily multiplied 5–10×; (3) inference GPU can be secured for the tournament.
- Reusable assets: shunsai drops straight into PUCT legal-move expansion; the self-play data infrastructure, and all of the USI/CSA/SPRT/floodgate operations, carry over. A Ryfamate-style consultation comes only once both sides are strong.
- **Not now**: under a budget constraint, one Elo is cheapest on the NNUE side. WCSC36's DL win is a product of corporate compute and does not mean DL is optimal on an individual budget.

## 6. The shunsai API extension catalogue

Each addition is prototyped on a shunsai branch, measured on shunsai's own bench, adopted, **released**, and then required by version here — a release per *adoption*, not one per API, so several additions adopted together may travel as one release. Several of them also unlock a re-measurement that shunsai's decision log parked for exactly this consumer.

| API | Phase | The shunsai re-measurement it unlocks |
|---|---|---|
| expose `attackers_to` + public `Bitboard` iteration | E1 | — (SEE's prerequisite) |
| staged generation (captures / evasions / quiets) | E1 | **the `MoveSet` 48-byte question** — this is the caller that finally *collects* move sets. **Measured demand from E0 step 3a**: every quiescence node runs full legal generation and keeps a handful, **48× generated per kept at the initial position and 86× on a drop-heavy middlegame** |
| `gives_check` | E1 | — |
| `do_null_move` / `undo_null_move` | E1 | — |
| expose `checkers` / `pinned` | E1 | "expose rather than recompute" (shunsai, 2026-07-29). **Its E0 consumer arrived at step 3a**: every quiescence node calls `in_check()`, which recomputes `attackers_to(king)` that `generate_moves` computes internally one line later and does not expose. **A second arrived at step 4**: every interior node's `do_move` now records an `in_check()` for the repetition path, which the 連続王手 half of the rule reads |
| (optional) a `DirtyPieces`-returning `do_move` | judged E3 → E4 | the incremental-AttackInfo trade family |
| (measurement only) magic vs qugiy vs `pext` | E1 once a TT-backed bench exists, and E4 on x86 | shunsai 2026-07-27 / 07-29 |
| (measurement only) lifting the `king_danger` neighbourhood filter | only if a demand appears | shunsai 2026-07-30 |
| SFEN I/O | **never added** | stays external on `shogi_usi_parser` |

## 7. Licensing

`MIT OR Apache-2.0`, for the engine and the training pipeline alike — shunsai's policy, adopted verbatim and sharpened in one place.

**The operative rules — the permissive list, the GPL list, and run-vs-link — are in [CLAUDE.md](./CLAUDE.md), which is where an implementation session reads them.** They are not repeated here; a licensing rule kept in two places is a licensing rule that can disagree with itself.

What belongs to the *plan* rather than to the rules is the consequence: **there is no major permissive reference for *search* code**, because the strong chess engines are GPL too. Search here is written from CPW, papers and first principles. That is a real cost, it is accepted deliberately, and it is why the roadmap gives search its own staged phase (E1) instead of assuming a shortcut exists.

**Data.** floodgate game records and self-play output are factual data (the position shunsai took on 2026-07-24). A pre-release provenance scan is mandatory, and the manifest chain must make each trained network demonstrably the product of own data and own trainer.

## 8. How things are verified

1. **The measurement loop from E0 onward is the most important thing**: patch → `bench` (node-count agreement, the search analogue of perft) → fixed-node paired games → SPRT → merge. **Record the rejected numbers too** — shunsai's habit of keeping the losing measurements applies here unchanged.
2. **Strength ladder**: Lesserkai → node-limited YaneuraOu (1k / 10k / 100k / 1M, giving evenly spaced grades from one binary) → 技巧2 → full YaneuraOu → floodgate rating.
3. **Correctness**: mate suites, repetition and declaration scenario tests, USI conformance dialogue tests. On the shunsai side the existing differential tests against `shogi_legality_lite` keep holding the movegen layer.
