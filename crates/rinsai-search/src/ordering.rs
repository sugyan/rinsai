//! Move ordering: which of a node's moves is tried before the others.

use shogi_core::{Move, PieceKind};
use shunsai::Position;

use crate::eval;

/// How promising `mv` is as a capture in `position`, larger first, or `None`
/// when it takes nothing.
///
/// The pair is a strict priority: the material the move wins decides, and the
/// attacker's own value breaks a tie between equal wins — take with the
/// cheapest piece that can.
pub(crate) fn capture_key(position: &Position, mv: Move) -> Option<(i32, i32)> {
    // First, so that a drop — which lands on an empty square and can therefore
    // never capture — leaves here rather than through the `from` lookup below.
    let (victim, _) = position.piece_at(mv.to())?.to_parts();
    let (attacker, _) = position.piece_at(mv.from()?)?.to_parts();

    let promotion = if mv.is_promoting() {
        promotion_gain(attacker)
    } else {
        0
    };
    Some((
        eval::capture_gain(victim) + promotion,
        -eval::board_value(attacker),
    ))
}

/// What promoting a `kind` adds to the mover's material.
///
/// Zero for a kind that cannot promote. A move claiming to promote one is not
/// legal, and every move reaching here came out of shunsai's generator.
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

    /// The key of a capture assembled by hand, so a test can ask about a
    /// (victim, attacker, promote) triple without needing a position that
    /// holds it.
    ///
    /// ⚠️ It repeats [`capture_key`]'s arithmetic deliberately: what the tests
    /// below check is the *ordering* the pair produces, and what ties this
    /// shape to `capture_key`'s own is
    /// [`the_board_lookups_find_the_victim_and_the_attacker`].
    fn key(victim: PieceKind, attacker: PieceKind, promote: bool) -> (i32, i32) {
        let promotion = if promote { promotion_gain(attacker) } else { 0 };
        (
            eval::capture_gain(victim) + promotion,
            -eval::board_value(attacker),
        )
    }

    /// The property the pair exists for: **the tie-break can never outrank the
    /// material**. Exhaustive over every triple, because that is what makes it
    /// a claim about the table rather than about the triples somebody thought
    /// of.
    #[test]
    fn the_attacker_never_outranks_the_material() {
        for victim in PieceKind::all() {
            for attacker in PieceKind::all() {
                for promote in [false, true] {
                    let left = key(victim, attacker, promote);
                    for other_victim in PieceKind::all() {
                        for other_attacker in PieceKind::all() {
                            for other_promote in [false, true] {
                                let right = key(other_victim, other_attacker, other_promote);
                                if left.0 > right.0 {
                                    assert!(
                                        left > right,
                                        "{victim:?}x{attacker:?} promote={promote} wins more \
                                         than {other_victim:?}x{other_attacker:?} \
                                         promote={other_promote} and ranks below it"
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    /// The dearer victim first, whatever takes it.
    #[test]
    fn the_dearer_victim_ranks_first() {
        assert!(
            key(PieceKind::Rook, PieceKind::Pawn, false)
                > key(PieceKind::Pawn, PieceKind::Rook, false)
        );
        assert!(
            key(PieceKind::Gold, PieceKind::Gold, false)
                > key(PieceKind::Silver, PieceKind::Pawn, false)
        );
    }

    /// Equal victims, cheapest attacker first — the tie-break, and the only
    /// thing the second element of the pair ever decides.
    #[test]
    fn an_equal_victim_is_taken_with_the_cheaper_piece() {
        assert!(
            key(PieceKind::Gold, PieceKind::Pawn, false)
                > key(PieceKind::Gold, PieceKind::Rook, false)
        );
    }

    /// Promoting is a material event of its own, so the promoting form of a
    /// capture ranks above the same capture declining it.
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
                key(PieceKind::Pawn, attacker, true) > key(PieceKind::Pawn, attacker, false),
                "{attacker:?}"
            );
        }
    }

    /// A promoted victim is won at its board value and its **unpromoted** hand
    /// value, which is what makes a と金 dearer to take than the gold it moves
    /// like is to take.
    ///
    /// Sabotage, both directions: drop `capture_gain`'s hand term, or take the
    /// hand value without unpromoting first.
    #[test]
    fn a_promoted_victim_is_won_at_its_board_value_and_an_unpromoted_hand() {
        assert!(
            key(PieceKind::ProRook, PieceKind::Pawn, false)
                > key(PieceKind::Rook, PieceKind::Pawn, false)
        );
        assert!(
            key(PieceKind::ProPawn, PieceKind::Pawn, false)
                < key(PieceKind::Gold, PieceKind::Pawn, false)
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

    /// The key read off a real board agrees with the one assembled by hand —
    /// what pins [`capture_key`]'s two `piece_at` lookups to the right
    /// squares.
    ///
    /// Sabotage: read the attacker from `mv.to()` rather than `mv.from()`, or
    /// drop the promotion term. Neither reaches the tests above, which build
    /// their keys the same way this one does.
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
            Some(key(PieceKind::Silver, PieceKind::Rook, false))
        );

        let take_with_pawn = Move::Normal {
            from: Square::new(3, 4).expect("3d"),
            to: Square::new(3, 3).expect("3c"),
            promote: true,
        };
        assert_eq!(
            capture_key(&board, take_with_pawn),
            Some(key(PieceKind::Pawn, PieceKind::Pawn, true))
        );
    }
}
