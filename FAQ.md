# rinsai — FAQ

Why the code is the way it is, and not the other way. Written for the question
someone actually asks; if an answer stops being true, fix the answer — `git log`
holds the old one, the pull requests hold what happened, and `gh issue list`
holds what is next.

**A measurement is written once** — here, beside the rule it justifies, or in
[STRENGTH.md](./STRENGTH.md) when producing it again would mean playing games.
Never in two of them.

## Route and repository

### Why NNUE + αβ rather than DL/MCTS?

Both routes reach the top of the championship, so for a solo developer the
deciding constraint is which converts a fixed budget into rating fastest. DL
winners are products of corporate-scale training; `halfkp_256x2-32-32` trains on
one mid-tier GPU in a few days and its data generation is CPU-bound, which is
where shunsai's movegen speed converts directly into throughput. DL/MCTS is
deferred behind the conditions in DESIGN.md's E6, not rejected.

### Why a new repository instead of rebooting `yasai`?

Replacing the tree leaves GPL code in the history. That is legal — the new tree
would not be a derivative — but it leaves a path for a helper to be lifted out
of an old commit, and that mistake is invisible after the fact. shunsai made the
same judgement: yasai was rewritten rather than relicensed.

### Why depend on a released shunsai rather than a git rev?

The git pin was the original scheme, on the assumption that a release per API
addition was a cost worth avoiding. It is not: shunsai belongs on crates.io
regardless, and cutting a release is ordinary maintenance. The rule that
outlives it is CLAUDE.md's — **no SPRT number may be recorded against a rev that
is not also a release**, because a rating series has to name what it measured.

### Why is `rinsai-game` a crate here rather than a sibling repository?

A path dependency carries no unreleased-dependency debt, and the shunsai git pin
was the one such debt E0's exit criterion existed to clear. Rejected also:
vendoring it into `xtask`, because a crate boundary is what lets tuishogi adopt
it. It was moved from tuishogi rather than written (tuishogi @ 73d0d9c, MIT,
same author, relicensed by the author to `MIT OR Apache-2.0`).

### Why does the referee use `shogi_legality_lite` instead of shunsai?

So that a refereed game is a differential test of the players' movegen rather
than an echo of it. The cost is two implementations of the repetition rule, and
that is deliberate: the search keeps its own zobrist-filtered one, the referee
runs `rinsai-game`'s normalised-SFEN exact one, and they were held to agree by
differential tests before the harness played a game. Over refereed games, which
end at the fourth occurrence, the two windows provably coincide; on a *fifth*
they deliberately differ, because `rinsai-game` cannot represent one.

### Why does `can_declare` answer a question instead of `declare` refusing?

Because only the caller knows what its own rules do about a false claim, and the
answers differ: the harness scores one as a foul, the usual tournament rule,
while a UI shows a refusal and plays on. A `declare` that checked would flatten
that into "nothing happened", and make one of the six adjudicators partial.

The engine will not share it either — a referee that asks the claimant's own
code whether the claimant was right is not refereeing — which is the split the
repetition rule already carries, differential test and all.

## Search

### Why is quiescence capped by *checked* plies rather than by plies?

Because a bound is only sound if it is a bound on the thing that is actually
unbounded. A capture chain is self-limiting — every capture moves a piece off
the board and quiescence plays no drops, so occupancy strictly decreases. A
check-evasion chain has no such argument: an evasion need not be a capture, and
an evasion list is *every* legal move including drops. A total-ply cap tight
enough to save nodes cuts capture chains in the middle, which is the horizon
effect reintroduced two plies down.

### Why is `QS_MAX_CHECK_PLIES` 2?

Because E0 measured it as the cheapest cap that is also correct — and ⚠️ **that
measurement no longer holds.** Re-derived at HEAD on the drop-heavy fixture at
depth 4, the cost is now *monotone* in the cap and one checked ply is cheaper
than two, where at E0 it was three times dearer. The ordering inverted when the
interior nodes and quiescence gained move ordering.

