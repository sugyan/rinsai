//! Sequential probability ratio test over colour-swapped game pairs.
//!
//! The unit of observation is a **pair** — one opening played twice with the
//! colours exchanged — so the model is pentanomial: a pair's outcome is the
//! candidate's points over its two games, one of `{0, ¼, ½, ¾, 1}`. Pairing
//! is what absorbs an unbalanced opening (its bias cancels within the pair),
//! and modelling the pair, not the game, is what keeps the variance estimate
//! honest about that correlation.
//!
//! The statistic is the generalised SPRT's normal-approximation LLR over the
//! empirical pair distribution, written from the public GSPRT literature:
//! with score coefficients `a = (0, ¼, ½, ¾, 1)`, empirical frequencies
//! `p̂ᵢ = Nᵢ/N` over `N` pairs, `φ̂ = Σ aᵢ p̂ᵢ` and per-mean variance
//! `V = (Σ aᵢ² p̂ᵢ − φ̂²) / N`,
//!
//! ```text
//! LLR ≈ (φ₁ − φ₀) · (2 φ̂ − φ₀ − φ₁) / (2 V),   φₖ = 1 / (1 + 10^(−eloₖ/400))
//! ```
//!
//! stopping at `ln(β/(1−α))` (accept H0) or `ln((1−β)/α)` (accept H1). The
//! test parameters themselves are CLAUDE.md §3's and arrive from the caller.

/// The expected score at an Elo gap — the logistic curve USI engines' rating
/// lists assume.
#[must_use]
pub fn phi(elo: f64) -> f64 {
    1.0 / (1.0 + 10f64.powf(-elo / 400.0))
}

/// An Elo point estimate for a score, the inverse of [`phi`]. Clamped away
/// from 0 and 1 so a perfect score reports a large finite number, not an
/// infinity.
#[must_use]
pub fn elo_estimate(score: f64) -> f64 {
    let s = score.clamp(1e-6, 1.0 - 1e-6);
    -400.0 * (1.0 / s - 1.0).log10()
}

/// Pair-outcome counts. `counts[k]` is the number of pairs in which the
/// candidate took `k` half-points over the pair's two games — `0` a double
/// loss, `2` one point (a win and a loss, or two draws), `4` a double win.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PairCounts {
    pub counts: [u64; 5],
}

impl PairCounts {
    /// Record one pair by the candidate's half-points over it (`0..=4`).
    ///
    /// # Panics
    /// If `half_points > 4` — the caller computed a pair score no pair of
    /// games can produce.
    pub fn record(&mut self, half_points: usize) {
        assert!(half_points <= 4, "a pair holds at most 4 half-points");
        self.counts[half_points] += 1;
    }

    #[must_use]
    pub fn pairs(&self) -> u64 {
        self.counts.iter().sum()
    }

    /// The candidate's score over all pairs, `φ̂` — `0.5` for an even match.
    /// `0.5` on an empty sample, so a status line can print before pair one.
    #[must_use]
    pub fn score(&self) -> f64 {
        let n = self.pairs();
        if n == 0 {
            return 0.5;
        }
        let points: f64 = self
            .counts
            .iter()
            .enumerate()
            .map(|(k, &c)| k as f64 * 0.25 * c as f64)
            .sum();
        points / n as f64
    }
}

/// The GSPRT log-likelihood ratio for H1 (`elo1`) against H0 (`elo0`).
///
/// A degenerate sample — no pairs, or every pair in one cell, which leaves
/// the variance estimate at zero — reports `0.0`: it carries no usable
/// directional evidence under this model, and the caller's pair cap is what
/// terminates a test that stays degenerate. A mirror match of a deterministic
/// engine against itself sits in this case for its whole run.
#[must_use]
pub fn llr(counts: &PairCounts, elo0: f64, elo1: f64) -> f64 {
    let n = counts.pairs();
    if n == 0 {
        return 0.0;
    }
    let n = n as f64;
    let mut mean = 0.0;
    let mut second = 0.0;
    for (k, &c) in counts.counts.iter().enumerate() {
        let a = k as f64 * 0.25;
        let p = c as f64 / n;
        mean += a * p;
        second += a * a * p;
    }
    let variance_of_mean = (second - mean * mean) / n;
    if variance_of_mean <= 0.0 {
        return 0.0;
    }
    let phi0 = phi(elo0);
    let phi1 = phi(elo1);
    (phi1 - phi0) * (2.0 * mean - phi0 - phi1) / (2.0 * variance_of_mean)
}

