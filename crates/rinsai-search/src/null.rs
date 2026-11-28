//! Null-move pruning: whether a node may pass instead of moving, and what a
//! pass that still reaches beta may claim.
//!
//! A side so far ahead that it stays ahead after doing nothing — the opponent
//! moving twice, and the search after it shortened — is taken to stay ahead
//! with whatever move it would have played, and the node is cut off before a
//! move is generated.
//!
//! ⚠️ **What breaks it is zugzwang, a position where every move is worse than
//! none.** A side with a piece in hand can usually drop it somewhere harmless,
//! which is why the case is rare in shogi; nothing here verifies a pass for
//! the side with an empty hand, where the argument is weakest.

use crate::score::{Depth, Score};

/// The shallowest node that may pass. **A starting point and not a
/// measurement.**
pub(crate) const MIN_DEPTH: Depth = 2;

/// How much shorter than an ordinary child the search after a pass is.
/// **A starting point and not a measurement**, and flat in depth: a schedule
/// that grows with it is a second effect, which one SPRT cannot tell apart
/// from the first.
pub(crate) const REDUCTION: Depth = 2;

/// Whether a node may pass.
///
/// Not in check, because passing would leave the king to be taken. Only on a
/// scout window, because a node on a wider one is a node whose exact score is
/// wanted, and a pass proves only a bound. Not when `beta` is a mate or a
/// repetition. And only when the static evaluation already reaches `beta`.
///
/// `evaluate` is called last, so a node that cannot pass for any other reason
/// does not pay for an evaluation it would not read.
pub(crate) fn may_pass(
    depth: Depth,
    scout: bool,
    in_check: bool,
    beta: Score,
    evaluate: impl FnOnce() -> Score,
) -> bool {
    !in_check
        && scout
        && depth >= MIN_DEPTH
        && beta.get().abs() < Score::REPETITION.get()
        && evaluate() >= beta
}

/// The depth the search after a pass runs at, from a node of `depth`.
pub(crate) fn depth(depth: Depth) -> Depth {
    depth - 1 - REDUCTION
}

/// What a node returns when the search after its pass came back with `score`,
/// at or above `beta`: the score, unless it is a mate or a repetition, which a
/// pass cannot prove, and then `beta`.
pub(crate) fn cut(score: Score, beta: Score) -> Score {
    if score.get().abs() < Score::REPETITION.get() {
        score
    } else {
        beta
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A node every other condition lets pass, at an evaluation at `beta`.
    fn passes(depth: Depth, scout: bool, in_check: bool, beta: Score) -> bool {
        may_pass(depth, scout, in_check, beta, || beta)
    }

    #[test]
    fn a_node_ahead_on_a_scout_passes() {
        assert!(passes(MIN_DEPTH, true, false, Score::ZERO));
    }

    #[test]
    fn a_node_in_check_does_not_pass() {
        assert!(!passes(MIN_DEPTH, true, true, Score::ZERO));
    }

    #[test]
    fn a_node_on_a_wider_window_does_not_pass() {
        assert!(!passes(MIN_DEPTH, false, false, Score::ZERO));
    }

    #[test]
    fn a_shallow_node_does_not_pass() {
        for depth in 1..MIN_DEPTH {
            assert!(!passes(depth, true, false, Score::ZERO), "depth {depth}");
        }
    }

    #[test]
    fn a_node_whose_beta_is_a_mate_or_a_repetition_does_not_pass() {
        for beta in [
            Score::mate_in(3),
            Score::mated_in(3),
            Score::REPETITION,
            -Score::REPETITION,
        ] {
            assert!(!passes(MIN_DEPTH, true, false, beta), "{beta:?}");
        }
        assert!(passes(MIN_DEPTH, true, false, Score::REPETITION - 1));
    }

    #[test]
    fn a_node_behind_does_not_pass() {
        assert!(!may_pass(MIN_DEPTH, true, false, Score::cp(1), || {
            Score::ZERO
        }));
    }

    #[test]
    fn a_node_that_cannot_pass_does_not_evaluate() {
        let refused = |depth, scout, in_check, beta| {
            may_pass(depth, scout, in_check, beta, || {
                panic!("evaluated a node that cannot pass")
            })
        };
        assert!(!refused(MIN_DEPTH, true, true, Score::ZERO));
        assert!(!refused(MIN_DEPTH, false, false, Score::ZERO));
        assert!(!refused(MIN_DEPTH - 1, true, false, Score::ZERO));
        assert!(!refused(MIN_DEPTH, true, false, Score::mate_in(1)));
    }

    #[test]
    fn a_cut_never_claims_a_mate_or_a_repetition() {
        let beta = Score::cp(40);
        assert_eq!(cut(Score::cp(250), beta), Score::cp(250));
        for score in [Score::mate_in(5), Score::REPETITION] {
            assert_eq!(cut(score, beta), beta, "{score:?}");
        }
    }
}