What survives is only this: **zero is wrong rather than cheap.** It evaluates
while in check and misses a mate one ply away, and no cost makes that a
trade. Every cap above it resolves checks; which one is best is now an open
question, and one the numbers currently answer as "fewer than two".

E1's futility item owns settling it, with an SPRT rather than a node count.

⚠️ **Re-derive before quoting any ordering from this answer.** The counts are
not written here because every ordering and pruning patch moves them — change
the constant, run `bench`, read the drop-heavy position — and the previous
version of this answer was falsified by a patch that landed two commits after
it was written.

### Why did TT move ordering land with the table rather than at E1?

A transposition table whose move nobody tries first is half a table. Measured
at E0, before anything else ordered an interior node: searching the stored move
first was worth between 1.00× and **14.02×** across five fixtures and four
depths, super-linearly in depth.

⚠️ **That range is E0's and does not hold today.** With MVV-LVA and killers
ordering the interior nodes, taking the stored move's front away no longer
reliably grows the tree — on several of the positions `bench` carries it
*shrinks* it. Node count is not strength, so this is not an argument for
dropping the ordering; it is why the tripwire that guards it had to change
fixture, and the sort of question E1 answers with an SPRT rather than a count.

### Why is a capture's ordering key a pair rather than one scaled number?

Because the pair makes the priority a property of the *type* rather than of a
constant. Folding "take with the cheapest piece" into the same integer as "win
the most material" needs a scale above the dearest attacker divided by the
smallest gap two capture gains can have. That ratio is computable from today's
table and a scale can be chosen — but both halves of it are properties of the
table, which E3 replaces wholesale, and a scale that quietly stops being large
enough reverses orderings with nothing to say so. `(i32, i32)` compares its
first element first, so there is no scale to get wrong and nothing to re-derive
when the table changes.

⚠️ **There was a test here claiming to check that, and it could not fail** —
`if left.0 > right.0 { assert!(left > right) }` over tuples is a restatement of
lexicographic `Ord`, true for any values at all. It is deleted rather than
repaired: the guarantee is the type's, and a test of it is a test of the
standard library.

### Why doesn't quiescence probe the transposition table?

It should, and E1 owns it. The conventional expectation — quiescence is 91–99%
of all nodes, so probing there thrashes the table and evicts the interior
entries that pay — is **refuted on every row measured**: patching it in halves
the tree at roughly flat wall-clock (0.77× on the initial position, 0.50× and
0.47× on a drop-heavy middlegame).

It was not shipped at E0 on three grounds about the step rather than the
feature: it is a separable search change and this project runs one at a time;
the instrument that could say whether halving the tree makes the engine stronger
is SPRT, which did not exist yet; and it would rebaseline the counts `bench`
exists to freeze, in the same commit that froze them.

### Why can't an `Exact` hit strictly inside the window return in place of searching?

Because the node's line has been cleared and not refilled, so a parent that
raises alpha on it publishes one move followed by nothing.

⚠️ **That was predicted to truncate a published line, and measured, it does
not.** Letting the `Bound::Exact` arm cut unconditionally leaves every published
line **byte-identical** over three fixtures at depths one to eight, moving only
the node count and by at most 1.2%. The restriction is kept anyway: the path is
reachable, the argument is about soundness rather than performance, and 1% is
what it costs. An end-to-end test of a rule about an internal seam is a test of
whether that seam is on today's hot path, which is a different and less stable
question — so a unit test on the rule itself is what covers it.

### Why may an `Entry` not store a `Move`?

`Move` carries no `#[repr]`, so its size is unpromised. ⚠️ **"Unpromised" is
precisely the property no assertion about today's layout can test**: the
sixteen-byte test stayed green when a `Move` was stored, because `Option<Move>`
happens to fit the same sixteen bytes on today's compiler. Neither the rule nor
the test was wrong; the note joining them was. The limit applies again the moment
E1 or E3 adds a packed struct.

### Why is the root's returned value exact?

