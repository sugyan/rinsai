//! Move ordering: which of a node's moves is tried before the others.

use shogi_core::{Move, PieceKind};
use shunsai::Position;

use crate::eval;

/// How promising `mv` is as a capture in `position`, larger first, or `None`
/// when it takes nothing.
///
/// Two `piece_at` reads and [`key_of`]; the ranking itself is that function's,
/// so that a test can ask about a (victim, attacker, promote) triple without
/// needing a board that holds it.
pub(crate) fn capture_key(position: &Position, mv: Move) -> Option<(i32, i32)> {
    // First, so that a drop — which lands on an empty square and can therefore
    // never capture — leaves here rather than through the `from` lookup below.
    let (victim, _) = position.piece_at(mv.to())?.to_parts();
    let (attacker, _) = position.piece_at(mv.from()?)?.to_parts();
    Some(key_of(victim, attacker, mv.is_promoting()))
}

/// The rank of one capture, larger first.
///
/// The pair is a strict priority, and the type is what enforces it: `(i32,
/// i32)` compares its first element first, so no attacker value can reverse a
/// difference in material.
///
/// * **What the move wins** — the victim's [`eval::capture_gain`] plus what
///   promoting adds.
/// * **What wins it**, only where the first ties: the cheapest attacker.
///   ⚠️ **The king is the cheapest of all, and that is deliberate.** Every move
///   here came out of shunsai's fully legal generation, so a king capture that
///   was generated leaves the king unattacked — the victim was undefended and
///   the material is won outright. `board_value` prices the king at zero for a
///   reason of its own, and this is the one reading of that zero that happens
///   to be right; `the_king_takes_first_because_a_legal_king_capture_is_free`
///   is what says so if either side of it changes.
pub(crate) fn key_of(victim: PieceKind, attacker: PieceKind, promote: bool) -> (i32, i32) {
    let promotion = if promote { promotion_gain(attacker) } else { 0 };
    (
        eval::capture_gain(victim) + promotion,
        -eval::board_value(attacker),
    )
}

/// What promoting a `kind` adds to the mover's material. Zero for a kind that
/// cannot promote.
fn promotion_gain(kind: PieceKind) -> i32 {
    kind.promote().map_or(0, |promoted| {
        eval::board_value(promoted) - eval::board_value(kind)
    })
}

#[cfg(test)]
mod tests {
    use shogi_core::{Color, PartialPosition, Piece, Square};
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn position(sfen: &str) -> Position {
        Position::new(PartialPosition::from_usi(sfen).expect("test SFEN parses"))
    }

    /// The dearer victim first, whatever takes it.
    ///
    /// Sabotage: swap the pair's two elements.
    #[test]
    fn the_dearer_victim_ranks_first() {
        assert!(
            key_of(PieceKind::Rook, PieceKind::Pawn, false)
                > key_of(PieceKind::Pawn, PieceKind::Rook, false)
        );
        assert!(
            key_of(PieceKind::Gold, PieceKind::Gold, false)
                > key_of(PieceKind::Silver, PieceKind::Pawn, false)
        );
    }

    /// Equal victims, cheapest attacker first — the only thing the second
    /// element of the pair ever decides.
    ///
    /// Sabotage: drop the negation on `board_value`.
    #[test]
    fn an_equal_victim_is_taken_with_the_cheaper_piece() {
        assert!(
            key_of(PieceKind::Gold, PieceKind::Pawn, false)
                > key_of(PieceKind::Gold, PieceKind::Rook, false)
        );
    }

    /// ⚠️ **The king outranks every other attacker**, because a king capture
    /// that shunsai generated is a capture of an undefended piece.
    ///
    /// Sabotage: price the king above the other attackers in `eval`'s table —
    /// the reading of `BOARD[King] == 0` this rests on — and it goes red.
    #[test]
    fn the_king_takes_first_because_a_legal_king_capture_is_free() {
        for attacker in PieceKind::all() {
            if attacker == PieceKind::King {
                continue;
            }
            assert!(
                key_of(PieceKind::Gold, PieceKind::King, false)
                    > key_of(PieceKind::Gold, attacker, false),
                "{attacker:?} ranks at or above the king"
            );
        }
    }

