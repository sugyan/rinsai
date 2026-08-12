//! The deterministic pair schedule: which opening each game plays, and who
//! sits where.
//!
//! Pure — the whole schedule is a function of `(opening_count, seed)`, which
//! is what lets the colour-swap structure be asserted without an engine
//! anywhere near the test, and what makes a match resumable in principle:
//! the same inputs name the same games.

use crate::rng;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GamePlan {
    /// Pairs are numbered from 0 in schedule order.
    pub pair: u64,
    /// Index into the opening list.
    pub opening: usize,
    /// The candidate takes Black in the pair's first game and White in its
    /// second; the baseline takes the other seat.
    pub candidate_is_black: bool,
}

/// Endless colour-swapped pairs: each lap is a fresh seeded permutation of
/// the openings (one generator state runs through all laps), so every
/// opening is played once per lap before any repeats. Empty openings yield
/// an empty schedule.
pub fn pairings(opening_count: usize, seed: u64) -> impl Iterator<Item = [GamePlan; 2]> {
    let mut state = seed;
    let mut lap: Vec<usize> = (0..opening_count).collect();
    let mut next = opening_count;
    let mut pair = 0u64;
    std::iter::from_fn(move || {
        if opening_count == 0 {
            return None;
        }
        if next == opening_count {
            rng::shuffle(&mut lap, &mut state);
            next = 0;
        }
        let opening = lap[next];
        next += 1;
        let plans = [
            GamePlan {
                pair,
                opening,
                candidate_is_black: true,
            },
            GamePlan {
                pair,
                opening,
                candidate_is_black: false,
            },
        ];
        pair += 1;
        Some(plans)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_games_of_a_pair_share_an_opening_and_swap_colours() {
        for (expected_pair, [a, b]) in pairings(37, 1).take(1_000).enumerate() {
            assert_eq!(a.pair, expected_pair as u64);
            assert_eq!(b.pair, a.pair);
            assert_eq!(a.opening, b.opening);
            assert!(a.candidate_is_black);
            assert!(!b.candidate_is_black);
        }
    }

    #[test]
    fn every_opening_is_played_once_per_lap_before_any_repeats() {
        let n = 41;
        let openings: Vec<usize> = pairings(n, 7).take(2 * n).map(|[a, _]| a.opening).collect();
        for lap in openings.chunks(n) {
            let mut sorted = lap.to_vec();
            sorted.sort_unstable();
            assert_eq!(sorted, (0..n).collect::<Vec<_>>(), "not a permutation");
        }
        assert_ne!(
            openings[..n],
            openings[n..],
            "two laps in the same order would say the state does not carry over"
        );
    }

    #[test]
    fn the_schedule_is_a_pure_function_of_seed_and_opening_count() {
        let a: Vec<_> = pairings(29, 42).take(100).collect();
        let b: Vec<_> = pairings(29, 42).take(100).collect();
        assert_eq!(a, b);
    }

    #[test]
    fn a_different_seed_orders_the_openings_differently() {
        let a: Vec<_> = pairings(32, 1).take(32).map(|[g, _]| g.opening).collect();
        let b: Vec<_> = pairings(32, 2).take(32).map(|[g, _]| g.opening).collect();
        assert_ne!(a, b);
    }

    #[test]
    fn no_openings_means_no_schedule() {
        assert_eq!(pairings(0, 1).next(), None);
    }
}
