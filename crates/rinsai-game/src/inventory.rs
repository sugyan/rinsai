//! The piece census: what a position holds of each kind, against what a shogi
//! set holds.
//!
//! Counted here rather than read off `shogi_legality_lite::status_partial`,
//! which answers mate first and returns before it counts: a census that runs
//! only for undecided positions is not a census.

use std::fmt;

use shogi_core::{Color, Hand, PartialPosition, PieceKind, Square};

/// What a shogi set holds of each kind, counting a promoted piece as the one it
/// promoted from.
///
/// ⚠️ The kings are not here, so nothing in this crate bounds their number: a
/// 詰将棋 diagram routinely omits the attacking king.
const PIECE_TOTALS: [(PieceKind, u32); 7] = [
    (PieceKind::Pawn, 18),
    (PieceKind::Lance, 4),
    (PieceKind::Knight, 4),
    (PieceKind::Silver, 4),
    (PieceKind::Gold, 4),
    (PieceKind::Bishop, 2),
    (PieceKind::Rook, 2),
];

/// A kind a position holds more of than a shogi set does: `count` of `kind` on
/// the board and in the hands together, counting a promoted piece as the one it
/// promoted from, where a set holds `total`.
///
/// A fact about the position rather than a verdict on it — what to do about one
/// is the caller's rule.
///
/// ⚠️ The kings are not counted, so a position missing one, or holding three,
/// is outside this answer entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImpossiblePieceCount {
    pub kind: PieceKind,
    pub count: u32,
    pub total: u32,
}

impl fmt::Display for ImpossiblePieceCount {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { kind, count, total } = self;
        write!(
            f,
            "{count} {kind:?} on the board and in hand, but a set holds {total}"
        )
    }
}

impl std::error::Error for ImpossiblePieceCount {}

/// The first kind `position` holds more of than a shogi set does, if any.
///
/// A caller decides what that means for it: this crate referees such a position
/// like any other, and [`Game`](crate::Game) will hold and play one.
///
/// No move creates a piece — a move relocates one, a promotion stays inside its
/// kind's union, and a capture moves one to a hand of the same base kind — so
/// the answer is a property of the root, and asking it of any later position in
/// a game gives the same one.
#[must_use]
pub fn over_inventory(position: &PartialPosition) -> Option<ImpossiblePieceCount> {
    let mut seen = [0u32; PieceKind::NUM];
    for square in Square::all() {
        if let Some(piece) = position.piece_at(square) {
            let kind = piece.piece_kind();
            seen[kind.unpromote().unwrap_or(kind).array_index()] += 1;
        }
    }
    for color in Color::all() {
        let hand = position.hand_of_a_player(color);
        for kind in Hand::all_hand_pieces() {
            seen[kind.array_index()] += u32::from(hand.count(kind).unwrap_or(0));
        }
    }
    PIECE_TOTALS.into_iter().find_map(|(kind, total)| {
        let count = seen[kind.array_index()];
        (count > total).then_some(ImpossiblePieceCount { kind, count, total })
    })
}

#[cfg(test)]
mod tests {
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn position(sfen: &str) -> PartialPosition {
        PartialPosition::from_usi(sfen).expect("valid sfen")
    }

    /// The census names the first kind a set cannot account for, and its count.
    ///
    /// ⚠️ Nineteen pawns above and eighteen below are the boundary, and it
    /// takes both to say the bound is the bound rather than a smell. The last
    /// row is the one a *status* function cannot answer: it is mate as well as
    /// over-inventory, and mate is reported before anything is counted.
    ///
    /// Sabotage: drop the `unpromote()` in `over_inventory`, or widen its
    /// `count > total` to `count >= total`. Each mutation turned this test and
    /// `rinsai-search`'s `the_two_root_censuses_agree_on_what_a_shogi_set_holds`
    /// red, and no other test in the workspace.
    #[test]
    fn a_position_no_shogi_set_could_hold_names_the_kind_and_the_count() {
        for (sfen, kind, count, total) in [
            (
                "sfen 4k4/9/9/9/9/9/9/9/4K4 b 19P 1",
                PieceKind::Pawn,
                19,
                18,
            ),
            ("sfen 4k4/9/9/9/9/9/9/9/4K4 b 5R 1", PieceKind::Rook, 5, 2),
            // Promoted pieces count as what they promoted from: a +P on the
            // board plus eighteen in hand is nineteen pawns.
            (
                "sfen 4k4/9/9/9/4+P4/9/9/9/4K4 b 18P 1",
                PieceKind::Pawn,
                19,
                18,
            ),
            // Per kind, not only about pawns.
            ("sfen 3kg4/9/9/9/9/9/9/9/3KG4 b 4G 1", PieceKind::Gold, 6, 4),
            // Over-inventory *and* mate.
            ("sfen 8l/9/9/9/9/9/9/8g/8K b 19P 1", PieceKind::Pawn, 19, 18),
        ] {
            assert_eq!(
                over_inventory(&position(sfen)),
                Some(ImpossiblePieceCount { kind, count, total }),
                "{sfen}"
            );
        }
        assert_eq!(
            over_inventory(&position("sfen 4k4/9/9/9/9/9/9/9/4K4 b 18P 1")),
            None,
            "eighteen pawns is a real position"
        );
        assert_eq!(
            over_inventory(&position("sfen 4k4/9/9/9/9/9/9/9/3KK4 b - 1")),
            None,
            "the census does not count kings"
        );
    }
}
