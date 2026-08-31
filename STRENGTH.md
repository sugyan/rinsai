# rinsai — what the games have said

Every strength claim this engine makes, and the games behind it. Newest first.

**The admission test: a result belongs here when re-deriving it means playing
games.** That is the one kind of measurement a checkout cannot reproduce — it
needs two binaries and hours of machine time — so it is the one kind that has
to be written down rather than re-run.

Everything a `cargo` command reproduces stays out — `bench`'s node counts are
frozen in its own `EXPECTED`, and a sweep somebody can repeat in a minute is
recorded as how to repeat it. ⚠️ **Node count is not an instrument for strength**, so a tree that
got smaller is not an entry here.

**A loss is an entry too**, and so is a run that turned out not to measure what
it claimed. Both are what stop the same ground being covered twice.

**What an entry carries**, because a number without its conditions cannot be
compared against another one: the two revs and their `bench` fingerprints, the
control and the opening set, the bounds, and the counts the run stopped on.
⚠️ **Never how long the run took.** A machine that was not quiet times
nothing, and only a fixed-node result is machine-independent. A *time control*
is different: it is one of the conditions, and a gate played under one records
it like any other.

---

## E1 — classical search, one feature at a time

### Item 5 — late move reductions for quiet moves — **pass**

H1 accepted at elo0 = 0 / elo1 = 10.

```
pairs 119 | games 238 | candidate W-D-L 119-66-53
pent [5, 2, 72, 2, 38] | llr +3.074 | score 63.87% (elo +98.9 est)
```

| | |
|---|---|
| candidate | `923972c`, replayed by a rebase as `a80bcf5` |
| baseline | `eb58a6f` |
| `bench` | candidate 232 082, baseline 272 244 — both read off the binaries that played, before the run |
| control | `--nodes 300000`, elo0 = 0 / elo1 = 10, α = β = 0.05, concurrency 4 |
| openings | `openings-v3.sfen`, seed 1, 119 distinct openings for 119 pairs |
| endings | 172 checkmates, 66 千日手, none at the 512-ply move limit |

The counts above were recomputed from `run.jsonl` rather than read off the
harness's summary, and agree to the digit. The two revs differ only in
`crates/xtask`, which is the harness rather than a player: the rebase carried
the branch over the working-gate change, and the engine crates are identical
between them.

⚠️ **The file predicted the opposite of this, twice.** Step 3's entry closes
with "E1's later items are the smaller ones", and the gate argument repeats it.
Item 5 is the largest effect E1 has measured after step 1, and it crossed in
**119 pairs** against 763 for step 2, 1937 for step 3 and 1857 for the
calibration. The ordering of the remaining items by size is not known, and
saying it is has now been wrong once.

**60.5% of pairs were even** — 72 of 119, against 77% at step 3 and 78.2% at
the calibration. Nothing about the opening set changed; a large effect is what
makes pairs decisive, which is the same quantity #65 proposes to buy from the
openings instead.

⚠️ **No game reached the move limit**, where step 3 had 35 in 3874 and the
calibration 25 in 3714. 238 games is too few to read that as a property of the
patch.

The bound was crossed and stayed crossed: first at +2.963 as the 116th pair
landed, then +3.039, +3.057, +3.074. ⚠️ Unlike step 3 and the calibration, the
recomputation over all 119 pairs agrees with the decision that stopped the run
— which is the outcome those entries warned is not guaranteed, not a change in
what is guaranteed.

⚠️ **What a pass says: the true difference is positive at α = 0.05, and the
gate had 95% power at +10.** The +98.9 is a point estimate from 119 pairs; it
pins the sign firmly and the size loosely.

### The working-gate calibration — step 3 re-measured at (0, 10) / 300 000 nodes — **pass**

H1 accepted at elo0 = 0 / elo1 = 10.

```
pairs 1857 | games 3714 | candidate W-D-L 1435-938-1341
pent [158, 20, 1453, 22, 204] | llr +2.919 | score 51.27% (elo +8.8 est)
```

| | |
|---|---|
| candidate | `4b49de5` |
| baseline | `a8ce924` |
| `bench` | candidate 272 244, baseline 276 470 — both read off the binaries that played, before the run |
| control | `--nodes 300000 --gain` (elo0 = 0 / elo1 = 10), α = β = 0.05, concurrency 3 |
| openings | `openings-v3.sfen`, seed 1, 1857 distinct openings for 1857 pairs |
| endings | 2776 checkmates, 913 千日手, 25 at the 512-ply move limit |

The counts above were recomputed from `run.jsonl` rather than read off the
harness's summary, and agree to the digit.

**What this entry is.** The first run of the working gate CLAUDE.md now
prescribes, played on the same two binaries as the step 3 entry below — the
same feature, under the new control. It does not revisit that verdict; it
answers whether the 300 000-node regime preserves what the 1 M regime
measured, on the smallest effect E1 has landed. It does.

⚠️ **What a pass says under the widened gate: the true difference is positive
at α = 0.05, and the gate had 95% power at +10.** The +8.8 point estimate sits
below elo1, which a pass permits — "at least elo1" was never the guarantee,
only the simple-vs-simple idealisation of it.

