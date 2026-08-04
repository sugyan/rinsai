# rinsai — design

This document is the canonical plan for `rinsai`. For the movegen layer it stands on, see [`shunsai`](https://github.com/sugyan/shunsai)'s own `DESIGN.md`; the division of labour between the two is §2 here and §1–§2 there.

## 1. Goal

A shogi engine that places well on **floodgate** and in the **世界コンピュータ将棋選手権 (WCSC)**.

`shunsai` was built as "the foundation for a search engine, and through it a strong shogi AI" — rinsai is that consumer. shunsai reached M5 (about 1.67× haitaka on the max-moves position) with perft as its only instrument; from here the instrument becomes game results, and the measurement loop becomes SPRT.

**Premises, confirmed with the author (2026-07-31):**

- **Licence**: engine *and* training pipeline stay **`MIT OR Apache-2.0`, clean-room**. GPL work may be read and reimplemented, never ported. **Running** GPL software as a separate process carries no obligation — see §7, run-vs-link.
- **Compute**: a cloud GPU budget on the order of tens of thousands of yen per month. The development machine is an Apple Silicon Mac.
- **Time**: no deadline. Milestones are feature-based. WCSC entry waits until the engine is strong enough (entry is usually Feb–Mar, the tournament in May; 電竜戦 in November is the dress rehearsal).

## 2. Scope, and the line against shunsai

**In rinsai**: search, evaluation, NNUE inference and training, USI and CSA protocol handling, time management, repetition detection (千日手), declaration (入玉宣言), opening books, self-play data generation, the match/SPRT harness.

**Not in rinsai — it belongs to shunsai**: legal move generation, position representation, do/undo, Zobrist keys, check and pin information.

**Not in either**: SFEN/USI *parsing* stays external, on [`shogi_usi_parser`](https://crates.io/crates/shogi_usi_parser) (MIT, same `shogi_core` types). shunsai deliberately does not add SFEN I/O, and rinsai does not reimplement it.

`go mate` is out of scope: that is [`tsumeshogi-solver`](https://github.com/sugyan/tsumeshogi-solver)'s territory.

**The dependency is a released version, not a git pin.** rinsai depends on `shunsai = "0.1"` from crates.io. Prototyping an API addition uses `[patch.crates-io]` with a path override; adopting it means **releasing shunsai** and raising rinsai's requirement. The loop is: try it on a shunsai branch → measure it on shunsai's own bench → adopt → release → bump. rinsai's own crates are `publish = false` — nothing depends on a search engine as a library, and its artifact is a binary.

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
├── DESIGN.md              # this file
├── README.md              # what it is, and the name
├── NETS.md                # evaluation-file registry (added at E3)
├── Cargo.toml             # [workspace]                      (added at E0)
├── crates/
│   ├── rinsai-search/     # lib: αβ + TT + qsearch + time management + repetition + eval
│   │                      #      the only crate that depends on shunsai
│   ├── rinsai/            # bin: the engine. USI on stdio by default, --csa for floodgate
│   └── xtask/             # bin: fetch-net / gen-openings / sprt / release
├── train/                 # PyTorch: dataset, model, quantize, export   (added at E3)
├── positions/             # openings-v1.sfen (frozen), test and bench positions
├── tools/                 # match harness config, ladder definitions
└── .github/workflows/     # fmt / clippy -D warnings / test / bench --no-run
```

**The training pipeline lives in this repository, not its own.** The network architecture, the quantization scheme and the data format are each defined twice — once in Rust for inference, once in Python for training — and any drift between the two is a silent strength bug that SPRT reads as "that patch was bad". Being able to change both in one commit is what prevents it. A secondary benefit: the WCSC appeal document's claim of own-data-and-own-trainer is demonstrable from one repository's history.

**Crates are split when there is a reason, not up front** — the same rule shunsai's log applies to optimizations.

| Crate | Split off at | Because |
|---|---|---|
| `rinsai-nnue` | E3 | SIMD backends (NEON / AVX2) and PyTorch-parity tests want their own test surface. **Draw the boundary carefully**: the accumulator stack parallels the *search* stack, so `nnue` owns pure inference and the `Accumulator` type, while pushing and popping it stays in the search. |
| `rinsai-protocol` | E2 | when the CSA client arrives and wants to share the line-oriented loop with USI |
| `rinsai-selfplay` | E3 | data generation drives the search library in-process rather than over USI, so it needs its own binary |

**One binary, not two.** USI on stdio is the default mode — that is what a GUI and [Ayane](https://github.com/yaneurao/Ayane) do: launch the executable and speak USI — and `--csa` selects the floodgate client. Two binaries would each embed the network and duplicate the time-management and search-driver glue.

**The network loads at runtime by default**, via the USI option `EvalFile`, and is embedded with `include_bytes!` only in release builds behind a feature flag. E3–E4 put many networks per generation through SPRT; recompiling per network does not scale.

### What is not in git

| Artifact | Where it lives | What is committed instead |
|---|---|---|
| Evaluation networks | **GitHub Releases**, named by content hash | `NETS.md`: hash → training run → SPRT result → URL |
| Self-play data | **Object storage** | the shard manifest: generator rev, seed, position count, opening set, label depth, checksum |
| Game records, SPRT logs | local + object storage | positions the engine misplayed, as test fixtures |

A `halfkp_256x2-32-32` network is about **64 MB in its feature transformer alone** (125,388 × 256 × int16), and E3–E4 produce dozens across generations; self-play data runs to tens of GB per generation. Neither belongs in git. The registry and the manifests do: they are the provenance chain that §7's pre-release scan and a WCSC appeal document both need. **The ledger is the artifact that has to be version-controlled — the weights are not.**

### Sparring opponents, and the run-vs-link boundary

GPL engines and servers used as sparring partners (YaneuraOu, 技巧2, Lesserkai, shogi-server) stay in the **local-only, unpublished `../benchmarks` repository**, which already isolates GPL checkouts for shunsai's cross-engine perft harness and already carries a pin policy. Its role widens from perft comparison to sparring; a second local-only repository is not created, because a second YaneuraOu checkout buys nothing.

The match harness and the SPRT scripts are **own code in this repository** and only ever *spawn processes*. The paths joining the two live in a gitignored local config, with an `.example` committed. So run-vs-link is not just a policy sentence — it is where the directory boundary falls.

## 5. Roadmap

Numbered **E0–E6** so as not to collide with shunsai's M0–M7. Rating targets are floodgate estimates.

### E0 — baseline: play legal moves and don't hang pieces (size M)

- USI shell (`position` / `go` / `stop` / `gameover`), iterative-deepening negamax αβ, **TT and quiescence search from the start** (material evaluation without qsearch fails on the horizon effect), simple time management, material evaluation.
- **Repetition (千日手) is mandatory at E0 and lives here, not in shunsai.** Stack `(key(), Hand, in_check())` per ply; use `key()` as the first filter and hand equality to confirm. Perpetual check — where the checking side loses — is decided from the `in_check()` history. shunsai holds no game history by design, so this is the engine's responsibility.
- Harness: **Ayane (Apache-2.0) adopted immediately**, plus SPRT scripts. Sparring by running GPL binaries: Lesserkai → node-limited YaneuraOu → 技巧2.
- **shunsai API additions: none, deliberately.** E0 building against a frozen shunsai is the layering's first contact with a real consumer.
- Infrastructure: repository skeleton, CLAUDE.md, USI conformance dialogue tests, a `bench` command (fixed positions × fixed depth, following the Stockfish convention), CI, and `openings-v1` — balanced positions extracted in-house from high-rated floodgate games, reusing the method of shunsai's `examples/gen_bench_positions.rs`.
- Verification: fixed-depth node counts as a regression test (the search analogue of perft), a mate-in-1..5 suite, repetition and perpetual-check scenario tests.

### E1 — classical search, one feature at a time (size L, 1 feature = 1 SPRT)

Introduction order, with the shogi-specific caveats that differ from chess:

1. TT move ordering
2. MVV-LVA for captures, promotion-aware
3. Killers — **drops can be killers**
4. History — **butterfly boards are impossible, because drops have no `from`**. Index by (piece kind × side, `to`) to unify board moves and drops. Countermove and continuation history later.
5. **Null-move pruning — safer than in chess**, because drops make zugzwang effectively absent. Exclude only when in check, plus endgame verification.
6. LMR — checks, evasions and capture-promotions exempt
7. Check extension — **more important than in chess**; watch its interference with perpetual-check repetition, which is a search-explosion risk
8. SEE in qsearch
9. Futility / razoring — hand value belongs in the margin
10. Aspiration windows
11. Singular extensions, and the rest

**SPRT discipline**: gain tests at elo0=0 / elo1=5, non-regression gates at elo0=-5 / elo1=0, α=β=0.05. **Paired openings with colours swapped are mandatory.** Feature patches run at fixed nodes; speed and time-management patches run in real time.

**This is where shunsai API additions start** — see §6.

### E2 — real-world infrastructure: resident on floodgate (size M)

- **Two stages**: short term, run a GPL bridge as a separate process to get onto floodgate immediately and start the rating series; medium term, an own Rust CSA client (also needed for WCSC). The `csa` crate is MIT and reusable as a record parser; integration-test against a locally hosted shogi-server.
- **Declaration (入玉宣言, the 27-point rule)** implemented engine-side from shunsai's existing bitboard accessors — `bestmove win` / `%KACHI`. No shunsai API addition.
- Lazy SMP with a shared TT (`Position` is cloned once per thread at startup, so there is no allocation problem), ponder, and real time management (floodgate uses Fischer; confirm WCSC's rules for the year).
- Operations: a small resident cloud VM — **aarch64 keeps the NEON path live, and a Mac is unusable here because sleep and NAT corrupt the rating series** — version encoded in the account name so ratings become generation-comparison data, systemd with auto-reconnect, a game-record archive, a rating dashboard.

### E3 — NNUE, first generation (size L — the biggest single step)

- **Start at `halfkp_256x2-32-32`.** HalfKP: king 81 × BonaPiece 1548 = 125,388 dimensions per perspective, ×2 perspectives → 256×2 → 32 → 32 → 1. Larger networks do not pay at first-generation data quality; iteration speed is what matters.
- **Dirty pieces are computed engine-side** from `piece_at` reads before `do_move`, leaving shunsai untouched. A `DirtyPieces`-returning API is considered *with measurement* only if it shows up in a profile. The accumulator stack parallels the search stack; refresh on own-king moves.
- int16/int8 quantization with both SIMD backends (NEON for the development machine and aarch64 VMs, AVX2/VNNI for x86 production).
- **Training written from scratch in PyTorch.** YaneuraOu's learner, nnue-pytorch and tanuki- are all GPL — readable, not portable; the recipes are ideas and therefore free. **The data format is also in-house**: fixed-length records of about 40 B, zstd shards, a manifest carrying generator rev and seed. Deliberately *not* PackedSfen-compatible.
- **Data bootstrap**: generation 0 is self-play by the finished E1 engine (random openings, depth 6–9 labels), **100 M positions** (20–50 M floor), plus own re-labelling of floodgate game positions. Generation runs on CPU spot fleets (aarch64 spot is cheap and compounds with the NEON work); training on an **L4/A10G-class GPU for 1–3 days**, inside the monthly budget. MPS is for prototyping only.
- **Generational iteration**: new network → self-play → 300–500 M positions per generation at depth 8–10 → retrain. Two to four generations give a marked gain. **Mix in a few percent of entering-king and declaration positions** — a classic NNUE weakness, and E2's declaration code is reused to generate them.
- Experiment tracking: wandb with local JSONL duplication; evaluation files named by content hash, registered in `NETS.md`, one-to-one with a git tag.
- Verification: pre/post-quantization agreement within tolerance, Rust inference against PyTorch inference on fixed positions, SPRT per generation, floodgate rating.

### E4 — iteration at scale, and adapting to production hardware (size L, mostly compute-bound)

- Automate generate → train → SPRT → promote; scale to roughly 1 B cumulative positions. A/B larger networks (L1 512/1024, HalfKPE9 / HalfKA families). SPSA on search parameters. A shallow opening book.
- **Settle shunsai's recorded x86-64 re-measurements in one batch**: magic vs qugiy vs `pext` on a TT-pressured search bench, AVX2/VNNI inference throughput, and confirming that `pext` is avoided on Zen. Tournaments and cloud production are x86, so this is the first point at which production-architecture numbers exist at all.
- Re-adjudicate the conditional items shunsai parked (MoveSet size, `DirtyPieces`, incremental AttackInfo). **Target: R3800–4200.**

### E5 — WCSC preparation (size M)

- Entry threshold **R3800+**; below that, defer a year and continue E4. **電竜戦 in November is the dress rehearsal** — CSA connection, long-run operation, declaration wins under real conditions.
- Feature freeze 4–6 weeks before. Appeal document (the clean-room policy and own training are good material). Library declaration — `shogi_core`, `shunsai`, own crates; check that year's rules. Hardware: a large cloud instance with an on-site laptop fallback. Pre-release provenance scan, applying §7 to the engine and the trainer alike.

### E6 — decision point: DL / MCTS / consultation (only if the conditions hold)

- **Start conditions, all three**: (1) NNUE + αβ has plateaued on floodgate for months with diminishing returns; (2) the training budget can be temporarily multiplied 5–10×; (3) inference GPU can be secured for the tournament.
- Reusable assets: shunsai drops straight into PUCT legal-move expansion; the self-play data infrastructure, and all of the USI/CSA/SPRT/floodgate operations, carry over. A Ryfamate-style consultation comes only once both sides are strong.
- **Not now**: under a budget constraint, one Elo is cheapest on the NNUE side. WCSC36's DL win is a product of corporate compute and does not mean DL is optimal on an individual budget.

## 6. The shunsai API extension catalogue

Each addition is prototyped on a shunsai branch, measured on shunsai's own bench, adopted, **released**, and then required by version here. Several of them also unlock a re-measurement that shunsai's decision log parked for exactly this consumer.

| API | Phase | The shunsai re-measurement it unlocks |
|---|---|---|
| expose `attackers_to` + public `Bitboard` iteration | E1 | — (SEE's prerequisite) |
| staged generation (captures / evasions / quiets) | E1 | **the `MoveSet` 48-byte question** — this is the caller that finally *collects* move sets |
| `gives_check` | E1 | — |
| `do_null_move` / `undo_null_move` | E1 | — |
| expose `checkers` / `pinned` | E1 | "expose rather than recompute" (shunsai, 2026-07-29) |
| (optional) a `DirtyPieces`-returning `do_move` | judged E3 → E4 | the incremental-AttackInfo trade family |
| (measurement only) magic vs qugiy vs `pext` | E1 once a TT-backed bench exists, and E4 on x86 | shunsai 2026-07-27 / 07-29 |
| (measurement only) lifting the `king_danger` neighbourhood filter | only if a demand appears | shunsai 2026-07-30 |
| SFEN I/O | **never added** | stays external on `shogi_usi_parser` (§2) |

## 7. Licensing

Adopts shunsai's §7 verbatim, plus one sharpening.

**May be referenced and reused as code** (permissive): haitaka, cozy-chess, the `shogi_core` family, `shogi_usi_parser`, the `usi` and `csa` crates (all MIT), **Ayane (Apache-2.0)**, and public write-ups — CPW, the NNUE papers, the AlphaZero-family papers, the Qugiy appeal document. Retain copyright notices when reusing.

**May be run, never read for porting** (GPL): YaneuraOu, dlshogi, tanuki-, cshogi, 技巧2, shogi-server, Stockfish, Fairy-Stockfish, and **the old yasai** — which is sugyan's own work but GPL-3.0, derived from apery_rust, and therefore just as off-limits.

**run-vs-link.** Running a GPL program as a separate process creates no obligation on this repository: nothing GPL is linked, and nothing GPL is distributed. That is what makes sparring ladders, CSA bridges and local match servers available to a permissive project. Reading-to-reimplement stays allowed; porting stays forbidden. GPL checkouts are isolated in a local-only directory (`../benchmarks`), never here.

**Note: there is no major permissive reference for *search* code** — the strong chess engines are GPL too. Search here is written from CPW, papers and first principles, which is a real cost and is accepted deliberately.

**Data.** floodgate game records and self-play output are factual data (the position shunsai took on 2026-07-24). A pre-release provenance scan is mandatory, and the manifest chain must make each trained network demonstrably the product of own data and own trainer.

## 8. How things are verified

1. **The measurement loop from E0 onward is the most important thing**: patch → `bench` (node-count agreement, the search analogue of perft) → fixed-node paired games → SPRT → merge. **Record the rejected numbers too** — shunsai's habit of keeping the losing measurements applies here unchanged.
2. **Strength ladder**: Lesserkai → node-limited YaneuraOu (1k / 10k / 100k / 1M, giving evenly spaced grades from one binary) → 技巧2 → full YaneuraOu → floodgate rating.
3. **Correctness**: mate suites, repetition and declaration scenario tests, USI conformance dialogue tests. On the shunsai side the existing differential tests against `shogi_legality_lite` keep holding the movegen layer.

## 9. Decision log

Newest last. The format follows shunsai's: what was decided, why, and — where the decision is conditional — what would have to be measured to revisit it.

- **2026-07-31 — NNUE + αβ first; DL/MCTS deferred behind explicit conditions; the plan is staged E0–E6.** Reasoning in §3, conditions in E6. Decided against the 2022–26 championship record, verified that day. Recorded in shunsai's decision log the same day as the schedule it imposes there.
- **2026-08-04 — named `rinsai` (凛彩), in a new repository, depending on released versions of shunsai.**
  - **The name**: **R**ust **I**mplementation, **N**NUE **S**earch, for **AI**. It continues both family rules — the `-sai` ending, and the two-layer construction of a kanji reading set against an initialism ending in AI (yasai 野菜 / "Yet Another Shogi library, for AI"; shunsai 旬菜・俊才 / "SHogi's Ultra-fast Next-gen Successor, for AI"). The governing constraint was that **the name must not assert strength**: an engine that turns out weak should not be embarrassed by its own name, which removed 天才/甜菜 and 勝ち/価値. Also rejected, for reasons that generalize: 主菜 (one letter from shunsai — confusable in directory listings and tournament tables), 懐石/解析 (the best functional meaning of any candidate; lost on sound), 菜の花 (collides with the existing engine なのは), 捌き / 矢倉 / と金 / 雁木 (naming a technique or piece outright reads as a lack of invention rather than an allusion), 光彩/虹彩 (heard as 交際 first). Checked against the WCSC36 entry list — **not** against crates.io, which does not apply because these crates are `publish = false`.
  - **A new repository, not a reboot of `yasai`** — which was considered, since "Yet Another Shogi AI" re-reads the same letters. Two apparent obstacles turned out not to exist: yasai was never published to crates.io, and `tsumeshogi-solver` pins it by git *tag*, which survives anything done to the branch. What decides it is that replacing the tree leaves GPL code in the history. That is legal — the new tree would not be a derivative — but it leaves a path for a helper to be lifted out of an old commit, and that mistake is invisible after the fact. Eliminating the surface is worth more than the pun to a project whose top rule is licensing. Secondarily, the lineage would read yasai → shunsai → yasai2. **This is the judgement shunsai already made**: yasai was rewritten rather than relicensed, for the same reason.
  - **Released-version dependency, replacing the git-pin scheme this plan originally assumed.** A release per API addition was assumed to be a cost worth avoiding. It is not: shunsai is a library with third-party value and belongs on crates.io regardless, and cutting a release when E1 adds `attackers_to` is ordinary maintenance. Consequence: **shunsai v0.1.0 is a prerequisite of E0**, and E1's API additions each carry semver.
  - **Networks and self-play data stay out of git** (§4), with the registry and manifests standing in for them. **Crates are split on demand** (§4), and the layout changed in two places from the original sketch: one binary rather than two, and runtime network loading rather than `include_bytes!` by default.