β is `INFINITE` so the root never fails high, and its children enter with α at
`-INFINITE` so they never fail low. That is what makes the early break on
`score.is_mate()` a break on a proof rather than a guess, for a loss as much as
for a win. **`pv[0]` can only ever be assembled from exact children.**

⚠️ That induction has one step that looks like a counterexample: a child that
fails *low* does reach `update_pv` at its parent — it hands the parent a score at
or above β — but doing so cuts the parent, so that parent is itself fail-high and
its line never lands on a published one. The truncated lines exist; none surface.

### Why can't `negamax_root` be given a narrow window?

It returns `None` on "no root move raised α", which is the same as "no root move
finished" only while α starts at `-INFINITE`. Given a narrow window an ordinary
fail-low comes back as `None` and the deepening loop breaks instead of
re-searching — no wrong answer announced, no panic, and no failing test, because
every test here searches an open window. A `debug_assert` pins it. E1's
aspiration-windows item owes the method a return type that tells the two apart.

### Why is 千日手 implemented at four occurrences rather than the two-fold cut most engines use?

Four occurrences is the rule; scoring the *first* repetition on the search path
as a draw is a **search heuristic**. It is standard practice and it prunes
cycles, but adopting it at E0 would have been adopting it on argument, with no
instrument for strength. It is an E1 item, where an SPRT can decide it.

⚠️ **The consequence shapes every test in the area**: twelve plies is more than
an E0 search sees, so the fourth occurrence is reached almost entirely out of the
*game's* history rather than from inside the tree. A fixture that exercises this
has to carry a real move list — a bare `sfen` root cannot test repetition at all,
and `bench` cannot exercise it either.

### Why does a 連続王手 win get a score band of its own rather than a mate score?

Reporting it as `score mate N` would announce a mate whose principal variation
does not deliver one — which `a_reported_mate_is_a_real_mate` exists to forbid.

### Why can a repetition verdict still come back out of the transposition table?

The verdict node's own key is never probed or stored, but the parent takes the
returned score as its `best` and stores it under a key that carries no path. So
a later probe can hand back a draw or a 連続王手 score for a position that no
repetition reached. This is an accepted imprecision, not a guarantee. The
unbuilt fix — propagating a "this subtree saw a repetition" flag up and declining
to store — is a separable search change with a node-count cost and no instrument
at E0 to weigh it; every engine lives with the imprecision. **Revisit** when E1
puts quiescence in the table, or when E2's Lazy SMP shares one.

### Why is `Game::clone`'s Zobrist cross-check only a `debug_assert`?

The concern is real — the history's keys come from shunsai's *incremental* key,
so a drift would corrupt a repetition verdict directly. What settles it the other
way is the cost of the alternative: `Game::clone` runs on the **protocol thread**,
once per `go`, so a real assertion puts a panic there, and bad input must never
stop the loop. Trading a quiet wrong verdict for a dead engine is not a trade
this project's error policy makes. **Revisit if** an incremental-key drift is
ever observed, or when E2's CSA client uses the record for an in-game decision.

### Why does `Game::search_board` exist?

`SearchJob` is public and E3's self-play driver is its second caller, so the
search's requirement must not depend on how the job was assembled. ⚠️ An earlier
version of this answer gave a different reason — the cost of deep-copying
shunsai's undo stack — which does not exist on the USI path. **A right decision
resting on a wrong reason is one refactor away from being deleted.**

Rejected: rebuilding the board from the record. What the rebuild bought was an
empty undo stack, and shunsai 0.1.0 has no stack at all, so the guarantee is now
structural. What replaced it is a copy of the game's own board, on a ground the
rebuild never had: the search seeds its repetition path from entries taken from
that board, so a second board agreeing only by assertion is two lineages compared
against each other.

### Why do some deliberately dead lines stay in the search?

