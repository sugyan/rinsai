//! Which quiet moves a shallow node may skip without searching them.
//!
//! A node close to the horizon whose static evaluation, plus what a quiet
//! move is taken to add to it, still does not reach alpha is assumed to have
//! nothing to gain from those moves: each would come back at or below alpha,
//! and searching it would only prove so. Skipping them is what buys the tree
//! back.
//!
//! ⚠️ **The depth-1 margin of zero rests on three things, and only the first
//! is tested here.** A quiet move cannot raise the evaluation —
//! `a_quiet_move_never_raises_the_evaluation` goes red the day one can. The
//! quiescence child stands pat, so it never comes back above the evaluation
//! the move left — a quiescence that cut on a stored bound below its
//! stand-pat would break that. And 千日手 is asked about at the skip site,
//! because a skipped move never reaches the place that asks.

use shogi_core::PieceKind;

use crate::eval;
use crate::score::{Depth, Score};

/// What a quiet move at a node of `depth` is taken to add to the static
/// evaluation by the time its search reaches the horizon, or `None` at a
/// depth that prunes nothing. **The one place that says which depths
/// prune.**
///
/// * **Depth 1: nothing, and that is exact rather than assumed**, on the
///   module's three conditions. A quiet move that neither checks nor promotes
///   leaves a material balance where it was, or — a drop — lowers it, since a
///   piece in hand is worth more than the same piece on the board.
/// * **Depth 2: a silver won**, board and hand together. The move has one
///   reply to get through before quiescence, and a threat it makes good on is
///   worth what a capture is worth, which in shogi includes the hand. **A
///   starting point and not a measurement.**
fn margin(depth: Depth) -> Option<i32> {
    match depth {
        1 => Some(0),
        2 => Some(eval::capture_gain(PieceKind::Silver)),
        _ => None,
    }
}

/// What a quiet move searched at this node is taken to score at most, or
/// `None` at a node that prunes nothing.
///
/// `evaluate` is called only at a node that can prune, so a node that cannot
/// does not pay for a static evaluation it would never read.
///
/// A node in check prunes nothing: every move it has is an evasion, which is
/// not a move it chose to make.
pub(crate) fn bound(
    depth: Depth,
    in_check: bool,
    evaluate: impl FnOnce() -> Score,
) -> Option<Score> {
    if in_check {
        return None;
    }
    margin(depth).map(|margin| evaluate() + margin)
}

/// Whether a quiet move may be skipped at a node whose [`bound`] is `bound`
/// and whose alpha is `alpha`.
///
/// What is quiet is the caller's to decide: a capture or a promotion is given
/// no bound. A checking move is never skipped — its reply is forced, so the
/// margin says nothing about it.
///
/// **Only while alpha is an evaluation.** Once alpha holds a mate or a 連続王手
/// win, a bound from the evaluation band is below it trivially, and at depth 2
/// a skipped move could have found a shorter mate through its reply. ⚠️ At
/// depth 1 it could not — its child stands pat and never claims mate — so
/// there the guard costs nodes and protects nothing.
pub(crate) fn prunes(bound: Score, alpha: Score, gives_check: bool) -> bool {
    !gives_check && bound <= alpha && alpha.get().abs() < Score::REPETITION.get()
}

#[cfg(test)]
mod tests {
    use shogi_core::{Move, PartialPosition};
    use shogi_usi_parser::FromUsi;
    use shunsai::Position;

    use super::*;
    use crate::moves::MoveBuf;
    use crate::negamax::MAX_DEPTH;

    #[test]
    fn depths_one_and_two_prune_and_no_others_do() {
        for depth in 0..=MAX_DEPTH {
            let pruning = depth == 1 || depth == 2;
            assert_eq!(
                bound(depth, false, || Score::ZERO).is_some(),
                pruning,
                "depth {depth}"
            );
        }
    }

