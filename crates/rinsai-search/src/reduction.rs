//! How much of a late quiet move's search to skip.
//!
//! The move list a node walks is ordered — the transposition move, then the
//! captures by what they win, then the quiet moves by killers and history —
//! so a move far down it is one every heuristic here ranked badly. Searching
//! those a ply shallower is what buys the tree back.
//!
//! ⚠️ **A reduction is a guess, and an unverified guess is a wrong answer
//! rather than a cheaper one.** The saving comes from moves that stay below
//! alpha; anything that comes back above it has to be searched again at full
//! depth before it may raise alpha or cut the node off. Nothing here can
//! enforce that — [`crate::NegamaxSearcher`]'s move loop is where it holds.

use crate::score::Depth;

/// The shallowest node that reduces anything.
///
/// Below it a reduction is not a shallower search but a truncation: the child
/// of a depth-2 node already runs at depth 1, so taking a ply off dispatches
/// it into quiescence and gives up the whole subtree rather than a ply of it.
/// `a_reduced_child_is_still_an_interior_node` is what says so.
const MIN_DEPTH: Depth = 3;

/// How many moves a node searches at full depth before it starts reducing.
///
/// The front of an ordered list is the transposition move and the best-ranked
/// captures, which is where the ordering is most likely right and a reduction
/// most likely wrong.
///
/// ⚠️ **The number is a starting point and not a measurement**, and no test
/// here pins it: every assertion below is written against the constant rather
/// than against its value, so the suite follows it wherever it is set.
const MIN_PLAYED: usize = 4;

/// How many plies to take off `played`'s search at a node of `depth`, or zero
/// to search it whole.
///
/// `played` is how many moves this node has already searched, not an index
/// into the move buffer — a node whose list starts part-way through the buffer
/// would otherwise reduce its own first move.
///
/// `quiet` must be false for a capture **and for a promotion**. A promotion
/// that takes nothing is quiet by the capture test — 歩→と lands on an empty
/// square — while [`ordering::promotion_gain`](crate::ordering) prices it
/// above every pawn capture, so the ranking that put it late is the one to
/// distrust.
///
/// The three flags are taken rather than a board so that a test can ask about
/// a (depth, played, kind-of-move) triple without a position that holds one.
pub(crate) fn reduction(
    depth: Depth,
    played: usize,
    quiet: bool,
    gives_check: bool,
    in_check: bool,
) -> Depth {
    // A capture is ranked by what it wins rather than by how it has done
    // elsewhere, a checking move forces the reply that would refute it, and
    // an evasion is not a move the node chose to make.
    if !quiet || gives_check || in_check {
        return 0;
    }
    if depth < MIN_DEPTH || played < MIN_PLAYED {
        return 0;
    }
    1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::moves::MAX_LEGAL_MOVES;
    use crate::negamax::MAX_DEPTH;

    /// A quiet move, not checking, at a node not in check — the only shape
    /// that is ever reduced.
    fn late_quiet(depth: Depth, played: usize) -> Depth {
        reduction(depth, played, true, false, false)
    }

    #[test]
    fn a_late_quiet_move_is_reduced() {
        assert_eq!(late_quiet(MIN_DEPTH, MIN_PLAYED), 1);
    }

    #[test]
    fn the_front_of_the_list_is_searched_whole() {
        for played in 0..MIN_PLAYED {
            assert_eq!(late_quiet(MIN_DEPTH, played), 0, "played {played}");
        }
    }

    #[test]
    fn a_shallow_node_reduces_nothing() {
        for depth in 1..MIN_DEPTH {
            assert_eq!(late_quiet(depth, MIN_PLAYED), 0, "depth {depth}");
        }
    }

    /// The property [`MIN_DEPTH`] was chosen for, over every depth and move
    /// number a search can reach: a reduced child still has a ply to spend, so
    /// it is a shallower search rather than a jump into quiescence.
    ///
    /// The bounds are the engine's own — a `go` is clamped to `MAX_DEPTH` and
    /// a ply holds at most `MAX_LEGAL_MOVES` moves — so the sweep follows
    /// them if either moves. ⚠️ **It is currently one case wearing 74 000**:
    /// the answer is flat in `played` and binding only at `depth == MIN_DEPTH`.
    /// It earns the sweep the day the reduction becomes a schedule.
    #[test]
    fn a_reduced_child_is_still_an_interior_node() {
        for depth in 1..=MAX_DEPTH {
            for played in 0..MAX_LEGAL_MOVES {
                let r = late_quiet(depth, played);
                if r > 0 {
                    assert!(depth - 1 - r > 0, "depth {depth}, played {played}");
                }
            }
        }
    }

    #[test]
    fn a_capture_is_not_reduced() {
        assert_eq!(reduction(MIN_DEPTH, MIN_PLAYED, false, false, false), 0);
    }

    #[test]
    fn a_checking_move_is_not_reduced() {
        assert_eq!(reduction(MIN_DEPTH, MIN_PLAYED, true, true, false), 0);
    }

    #[test]
    fn a_node_in_check_reduces_nothing() {
        assert_eq!(reduction(MIN_DEPTH, MIN_PLAYED, true, false, true), 0);
    }

    /// Deeper and later does not reduce further. One effect per patch: a
    /// schedule that grows with either is a second one, and a single SPRT
    /// cannot tell two apart.
    #[test]
    fn the_reduction_is_flat() {
        assert_eq!(late_quiet(32, 60), 1);
    }
}