Because they are reachable and unfalsifiable, and a future "delete the dead line"
needs something to check itself against. Deleting `negamax`'s own
`pv[ply].clear()` reproduces `depth`, `seldepth`, `score` and `pv`
byte-identically over 9 fixtures × depths 1–6, while `qsearch`'s copy is
observable. Scoring `negamax`'s mated node `mated_in(0)` leaves the whole suite
green, `bench` included, while `qsearch`'s copy fires three tests.

### Why is the move buffer one big allocation sliced per ply?

Rejected: `ArrayVec<Move, 593>` per ply on the stack — same size but *per node*,
and `Move` has no `Default`, so an `unsafe`-free version needs `MaybeUninit`,
which the workspace denies. `Vec<Move>` per ply — one malloc/free per node. A
ply-indexed array in the search stack, where E3's accumulator stack goes — it
leaves the move list borrowed while recursing on `&mut stack`.

### Why does allocating the transposition table happen at `isready` rather than at `go`?

⚠️ Because it costs about as much as a whole E0-depth search: 6–21 ms for
256 MiB against 13–20 ms for a depth-6 search from the initial position.
Harmless where it is — once per searcher, and `isready` may take arbitrarily
long — but a future change that moved it onto the `go` path would look free.

### Why does `score.rs` have surfaces with no caller?

It is the named-caller rule's one exception: a type that exists to freeze a
convention. A wrong negamax sign, centipawn scale, mate band or `MAX_PLY` is a
class of bug SPRT reads as "that patch was bad". Of the six surfaces it put on
probation, five gained a named caller and stayed; `Score::NONE`'s never turned
up and it went.

⚠️ **A named caller that does not turn up is a result too.** This file
predicted per-ply state would gain a static evaluation; quiescence computes its
stand-pat as a local and nothing reads it again. The `Stack` struct it also
predicted did turn up, when E1's killers gave the search a second per-ply field
to keep beside the line.

## Time control

### Why is the allowance byoyomi-shaped rather than the chess `remaining / divisor + increment`?

A referee grants the byoyomi period afresh each move and charges main time only
with the excess, so the period is free. The chess formula answers
`btime 0 byoyomi 10000` — a long game's ordinary endgame — with an instant move
and ten unused seconds.

### Why does `Clock::now` return a `Duration` rather than an `Instant`?

`Instant` has no public constructor, so a trait returning one can only be
implemented by something that calls `Instant::now` — the one thing the seam
exists to make optional. Rejected also: `Budget<'a, C>` carrying the clock —
built from `&self.clock` it holds a shared borrow of `*self` across a deepening
loop that needs `&mut *self` (E0502), escapable only by `C: Clone` and a clone
per search.

### Why does `DeliveryMargin` bound the allowance rather than reduce it?

⚠️ **This was adopted in the reducing form and reverted**, and the argument for
it is good enough to be made again: inside the cap the margin does nothing while
main time remains, which reads as a bug. Both forms uphold
`allowance ≤ available − margin`; the reducing form additionally surrenders the
margin on every move, and measured, a 900 ms clock at the default margin then
gave an allowance of zero and stopped the engine at depth 1.

### Why isn't `DeliveryMargin` tuned by SPRT?

Its right value is a property of the deployment, not of the engine — microseconds
on localhost, a round trip over a network — and self-play on one quiet machine
drives it to zero, which is the answer that loses games over a network.

### Why does the harness cut the wait at the mover's allowance instead of comparing elapsed afterwards?

A flag fall *is* the wait ending without an answer. An answer that arrived is
played however much of the allowance it took, and the elapsed the seat reports is
measured to the instant the line arrived, so it can never exceed the bound.

- **Rejected: judging on `elapsed >= allowance`.** It decides the boundary on the
  harness's own work — the elapsed then included the stale-line drain, both
  writes and the `bestmove` parse — so an answer delivered inside the allowance
  could still be charged past it and the legal move discarded. Reproduced at
  300 µs of margin in 1 243 of 2 000 trials. It also routed every engine failure
  through the clock, so a crash arriving late became a flag fall and the run lost
  its ⚠️ tally and stderr post-mortem.