    /// Promoting is a material event of its own, so the promoting form of a
    /// capture ranks above the same capture declining it.
    ///
    /// Sabotage: drop the `promotion` term from `key_of`.
    #[test]
    fn a_capture_that_promotes_ranks_above_the_same_capture_declining() {
        for attacker in [
            PieceKind::Pawn,
            PieceKind::Lance,
            PieceKind::Knight,
            PieceKind::Silver,
            PieceKind::Bishop,
            PieceKind::Rook,
        ] {
            assert!(
                key_of(PieceKind::Pawn, attacker, true) > key_of(PieceKind::Pawn, attacker, false),
                "{attacker:?}"
            );
        }
    }

    /// A promoted victim is won at its board value and its **unpromoted** hand
    /// value: a と金 goes to the hand as a pawn, so taking one wins less than
    /// taking the gold it moves like.
    ///
    /// Sabotage, both directions: drop `capture_gain`'s hand term, or take the
    /// hand value without unpromoting first.
    #[test]
    fn a_promoted_victim_is_won_at_its_board_value_and_an_unpromoted_hand() {
        assert!(
            key_of(PieceKind::ProRook, PieceKind::Pawn, false)
                > key_of(PieceKind::Rook, PieceKind::Pawn, false)
        );
        assert!(
            key_of(PieceKind::ProPawn, PieceKind::Pawn, false)
                < key_of(PieceKind::Gold, PieceKind::Pawn, false)
        );
    }

    /// A move that takes nothing has no key at all — the partition
    /// [`MoveBuf::order_captures`](crate::moves::MoveBuf::order_captures) runs
    /// is this answer and no second question.
    #[test]
    fn a_move_that_takes_nothing_has_no_key() {
        let board = position("sfen 4k4/9/9/9/9/9/9/9/4K4 b P 1");
        let quiet = Move::Normal {
            from: Square::new(5, 9).expect("5i"),
            to: Square::new(5, 8).expect("5h"),
            promote: false,
        };
        assert_eq!(capture_key(&board, quiet), None);

        let drop = Move::Drop {
            piece: Piece::new(PieceKind::Pawn, Color::Black),
            to: Square::new(5, 5).expect("5e"),
        };
        assert_eq!(capture_key(&board, drop), None);
    }

    /// The key read off a real board is [`key_of`]'s, for the victim on the
    /// destination and the attacker on the origin.
    ///
    /// Sabotage: read the attacker from `mv.to()` rather than `mv.from()`, or
    /// pass a constant in place of `mv.is_promoting()`.
    #[test]
    fn the_board_lookups_find_the_victim_and_the_attacker() {
        // A black rook on 5f under a white silver on 5e, and a black pawn on
        // 3d under a white pawn on 3c where promotion is optional.
        let board = position("sfen 4k4/9/6p2/6P2/4s4/4R4/9/9/4K4 b - 1");

        let take_with_rook = Move::Normal {
            from: Square::new(5, 6).expect("5f"),
            to: Square::new(5, 5).expect("5e"),
            promote: false,
        };
        assert_eq!(
            capture_key(&board, take_with_rook),
            Some(key_of(PieceKind::Silver, PieceKind::Rook, false))
        );

        let take_with_pawn = Move::Normal {
            from: Square::new(3, 4).expect("3d"),
            to: Square::new(3, 3).expect("3c"),
            promote: true,
        };
        assert_eq!(
            capture_key(&board, take_with_pawn),
            Some(key_of(PieceKind::Pawn, PieceKind::Pawn, true))
        );
    }

    /// The king case on a real board, where it is reachable: in check, with a
    /// cheaper attacker of the same piece available.
    ///
    /// **The fixture is what makes this able to fail** — a gold on 5e giving
    /// check, takeable by the king on 5f and by the silver on 4f, and
    /// undefended, so both captures are generated.
    #[test]
    fn a_king_capture_and_a_cheaper_one_rank_the_king_first() {
        let board = position("sfen 8k/9/9/9/4g4/4KS3/9/9/9 b - 1");
        assert!(board.in_check(), "the fixture stopped being a check");
        let to = Square::new(5, 5).expect("5e");
        let by = |file, rank| {
            capture_key(
                &board,
                Move::Normal {
                    from: Square::new(file, rank).expect("a square"),
                    to,
                    promote: false,
                },
            )
            .expect("a capture")
        };
        assert!(by(5, 6) > by(4, 6), "the silver outranked the king");
    }
}