    #[test]
    fn a_node_in_check_prunes_nothing() {
        for depth in 1..=2 {
            assert_eq!(bound(depth, true, || Score::ZERO), None, "depth {depth}");
        }
    }

    /// Sabotage: call `evaluate` before the guard, and the first row panics.
    #[test]
    fn a_node_that_prunes_nothing_does_not_evaluate() {
        let unreachable = || -> Score { panic!("evaluated at a node that prunes nothing") };
        assert_eq!(bound(3, false, unreachable), None);
        assert_eq!(bound(1, true, unreachable), None);
    }

    #[test]
    fn the_bound_is_the_evaluation_plus_the_margin() {
        assert_eq!(bound(1, false, || Score::cp(-300)), Some(Score::cp(-300)));
        assert_eq!(
            bound(2, false, || Score::cp(-300)),
            Some(Score::cp(-300) + eval::capture_gain(PieceKind::Silver))
        );
    }

    /// A node with more depth left gives a quiet move more room to make
    /// something of itself, never less.
    #[test]
    fn the_margin_does_not_shrink_with_depth() {
        assert!(margin(2) >= margin(1));
    }

    #[test]
    fn a_bound_at_alpha_prunes_and_one_above_does_not() {
        let alpha = Score::cp(-200);
        assert!(prunes(alpha, alpha, false));
        assert!(!prunes(alpha + 1, alpha, false));
    }

    #[test]
    fn a_checking_move_is_not_pruned() {
        assert!(!prunes(Score::cp(-200), Score::ZERO, true));
    }

    /// Every row passes a bound from the evaluation band, the only kind a
    /// search produces, so a guard that read the bound instead of alpha would
    /// fail them.
    #[test]
    fn nothing_is_pruned_against_a_mate_or_a_repetition_win() {
        for alpha in [Score::mate_in(5), Score::REPETITION] {
            assert!(!prunes(Score::ZERO, alpha, false), "{alpha:?}");
        }
        // The top of the evaluation band still prunes, so the guard is the
        // band and not something narrower.
        assert!(prunes(Score::ZERO, Score::REPETITION - 1, false));
    }

    /// The first of the module's three conditions, over every move the search
    /// calls quiet — by [`MoveBuf::is_quiet`] itself — that does not check,
    /// from positions with drops on offer and without.
    ///
    /// Sabotage: price a pawn in hand at 85 instead of 115, and this fails on a
    /// pawn drop from the drop-heavy row. Drop `!is_promoting()` from
    /// `MoveBuf::is_quiet` and it fails on a promotion from the same row; drop
    /// `index >= quiets_from` and it fails on a capture from the second.
    #[test]
    fn a_quiet_move_never_raises_the_evaluation() {
        let fixtures = [
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "sfen lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3",
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            "sfen R8/2K1S1SSk/4B4/9/9/9/9/9/1L1L1L3 b RBGSNLP3g3n17p 1",
        ];
        let mut buf = MoveBuf::new();
        let mut drops = 0;
        for sfen in fixtures {
            let mut board =
                Position::new(PartialPosition::from_usi(sfen).expect("a fixture parses"));
            let before = eval::evaluate(&board);
            let base = buf.generate(&board);
            let quiets_from = buf.order_captures(base, &board);
            for i in base..buf.len() {
                if !buf.is_quiet(i, quiets_from) {
                    continue;
                }
                let mv = buf.get(i);
                let undo = board.do_move(mv);
                let checks = board.in_check();
                // From the mover's side, which is now the side not to move.
                let after = -eval::evaluate(&board);
                board.undo_move(mv, undo);
                if checks {
                    continue;
                }
                drops += usize::from(matches!(mv, Move::Drop { .. }));
                assert!(
                    after <= before,
                    "{sfen}: {mv:?} raised {before:?} to {after:?}"
                );
            }
            buf.truncate(base);
        }
        assert!(
            drops > 0,
            "no fixture offered a drop, so half the claim went unasked"
        );
    }
}