- **Rejected: waiting under the hang timeout and comparing afterwards.** The
  timeout would have to be set above the whole clock it is watching, and the
  overrun it would have to clear is the quantity being measured. A detector tuned
  against its own subject is not one.
- **Rejected: a grace window.** ⚠️ Measured before step 5b, rinsai overran every
  byoyomi it used by a constant ~0.14 ms — the poll interval, independent of the
  byoyomi from 1 ms to 1000 ms. A grace wide enough to absorb that is a gate
  scoring itself. The engine's delivery margin later removed the overrun; the
  grace window stays rejected on the other argument.

**Reopens if** the harness ever has to stand in for a server that grants a
delivery margin of its own.

## The harness and measurement

### Why isn't Ayane vendored as the match harness?

Read at `yaneurao/Ayane@5fc6afd`, it lacks all three things the gates need.
**No fixed-node play** — `set_time_setting` accepts nine time tokens and nothing
else, and the `go` line it builds is always clock-shaped. **No 千日手
adjudication** — its own comment says a repetition draw is expected to leave via
`max_moves`, capped at 320 where floodgate plays 512. **No legality checking** —
a `bestmove` is string-concatenated onto the position, and `GameResult.ILLEGAL_MOVE`
is declared but never assigned. Patching in nodes support and wrapping an
external repetition referee around it would leave Ayane contributing
process-spawning — which `usi_process.rs` already does in-tree — at the price of
a second toolchain in the measurement loop. CLAUDE.md's permission to vendor it
stays true and stays unused.

### Why is an opening candidate judged on an iteration that finished?

The last `info` line published may belong to an iteration the node cap
interrupted, whose score is the best over a *prefix* of the root list. 150 of
`openings-v1`'s 256 lines were selected that way.

⚠️ **"A lower bound admits what a completed search rejects, and never the
reverse" was false**, and stood through two reviews: the window is two-sided, so
a lower bound errs in both directions, and the reverse direction is the one that
moved four of the eight lines checked.

Rejected: dropping any candidate whose balance search hit the cap. It keys on
tree size, so only positions whose depth-6 search fits survive — and the open,
capture-rich middlegames that removes are exactly where E1's ordering and pruning
are meant to pay. **Reopens if** the balance search stops being a rinsai search,
or the window stops being two-sided.

### Why is a generated set traced by a tag rather than by the commit that added it?

⚠️ **The expectation was the reverse.** The adding commit is on `main` and its
tree looked like it must contain the generator. It does not: review commits land
between generating a set and merging it, so the adding commit carries a *later*
generator — measured, `openings.rs` differs by 83 lines for v1 and 9 for v2
between the two. A handle that is easy to reach and wrong is worse than the rev.

Rejected also: relying on the branch surviving, which is what had been holding v1
and v2 together without anybody deciding it. This repository has never made a
merge commit, so a set's generating rev is never on `main`, and the two older
revs were reachable only because their branches were never deleted.

### Why does upgrading shunsai not move the `bench` counts?

⚠️ **It was predicted to move all of them**, on the ground that shunsai's
generation order is unspecified and the release was several commits and two
measured speedups ahead of the pin. Measured at depth 4 and 16 MiB: all seven
counts unchanged, and the openings fixture reproduces byte for byte. **A count
depends on the order and the membership of the generated list**, and a faster way
to reach the same list moves neither.

### Why does `bench` keep its baseline counts in the source rather than in a document?

A regression baseline has to be executable to be a regression test, and a copy of
it in a document is a second copy that drifts.

## Protocols

### Why isn't the USI layer shared with `tuishogi`?

USI is asymmetric: an engine **reads** what a GUI sends and **writes** its
answers, and a GUI does exactly the reverse. The function that parses
`go btime 1000` and the function that writes it are two functions, and having
either does not give you the other. rinsai holds the engine half and only that,
so an extraction hands tuishogi nothing it can call.