/// `(lower, upper)` = `(ln(β/(1−α)), ln((1−β)/α))`.
#[must_use]
pub fn bounds(alpha: f64, beta: f64) -> (f64, f64) {
    ((beta / (1.0 - alpha)).ln(), ((1.0 - beta) / alpha).ln())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    AcceptH0,
    AcceptH1,
    Continue,
}

#[must_use]
pub fn status(llr: f64, alpha: f64, beta: f64) -> Status {
    let (lower, upper) = bounds(alpha, beta);
    if llr <= lower {
        Status::AcceptH0
    } else if llr >= upper {
        Status::AcceptH1
    } else {
        Status::Continue
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng;

    const GAIN: (f64, f64) = (0.0, 5.0);

    #[test]
    fn phi_maps_a_zero_gap_to_an_even_score_and_is_antisymmetric() {
        assert!((phi(0.0) - 0.5).abs() < 1e-12);
        for elo in [5.0, 100.0, 400.0] {
            assert!((phi(elo) + phi(-elo) - 1.0).abs() < 1e-12);
        }
        assert!((elo_estimate(0.5)).abs() < 1e-9);
        assert!((elo_estimate(phi(123.0)) - 123.0).abs() < 1e-6);
        assert!(elo_estimate(1.0).is_finite() && elo_estimate(0.0).is_finite());
    }

    /// The LLR of one concrete sample, against the formula spelled out step
    /// by step with its intermediate values — a transposition in the
    /// implementation (swapped hypotheses, a dropped factor of two, variance
    /// of the sum instead of the mean) lands visibly off.
    #[test]
    fn the_llr_of_a_hand_computed_sample_matches_the_formula() {
        let counts = PairCounts {
            counts: [0, 10, 60, 25, 5],
        };
        // phi-hat = .25(.1) + .5(.6) + .75(.25) + 1(.05) = 0.5625
        assert!((counts.score() - 0.5625).abs() < 1e-12);
        // second moment = .0625(.1) + .25(.6) + .5625(.25) + 1(.05) = 0.346875
        // variance of the mean = (0.346875 - 0.5625^2) / 100
        let v = (0.346875 - 0.5625 * 0.5625) / 100.0;
        let (phi0, phi1) = (phi(GAIN.0), phi(GAIN.1));
        let expected = (phi1 - phi0) * (2.0 * 0.5625 - phi0 - phi1) / (2.0 * v);
        let got = llr(&counts, GAIN.0, GAIN.1);
        assert!((got - expected).abs() < 1e-12, "{got} vs {expected}");
        // And the coarse magnitude, independent of both computations above.
        assert!((got - 1.391).abs() < 1e-3, "{got}");
    }

    /// ⚠️ **Replicating a sample multiplies the LLR by the replication factor,
    /// and this function has no way to know it happened** — the score is
    /// unmoved, so nothing else reports it. It is a property of the statistic,
    /// not a defect here: the estimator is told `n` independent pairs and there
    /// is no such thing as a duplicate pair to it.
    ///
    /// What makes it reachable is a caller playing more pairs than its opening
    /// set holds against deterministic engines, where a replayed opening is a
    /// replayed game. `runner::check_budget_fits_openings` is the guard, and
    /// this test is the arithmetic it cites.
    #[test]
    fn replication_multiplies_the_llr_it_should_not_move() {
        let once = PairCounts {
            counts: [12, 13, 199, 26, 6],
        };
        for factor in [2u64, 8, 100] {
            let replicated = PairCounts {
                counts: once.counts.map(|c| c * factor),
            };
            assert_eq!(
                replicated.score(),
                once.score(),
                "replication moves no score, which is why it is silent"
            );
            let (a, b) = (llr(&once, -5.0, 0.0), llr(&replicated, -5.0, 0.0));
            assert!(
                (b - a * factor as f64).abs() < 1e-9,
                "{factor}x: {b} against {a} scaled"
            );
        }
    }

    /// Pairs that never split a point are a trinomial over {0, ½, 1}; the
    /// pentanomial formula must agree with that reindexed instance exactly.
    #[test]
    fn a_pentanomial_with_empty_quarter_cells_agrees_with_the_trinomial_formula() {
        let (l, d, w) = (31u64, 42u64, 27u64);
        let counts = PairCounts {
            counts: [l, 0, d, 0, w],
        };
        let n = (l + d + w) as f64;
        let mean = (0.5 * d as f64 + w as f64) / n;
        let second = (0.25 * d as f64 + w as f64) / n;
        let v = (second - mean * mean) / n;
        let (phi0, phi1) = (phi(GAIN.0), phi(GAIN.1));
        let tri = (phi1 - phi0) * (2.0 * mean - phi0 - phi1) / (2.0 * v);
        let penta = llr(&counts, GAIN.0, GAIN.1);
        assert!((penta - tri).abs() < 1e-12, "{penta} vs {tri}");
    }

    /// One draw-pair first so the variance estimate is never zero; after it,
    /// a run of double wins must accept H1, and its mirror H0.
    #[test]
    fn one_sided_streams_cross_their_bounds() {
        let mut up = PairCounts::default();
        up.record(2);
        let mut down = up;
        let mut accepted = (false, false);
        for _ in 0..10_000 {
            up.record(4);
            down.record(0);
            if status(llr(&up, GAIN.0, GAIN.1), 0.05, 0.05) == Status::AcceptH1 {
                accepted.0 = true;
            }
            if status(llr(&down, GAIN.0, GAIN.1), 0.05, 0.05) == Status::AcceptH0 {
                accepted.1 = true;
            }
            if accepted.0 && accepted.1 {
                return;
            }
        }
        panic!("neither bound crossed in 10 000 pairs: {accepted:?}");
    }

    /// The mirror match's live case: every pair identical means zero
    /// variance, and the test must idle rather than divide by it.
    #[test]
    fn an_all_draw_stream_has_no_variance_and_reports_zero_llr() {
        let counts = PairCounts {
            counts: [0, 0, 500, 0, 0],
        };
        assert_eq!(llr(&counts, GAIN.0, GAIN.1), 0.0);
        assert_eq!(
            status(llr(&counts, GAIN.0, GAIN.1), 0.05, 0.05),
            Status::Continue
        );
    }

    #[test]
    fn an_empty_sample_reports_zero_llr_and_continues() {
        let counts = PairCounts::default();
        assert_eq!(llr(&counts, GAIN.0, GAIN.1), 0.0);
        assert!((counts.score() - 0.5).abs() < 1e-12);
        assert_eq!(status(0.0, 0.05, 0.05), Status::Continue);
    }

    #[test]
    fn the_bounds_are_symmetric_log_odds_at_equal_alpha_and_beta() {
        let (lower, upper) = bounds(0.05, 0.05);
        assert!((upper - 19f64.ln()).abs() < 1e-12);
        assert!((lower + upper).abs() < 1e-12);
    }

    /// Swapping the candidate and the baseline reverses the cells and negates
    /// both hypotheses; the evidence must exactly change sign.
    #[test]
    fn mirrored_counts_negate_the_llr_under_mirrored_hypotheses() {
        let counts = PairCounts {
            counts: [3, 17, 96, 30, 8],
        };
        let mut reversed = counts;
        reversed.counts.reverse();
        let forward = llr(&counts, GAIN.0, GAIN.1);
        let backward = llr(&reversed, -GAIN.1, -GAIN.0);
        assert!((forward + backward).abs() < 1e-9, "{forward} vs {backward}");
    }

    /// Draw one pair outcome from a fixed pentanomial distribution.
    fn sample(probabilities: &[f64; 5], state: &mut u64) -> usize {
        let u = (rng::splitmix64(state) >> 11) as f64 / (1u64 << 53) as f64;
        let mut acc = 0.0;
        for (k, p) in probabilities.iter().enumerate() {
            acc += p;
            if u < acc {
                return k;
            }
        }
        4
    }

    /// The end-to-end synthetic-stream check: a candidate genuinely above
    /// elo1 is accepted, one genuinely below elo0 is rejected, both from one
    /// seed and both in bounded time.
    #[test]
    fn a_seeded_stream_above_elo1_accepts_h1_and_one_below_elo0_accepts_h0() {
        // A symmetric base shifted by ±ε across the quarter cells moves the
        // mean score by ε/2; ε = 4·(phi(10) − ½) puts the true strength at
        // ±10 Elo, comfortably outside the (0, 5) gain hypotheses.
        let shift = 4.0 * (phi(10.0) - 0.5);
        let stronger = [0.10, 0.20 - shift / 2.0, 0.40, 0.20 + shift / 2.0, 0.10];
        let weaker = [0.10, 0.20 + shift / 2.0, 0.40, 0.20 - shift / 2.0, 0.10];

        for (probabilities, expected) in [(stronger, Status::AcceptH1), (weaker, Status::AcceptH0)]
        {
            let mut state = 0x5249_4E53_4149_2D31u64;
            let mut counts = PairCounts::default();
            let mut decided = None;
            for _ in 0..100_000 {
                counts.record(sample(&probabilities, &mut state));
                let s = status(llr(&counts, GAIN.0, GAIN.1), 0.05, 0.05);
                if s != Status::Continue {
                    decided = Some(s);
                    break;
                }
            }
            assert_eq!(decided, Some(expected), "after {} pairs", counts.pairs());
        }
    }
}
