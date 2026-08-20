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