So this was never "extract the USI layer" but "write a new crate holding both
directions of both types". **A shared type is the *union* of both sides' needs
rather than the intersection** — rinsai's deliberately lossy `GuiCommand`
variants are each small and together a permanent tax on both consumers.

**Reopened when** a dialect bug found in one repository turns out to have a twin
in the other, or a second Rust consumer of either half appears.

### Why not use the `usi` crate?

`usi` 0.6.2 (nozaq/usi-rs) is MIT and **does not depend on `shogi_core` at all**
— its moves are strings. It offers `EngineCommand::parse`, `Info` and
`BestMoveParams { MakeMove, Resign, Win }`, the last carrying 入玉宣言. Its
`GuiCommand` can be written but not read, and reading it is the half an engine
needs. Same asymmetry, from the other side.

### Why is `rinsai-protocol` reserved as a session layer rather than a codec?

Rejected: splitting it on "the line-oriented loop the CSA client would share with
USI". That loop is a few lines of framing, and CSA shares nothing else — not the
grammar, not the move notation (`+7776FU` against `7g7f`), not the state machine.
What the two protocols genuinely share is the layer *underneath*: the same
`SearchDriver`, `Game` and time management, and "one answer per turn,
structurally".

### Why isn't `shogi_core::Position::from_usi` used for the `moves` list?

⚠️ **The reason first recorded for it was itself false.** "It never reports an
error" was written in this file and in the code; measured, a malformed token does
error, a well-formed but unmakeable move is silently dropped, and a structurally
fine but *illegal* one is applied. The decision is unchanged and
`crates/rinsai-search/tests/shogi_core_from_usi.rs` pins the behaviour, so the day
`shogi_core` changes it this is revisited rather than inherited. **The habit it
enforces: a rejection or a guarantee recorded from *reading* source is a claim,
not a finding.**

### Why is there a process-level protocol test as well as the in-process dialogues?

An in-process test cannot see a bug whose symptom is timing between two threads.
The conformance dialogues feed a whole script at once, so a genuine overlap is
indistinguishable from the `go`-after-`go` bug E0 step 1 shipped. `usi_process.rs`
plays a properly sequenced game through the real binary over real pipes and
requires stderr to be empty — and that decides where E2's CSA tests live.

Related: **a conformance dialogue cannot exercise a search of a stated size**,
because `quit` stops the search and `usi::run` reads input as fast as it arrives,
so `printf … | rinsai` answers from the depth-1 iteration. Size-dependent tests
belong in `rinsai-search` or in `usi_process.rs`.

### Why does `Outcome` have six small setter methods instead of one taking an `Outcome`?

A public setter would hand a caller the three endings `play` *derives* — 詰み,
千日手, 連続王手の千日手 — and a caller that sets one of those asserts what the
crate exists to check. Six methods buy the property that a declared ending and a
derived one cannot be spelled the same way. Rejected also: a second enum for the
declared endings converted into `Outcome` — two vocabularies and a conversion,
where both consumers match on `Outcome` at the end of it anyway.

### Why isn't `EndReason` collapsed onto `Outcome`?

The harness keeps `Died`, `Protocol` and `Timeout` apart because the run summary
tallies them apart; `Outcome::Abandoned` is one variant for all three, so the
collapse would delete what a degraded run prints about itself. **Reopens if** the
summary stops needing the three apart, or `Outcome` grows the distinction.

⚠️ A downstream that only `Display`s an `Outcome` compiles clean against a new
variant and renders a sentence nobody chose; `#[non_exhaustive]` does not help,
because the hazard is the absent `match` rather than an exhaustive one. Nothing
here can fix that for a consumer.

### Why is `Engine::go`'s `submit`-failed branch untested?

Reaching it needs a worker that is gone while the loop still runs. It is kept
because it guards the one-answer-per-`go` invariant, and named here rather than
left to be discovered.

## Portability and dependencies

### Why is wasm a constraint with no build behind it?

