//! Which quiet moves a shallow node may skip without searching them.
//!
//! A node close to the horizon whose static evaluation, plus what a quiet
//! move is taken to add to it, still does not reach alpha is assumed to have
//! nothing to gain from those moves: each would come back at or below alpha,
//! and searching it would only prove so. Skipping them is what buys the tree
//! back.
//!
//! ⚠️ **The margin is a claim about the evaluation**, not about the search. The
//! depth-1 margin is zero because a material balance cannot be raised by a
//! quiet move; `a_quiet_move_never_raises_the_material_balance` is what says
//! so, and it is the test that goes red when the evaluation stops being
//! material only.

use shogi_core::PieceKind;

use crate::eval;
use crate::score::{Depth, Score};

/// The deepest node that prunes anything.
const MAX_FUTILE_DEPTH: Depth = 2;

/// What a quiet move at a node of `depth` is assumed able to add to the
/// static evaluation by the time its search reaches the horizon.
///
/// * **Depth 1: nothing, and that is exact rather than assumed**, 千日手
///   aside. The child is a quiescence node that is not in check, so it stands
///   pat; and a quiet move that neither checks nor promotes leaves a material
///   balance where it was, or — a drop — lowers it, since a piece in hand is
///   worth more than the same piece on the board.
/// * **Depth 2: a silver won**, board and hand together. The move has one
///   reply to get through before quiescence, and a threat it makes good on is
///   worth what a capture is worth, which in shogi includes the hand. **A
///   starting point and not a measurement.**
fn margin(depth: Depth) -> i32 {
    match depth {
        1 => 0,
        _ => eval::capture_gain(PieceKind::Silver),
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
    if in_check || depth > MAX_FUTILE_DEPTH {
        return None;
    }
    Some(evaluate() + margin(depth))
}

/// Whether a move may be skipped at a node whose [`bound`] is `bound` and
/// whose alpha is `alpha`.
///
/// `quiet` must be false for a capture **and for a promotion**, as for
/// [`crate::reduction`]: the margin is what a quiet move can win, and a
/// promotion wins material by itself. A checking move is never skipped — its
/// child cannot stand pat, so nothing bounds it.
///
/// Only while alpha is an evaluation: once it holds a mate or a 連続王手 win,
/// a bound from the evaluation band is below it trivially, and skipping on
/// that would discard the moves that could find a shorter mate.
pub(crate) fn prunes(bound: Score, alpha: Score, quiet: bool, gives_check: bool) -> bool {
    quiet && !gives_check && bound <= alpha && alpha.get().abs() < Score::REPETITION.get()
}

#[cfg(test)]
mod tests {
    use shogi_core::{Move, PartialPosition};
    use shogi_usi_parser::FromUsi;
    use shunsai::Position;

    use super::*;

    /// A bound at alpha, from a quiet move that does not check — the only
    /// shape that is ever skipped.
    fn quiet_at(alpha: Score) -> bool {
        prunes(alpha, alpha, true, false)
    }

    #[test]
    fn a_shallow_node_prunes() {
        for depth in 1..=MAX_FUTILE_DEPTH {
            assert!(
                bound(depth, false, || Score::ZERO).is_some(),
                "depth {depth}"
            );
        }
    }

    #[test]
    fn a_deeper_node_prunes_nothing() {
        assert_eq!(bound(MAX_FUTILE_DEPTH + 1, false, || Score::ZERO), None);
    }

    #[test]
    fn a_node_in_check_prunes_nothing() {
        for depth in 1..=MAX_FUTILE_DEPTH {
            assert_eq!(bound(depth, true, || Score::ZERO), None, "depth {depth}");
        }
    }

    /// Sabotage: call `evaluate` before the guard, and the first row panics.
    #[test]
    fn a_node_that_prunes_nothing_does_not_evaluate() {
        let unreachable = || -> Score { panic!("evaluated at a node that prunes nothing") };
        assert_eq!(bound(MAX_FUTILE_DEPTH + 1, false, unreachable), None);
        assert_eq!(bound(1, true, unreachable), None);
    }

    #[test]
    fn the_bound_is_the_evaluation_plus_the_margin() {
        for depth in 1..=MAX_FUTILE_DEPTH {
            assert_eq!(
                bound(depth, false, || Score::cp(-300)),
                Some(Score::cp(-300) + margin(depth)),
                "depth {depth}"
            );
        }
    }

    /// A node with more depth left gives a quiet move more room to make
    /// something of itself, never less.
    #[test]
    fn the_margin_does_not_shrink_with_depth() {
        for depth in 2..=MAX_FUTILE_DEPTH {
            assert!(margin(depth) >= margin(depth - 1), "depth {depth}");
        }
    }

    #[test]
    fn a_bound_at_alpha_prunes_and_one_above_does_not() {
        let alpha = Score::cp(-200);
        assert!(prunes(alpha, alpha, true, false));
        assert!(!prunes(alpha + 1, alpha, true, false));
    }

    #[test]
    fn a_capture_or_a_promotion_is_not_pruned() {
        assert!(!prunes(Score::cp(-200), Score::ZERO, false, false));
    }

    #[test]
    fn a_checking_move_is_not_pruned() {
        assert!(!prunes(Score::cp(-200), Score::ZERO, true, true));
    }

    #[test]
    fn nothing_is_pruned_against_a_mate_or_a_repetition_win() {
        for alpha in [Score::mate_in(5), Score::REPETITION] {
            assert!(!quiet_at(alpha), "{alpha:?}");
        }
        // The top of the evaluation band still prunes, so the guard is the
        // band and not something narrower.
        assert!(quiet_at(Score::REPETITION - 1));
    }

    /// The claim the depth-1 margin of zero stands on, over every quiet move
    /// that neither checks nor promotes, from positions with drops on offer
    /// and without.
    ///
    /// Sabotage: price a pawn in hand at 85 instead of 115, and this fails on a
    /// pawn drop from the drop-heavy row.
    #[test]
    fn a_quiet_move_never_raises_the_material_balance() {
        let fixtures = [
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            "sfen lnsgkgsnl/1r5b1/pppppp1pp/6p2/9/2P6/PP1PPPPPP/1B5R1/LNSGKGSNL b - 3",
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            "sfen R8/2K1S1SSk/4B4/9/9/9/9/9/1L1L1L3 b RBGSNLP3g3n17p 1",
        ];
        let mut drops = 0;
        for sfen in fixtures {
            let mut board =
                Position::new(PartialPosition::from_usi(sfen).expect("a fixture parses"));
            let before = eval::evaluate(&board);
            for mv in board.legal_moves() {
                let quiet = match mv {
                    Move::Normal { to, promote, .. } => !promote && board.piece_at(to).is_none(),
                    Move::Drop { .. } => true,
                };
                if !quiet {
                    continue;
                }
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
        }
        assert!(
            drops > 0,
            "no fixture offered a drop, so half the claim went unasked"
        );
    }
}