⚠️ **The final LLR sits below the bound the run stopped on.** The bound was
crossed at +2.994 with three pairs in flight; two landed even and one a
loss-draw, and the recomputation over all 1857 pairs reads +2.919. Step 3's
entry warned that the recomputation need not agree with the stopping decision;
this is the run where it did not. The verdict is the decision that stopped the
run.

⚠️ **The cheaper game is the whole saving — the effect did not get easier to
see.** 78.2% of pairs were even against 76.7% at 1 M, 千日手 24.6% of games
against 26.0%, and the pair count barely moved (1857 against 1937): with elo1
at the effect's own size, the bounds bought little here and were not expected
to — an effect at the gate's own bound is the grind case at any gate that
resolves it. What fell was the cost of each pair.

千日手 games are the short ones at this budget — median 41 plies against 131
for a checkmate — so an adjudication rule aimed at repetition games would save
nothing here; the 25 move-limit games are where the long tail lives.

### Step 3 — history for quiet moves, indexed by (side, kind, destination) — **pass**

H1 accepted at elo0 = 0 / elo1 = 5.

```
pairs 1937 | games 3874 | candidate W-D-L 1473-1041-1360
pent [168, 27, 1485, 38, 219] | llr +2.954 | score 51.46% (elo +10.1 est)
```

| | |
|---|---|
| candidate | `4b49de5` |
| baseline | `a8ce924` |
| `bench` | candidate 272 244, baseline 276 470 — both read off the binaries that played |
| control | `--nodes 1000000 --gain`, α = β = 0.05, concurrency 4 |
| openings | `openings-v3.sfen`, seed 1, 1937 distinct openings for 1937 pairs |
| endings | 2833 checkmates, 1006 千日手, 35 at the 512-ply move limit |

The counts above were recomputed from `run.jsonl` rather than read off the
harness's summary, and agree with it to the digit.

⚠️ **The bound was crossed and then uncrossed before the run stopped.** Over
the last six pairs to land the LLR read +2.933, +3.003, +2.926, +2.891 and
+2.954 — above the bound, below it twice, then above. The verdict is the
decision that stopped the run, not a recomputation, and the same recomputation
over all 1937 pairs happens to agree here. ⚠️ **It need not have.**

⚠️ **A +5 gate on a +10 effect is the expensive case.** Step 2 crossed in 763
pairs on a candidate whose point estimate was +29.9; this one needed 1937 for
+10.1, and 1485 of those pairs — 77% — were even. The pair count a gate needs
grows as the effect approaches its bound, and E1's later items are the smaller
ones.

⚠️ **What it says is that the true difference is at least 5 elo at α = 0.05.**
The +10.1 is a point estimate and is not the claim.

A first attempt was interrupted at 1560 pairs, undecided at +1.157, when the
machine running it restarted. It is not a separate result: every one of those
1560 pairs appears in the run above with the same opening and the same two
game results, none mismatching, so they are a prefix of these 1937 and nothing
is recorded twice.

⚠️ **The games replayed; the order they landed in did not.** Under
`--concurrency` the pairs are a race, and the two runs completed the same pairs
in different orders — so the running LLR the harness prints is order-dependent
even though the games are not. Two runs of one seed can therefore stop at
different pair counts, and the count is a condition of the verdict rather than
a property of the patch.

### Step 2 — killers, and the `Stack` the second per-ply field earns — **pass**

H1 accepted at elo0 = 0 / elo1 = 5.

```
pairs 763 | games 1526 | candidate W-D-L 622-413-491
pent [73, 23, 507, 20, 140] | llr +3.021 | score 54.29% (elo +29.9 est)
```

| | |
|---|---|
| candidate | built from `981ad27` |
| baseline | `060a197` |
| `bench` | candidate 276 470, baseline 474 941 — both read off the binaries that played |
| control | `--nodes 1000000 --gain`, α = β = 0.05, concurrency 3 |
| openings | `openings-v3.sfen`, seed 1, 763 distinct openings for 763 pairs |
| endings | 1113 checkmates, 397 千日手, 16 at the 512-ply move limit |

The counts above were recomputed from `run.jsonl` rather than read off the
harness's summary.

⚠️ **Sixteen games ended at the move limit rather than at a result.** The
harness does not count those as abnormal — its tally is for a seat that broke
the protocol, hung or died, and there were none of those — but step 1's
eighteen-game run reached the limit no times, and a limit game is a pair the
openings did not decide.

⚠️ **What it says is that the true difference is at least 5 elo at α = 0.05.**
The +29.9 is a point estimate; unlike step 1's +492 from nine pairs, 763 pairs
pin it reasonably well, but the estimate is still not the claim.

**The two E1 verdicts are the same word and not the same size.** Step 1 crossed
the bound in nine pairs; this took 763, for a gain two orders smaller. The patch
that cut `bench` to 0.56× is worth about a thirtieth of the one that cut it to
0.11×, which is what "node count is not an instrument for strength" costs when
it is ignored.

