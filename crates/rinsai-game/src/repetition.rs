//! Incremental fourfold-repetition tracking.
//!
//! [`shogi_legality_lite::status`] can detect 千日手, but it replays the entire
//! game and compares every SFEN against every earlier one — O(n²) per call, and
//! it does not classify 連続王手の千日手 at all. Keeping the index here makes it
//! O(1) per ply and gives [`crate::Game`] the occurrence window that the
//! perpetual-check rule needs.

use std::collections::HashMap;

use shogi_core::PartialPosition;

/// A position identity for the fourfold-repetition rule.
///
/// 千日手 is defined over board, side to move and both hands — and explicitly
/// *not* the move number, so that is normalised away before the SFEN is taken.
/// The representation is the normalised SFEN itself: exact, never a hash that
/// could collide.
///
/// Callers outside this crate: `crates/xtask`'s opening extractor, which
/// deduplicates candidate openings by this key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PositionKey(String);

/// The [`PositionKey`] of a position.
#[must_use]
pub fn position_key(position: &PartialPosition) -> PositionKey {
    let mut normalised = position.clone();
    let ok = normalised.ply_set(1);
    debug_assert!(ok, "1 is always a valid move number");
    PositionKey(normalised.to_sfen_owned())
}

#[derive(Debug, Clone)]
pub(crate) struct RepetitionIndex {
    counts: HashMap<PositionKey, u32>,
    /// Parallel to `Game::positions`, so an index here is an index there.
    keys: Vec<PositionKey>,
}

impl RepetitionIndex {
    /// The initial position counts as its own first occurrence, which is what
    /// makes `count >= 4` exactly the fourfold rule.
    pub(crate) fn new(initial: &PartialPosition) -> Self {
        let key = position_key(initial);
        let mut counts = HashMap::new();
        counts.insert(key.clone(), 1);
        Self {
            counts,
            keys: vec![key],
        }
    }

    /// Record the position reached after a move; returns its occurrence count.
    pub(crate) fn push(&mut self, position: &PartialPosition) -> u32 {
        let key = position_key(position);
        let count = self.counts.entry(key.clone()).or_insert(0);
        *count += 1;
        let count = *count;
        self.keys.push(key);
        count
    }

    pub(crate) fn pop(&mut self) {
        let Some(key) = self.keys.pop() else { return };
        if let Some(count) = self.counts.get_mut(&key) {
            *count -= 1;
            if *count == 0 {
                self.counts.remove(&key);
            }
        }
    }

    /// Index of the earliest occurrence of the position currently on top.
    ///
    /// [`crate::Game`] walks the moves from here to classify whether the cycle
    /// was a plain repetition or a perpetual check.
    pub(crate) fn first_occurrence(&self) -> Option<usize> {
        let current = self.keys.last()?;
        self.keys.iter().position(|key| key == current)
    }
}

#[cfg(test)]
mod tests {
    use shogi_core::{Color, Move, PieceKind, Square};

    use super::*;

    fn sq(file: u8, rank: u8) -> Square {
        Square::new(file, rank).expect("valid square")
    }

    #[test]
    fn the_key_ignores_the_move_number() {
        let mut a = PartialPosition::startpos();
        let mut b = PartialPosition::startpos();
        assert!(a.ply_set(1));
        assert!(b.ply_set(57));
        assert_eq!(position_key(&a), position_key(&b));
    }

    #[test]
    fn the_key_separates_positions_that_differ_only_in_side_to_move() {
        let a = PartialPosition::startpos();
        let mut b = PartialPosition::startpos();
        b.side_to_move_set(Color::White);
        assert_ne!(position_key(&a), position_key(&b));
    }

    #[test]
    fn the_key_separates_positions_that_differ_only_in_hand_contents() {
        let a = PartialPosition::startpos();
        let mut b = PartialPosition::startpos();
        let hand = b.hand_of_a_player_mut(Color::Black);
        *hand = hand.added(PieceKind::Pawn).expect("a pawn fits in hand");
        assert_ne!(position_key(&a), position_key(&b));
    }

    /// Shuffling both rooks out and back returns to the start position, so the
    /// fourth arrival is the fourth occurrence.
    #[test]
    fn a_four_fold_cycle_reaches_a_count_of_four() {
        let cycle = [
            (sq(2, 8), sq(3, 8)),
            (sq(8, 2), sq(7, 2)),
            (sq(3, 8), sq(2, 8)),
            (sq(7, 2), sq(8, 2)),
        ];
        let mut position = PartialPosition::startpos();
        let mut index = RepetitionIndex::new(&position);
        let mut counts = Vec::new();
        for _ in 0..3 {
            for (from, to) in cycle {
                position
                    .make_move(Move::Normal {
                        from,
                        to,
                        promote: false,
                    })
                    .expect("rook shuffle is legal");
                counts.push(index.push(&position));
            }
        }
        // Every fourth push returns to the start position.
        assert_eq!(counts[3], 2);
        assert_eq!(counts[7], 3);
        assert_eq!(counts[11], 4);
    }

    #[test]
    fn popping_restores_the_previous_counts_exactly() {
        let mut position = PartialPosition::startpos();
        let mut index = RepetitionIndex::new(&position);
        let before = index.clone();

        position
            .make_move(Move::Normal {
                from: sq(7, 7),
                to: sq(7, 6),
                promote: false,
            })
            .expect("7g7f is legal");
        index.push(&position);
        index.pop();

        assert_eq!(index.keys, before.keys);
        assert_eq!(index.counts, before.counts);
    }

    #[test]
    fn first_occurrence_points_at_the_start_of_the_cycle() {
        let cycle = [
            (sq(2, 8), sq(3, 8)),
            (sq(8, 2), sq(7, 2)),
            (sq(3, 8), sq(2, 8)),
            (sq(7, 2), sq(8, 2)),
        ];
        let mut position = PartialPosition::startpos();
        let mut index = RepetitionIndex::new(&position);
        for (from, to) in cycle {
            position
                .make_move(Move::Normal {
                    from,
                    to,
                    promote: false,
                })
                .expect("rook shuffle is legal");
            index.push(&position);
        }
        // Back at the start position, whose first occurrence is index 0.
        assert_eq!(index.first_occurrence(), Some(0));
    }
}
