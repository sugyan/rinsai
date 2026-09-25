//! How much deeper than its siblings to search a move that gives check.
//!
//! ⚠️ **A node in check extends nothing, and that is the bound rather than a
//! tuning choice.** The child of an extended move is in check by definition,
//! so every extended ply is followed by one that is not, and the depth still
//! falls at least once every two plies. Drop the exemption and a line of
//! evasions that check back never spends depth at all: it runs until
//! 千日手 or the ply limit stops it.

use crate::score::Depth;

/// How many plies to add to the search of a move at a node, or zero to search
/// it at the ordinary depth.
///
/// `gives_check` is whether the move leaves the opponent in check;
/// `in_check` is whether the node playing it is in check itself.
///
/// The two flags are taken rather than a board so that a test can ask about a
/// pair without a position that holds one.
pub(crate) fn extension(gives_check: bool, in_check: bool) -> Depth {
    Depth::from(gives_check && !in_check)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_check_is_searched_a_ply_deeper() {
        assert_eq!(extension(true, false), 1);
    }

    #[test]
    fn a_move_that_does_not_check_is_not_extended() {
        assert_eq!(extension(false, false), 0);
        assert_eq!(extension(false, true), 0);
    }

    #[test]
    fn an_evasion_is_not_extended_even_when_it_checks_back() {
        assert_eq!(extension(true, true), 0);
    }
}