No web application asks for one, and a `cfg(target_arch = "wasm32")` branch
nobody exercises rots faster than an absent one. What lands is CONVENTIONS.md's
Portability rule and a CI step keeping `rinsai-search` compiling for the target.
Rejected in the shape it would take: a **threaded** build, which costs nightly
and COOP/COEP on the host to optimise the weakest build for strength; and a
**typed JS API** in place of `step(line) -> Vec<String>`, which would re-encode
the protocol into a second one covered by one set of tests. **Revisit when** the
browser stops being of interest, or when honouring the rule costs measurable Elo
natively — in which case the native engine wins.

⚠️ Two things worth knowing before relying on them there. `catch_unwind` is
inert: the target is `panic = "abort"`, so "a panicking search answers `resign`
and the engine plays on" is a **native-only** property. And **a trap located by
reading the type that owns the concept will miss the caller that reads it for
another reason** — the clock trap was found by reading `Budget`, the only place a
*deadline* lives, while the unconditional `Instant::now()` one scope up went
unchecked, and it is what makes `go depth 6 nodes 3000000` panic before a node is
searched.

### Why can't a browser build just ship the network?

The constraint is **download size, not arithmetic**: the E3 network's feature
transformer alone is far larger than a browser download can be, and halving it
would not change the conclusion, so compression is not the lever. Neither is GPU
offload — NNUE's cost is an incremental accumulator update of a few columns, αβ
evaluates positions strictly sequentially so there is nothing to batch, and a
dispatch costs more than the whole evaluation. WebGPU is the right instrument for
the E6 branch and not for this one. So a browser build's **evaluation** forks
from the native one, which the native design already allows for; the three E3
consequences of that are tracked as #42.

### Why not `web-time` for the clock?

It is `MIT OR Apache-2.0` and a genuine drop-in for `Instant`, but on wasm it
pulls `js-sys` and `wasm-bindgen` and their trees into a lock file whose whole
third-party set is three crates, every one of them provenance-scan surface. A
`cfg`-selected clock adds none. **Adopt if** a browser build ever needs a
deadline *inside* the engine that the host cannot express as depth or nodes.

### Why threads rather than async, and why no `tokio`?

One stdin reader and one CPU-bound search do not need a runtime, and
`spawn_blocking` would put the search back on a thread anyway — for a dependency
tree that is provenance-scan surface. **Reopens if** something inside the engine
process has to wait on *many* independent I/O sources at once; E2's Lazy SMP,
E2's single CSA connection and E3's in-process self-play are not that.

### Why not `[patch.crates-io]` during shunsai prototyping?

It **does** work for an unpublished crate — cargo's own
`tests/testsuite/patch.rs::nonexistent` covers it. Rejected on three other
grounds: the manifest's `shunsai = "0.1"` would assert something false; `[patch]`
is honoured only at the workspace root, so `rinsai-search/Cargo.toml` would stop
saying where shunsai comes from; and it would keep overriding silently after
publication, so the switch we are trying not to forget would never go red.

## Tooling

### Why is there no prose hook, reviewer agent or `/create-pr` command?

There was, as a plugin, and it was dropped. Its review loop ran ten passes on
one branch and **five changed no executable line at all**; the command driving
it said "fix the sentences and return to step 1", which has no stopping
condition. Against that, its mechanical checker found nothing. The rules survive
as six imperatives in CLAUDE.md.

Rejected: moving the plugin into `.claude/hooks` and `.claude/agents`, which is
what tuishogi does and would have worked in the cloud — the machinery was the
cost, not its location. Rejected also: publishing the marketplace to GitHub,
which keeps one shared copy for three repositories and keeps the loop.

**Reopens if** false claims start reaching `main` at a rate the six imperatives
do not hold down.

### Why does no source file, manifest or generated artifact cite a document here?

It does not resolve in `cargo doc`, and `rinsai-game`'s manifest — the one
publication-intended crate — would ship its citation to crates.io. 73 such
references were removed, and CI's `FILE.md §N` citation guard with them. State
the rule where it is needed, or not at all.