⚠️ **The early trend predicted the wrong verdict.** The LLR read +0.175 at 19
pairs, +0.001 at 67 and **−0.012 at 93** before climbing to the upper bound.
Anyone watching a run in progress should read it as undecided, not as trending.

The binary that played is one commit behind the branch head. The difference in
the engine is a single line — `Stack::default()` giving the per-ply line a
capacity of zero, restored to `Vec::with_capacity` — plus test and doc changes.
Rather than argue it changes nothing, twenty pairs were replayed at the same
seed with a build of `c6d6c68`: all forty games match the recorded run on
opening, colour, result, end reason, ply count **and every move played**. ⚠️
That is evidence of identical behaviour on this workload, not a second
independent measurement of the gain.

### Step 1 — MVV-LVA for captures, promotion-aware — **pass**

H1 accepted at elo0 = 0 / elo1 = 5.

```
pairs 9 | games 18 | candidate W-D-L 16-2-0
pent [0, 0, 0, 2, 7] | llr +2.643 | score 94.44% (elo +492.2 est)
```

| | |
|---|---|
| candidate | `1aab003`, review fixes in `4cc159b`, merged as `060a197` |
| baseline | `ddab7d1` |
| `bench` | candidate 474 941, before and after the review fixes — ⚠️ the baseline's was not recorded, and is re-derived by building `ddab7d1` |
| control | `--nodes 1000000 --gain`, α = β = 0.05 |
| openings | `openings-v3.sfen`, seed 1, nine distinct openings for nine pairs |
| abnormal | none: sixteen checkmates and two 千日手 |

The bound was crossed at +3.917 while the final pair was still in flight; it
landed afterwards, and the LLR is not monotone over the accumulated counts. The
verdict is the decision that stopped the run, not a recomputation.

⚠️ **What it says is that the true difference is at least 5 elo at α = 0.05. It
does not say the difference is 492** — that is a point estimate from nine pairs,
which pins nothing but the sign.

Re-run against the rebuilt binary after the review fixes: game-for-game
identical on opening, colour, result, end reason and ply count. That confirms
the fixes changed no behaviour; it is not independent evidence about the size of
the gain, because both runs share a seed.

## E0 — baseline

### The retroactive non-regression audit — **pass**

The finished E0 is not a regression against the step-3b engine. H1 accepted at
elo0 = −5 / elo1 = 0.

```
pairs 697 | games 1394 | candidate W-D-L 487-428-479
pent [7, 35, 599, 55, 1] | llr +2.967 | score 50.29% (elo +2.0 est)
```

| | |
|---|---|
| candidate | the finished E0 |
| baseline | the step-3b engine |
| control | `--nodes 1000000 --non-regression`, α = β = 0.05 |
| openings | `openings-v3.sfen`, 697 distinct openings for 697 pairs |
| abnormal | none |

The harness's own numbers were recomputed independently from `run.jsonl` and
agree to four decimals.

⚠️ **Neither engine is recorded by rev, and neither `bench` fingerprint was
kept.** This entry is as precise as its source allows; the fields above exist so
that the next one is better.

Two limits on what the verdict covers:

- **Step 5b's time management is not in it, and cannot be** — under `--nodes`
  the engine derives no deadline. The clock gate below is what covers that.
- **The relief narrowing is not fully inert even at 1 M nodes.** In about
  0.016% of moves the baseline's whole first iteration exceeds the budget and it
  plays on where the candidate stops; baseline moves over 10× the median: 9,
  candidate: 1. The residual favours the baseline, so a pass is the conservative
  direction.

### The fifty-game clock gate — **pass**

50 games under a real clock, 200 ms byoyomi. **Zero flag falls**, with both
engines searching to depth 7.

⚠️ **The main time is recorded two ways and they disagree** — 30 s where the
audit reports this gate, 300 s where a later measurement quotes the same run by
its flags (`--time-ms 300000 --byoyomi-ms 200`, 4 844 clocked moves). One of the
two is wrong and the run itself is gone. Re-derive it before quoting it.

The same run also measured what a clocked game costs at its start, and that is
kept here rather than only in the defect it opened, because re-deriving it means
playing the fifty games again: the first move of a game overshoots the byoyomi
by a median of 14.3 ms against 0.134 ms for every other move, and is the worst
move of its game in 27 of the 50. ⚠️ **Whichever fix is chosen, this
distribution is the instrument** — the first move has to collapse onto the
median of the rest.

### Rejected — the audit's first attempt

Not a result. 2146 pairs played over `openings-v2.sfen`'s 256 lines, so laps
2–9 replayed the first move for move. The pentanomial LLR sizes evidence by the
pair count and cannot see a replay, so the run reported **+3.0199 against a
±2.9444 bound** — a decision — where the 256 distinct observations carried
**+0.3109**, a tenth of the way to one.

⚠️ **This is why a fixed-node run may not outrun its opening set**, and why
`openings-v3.sfen` exists. The harness refuses the shape now, so the same
mistake cannot be made twice by accident — but the number above is the reason
the check is there, and it is the kind of number that reads like a pass.
