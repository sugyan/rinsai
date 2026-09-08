//! The piece census: what a position holds of each kind, against what a shogi
//! set holds and against what this crate can represent.
//!
//! Counted here rather than read off `shogi_legality_lite::status_partial`,
//! which answers mate first and returns before it counts: a census that runs
//! only for undecided positions is not a census.

use std::fmt;

use shogi_core::{Color, Hand, PartialPosition, PieceKind, Square};

/// What a shogi set holds of each kind.
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
/// is outside this answer entirely: a 詰将棋 diagram routinely omits the
/// attacking king.
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

/// The most of one kind a game can hold, board and hands together.
///
/// ⚠️ Not a rule of shogi — a limit of what can be represented, and the reason
/// [`Game::from_position`](crate::Game::from_position) can refuse. Three things
/// break above it, none of them here: USI hand notation writes a count its own
/// reader takes at two digits, so a game holding more could not write itself
/// back out; `shogi_core`'s hand counts a kind in a `u8` that wraps rather than
/// refusing, so a capture past it destroys pieces silently; and the legality
/// crate's status sums board and both hands in a `u8` too.
const REPRESENTABLE: u32 = 99;

/// A root holding more of `kind` than a game can represent.
///
/// ⚠️ Far above what a shogi set holds: an over-inventory position is
/// [`over_inventory`]'s answer and is played like any other. This is the
/// separate line past which the representation itself stops being faithful.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnrepresentableRoot {
    pub kind: PieceKind,
    pub count: u32,
}

impl fmt::Display for UnrepresentableRoot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self { kind, count } = self;
        write!(
            f,
            "{count} {kind:?} on the board and in hand, past the {REPRESENTABLE} a game can hold"
        )
    }
}

impl std::error::Error for UnrepresentableRoot {}

/// Every kind's count, board and both hands together, a promoted piece counting
/// as the one it promoted from.
///
/// ⚠️ Indexed by [`PieceKind`], but only the seven hand kinds are ever read:
/// nothing bounds the kings, and a promoted kind's slot stays zero.
fn census(position: &PartialPosition) -> [u32; PieceKind::NUM] {
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
    seen
}

/// The first kind `position` holds more of than a shogi set does, if any.
#[must_use]
pub fn over_inventory(position: &PartialPosition) -> Option<ImpossiblePieceCount> {
    let seen = census(position);
    PIECE_TOTALS.into_iter().find_map(|(kind, total)| {
        let count = seen[kind.array_index()];
        (count > total).then_some(ImpossiblePieceCount { kind, count, total })
    })
}

/// The first kind `position` holds more of than a game can represent, if any.
pub(crate) fn unrepresentable(position: &PartialPosition) -> Option<UnrepresentableRoot> {
    let seen = census(position);
    PIECE_TOTALS.into_iter().find_map(|(kind, _)| {
        let count = seen[kind.array_index()];
        (count > REPRESENTABLE).then_some(UnrepresentableRoot { kind, count })
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
    /// Sabotage: drop the `unpromote()` in `census`, and this test and
    /// `rinsai-search`'s `the_two_root_censuses_agree_on_what_a_shogi_set_holds`
    /// go red, no other test in the workspace. Widening `count > total` to
    /// `count >= total` turns those two red and `rinsai-game`'s
    /// `a_root_outside_the_standard_inventory_still_plays` with them, because
    /// the all-golds fixture holds four lances and `PIECE_TOTALS` reaches Lance
    /// before Gold.
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
