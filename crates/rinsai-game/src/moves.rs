//! Turning USI text into moves, and asking whether a king is attacked.
//!
//! Both of these exist because the obvious call does the wrong thing.

use shogi_core::{Color, Move, PartialPosition, Piece};
use shogi_usi_parser::FromUsi;

/// The one place a USI move string becomes a [`Move`].
///
/// USI drop notation carries no colour — `P*7f` is the same text whichever side
/// plays it — and `shogi_usi_parser` resolves the ambiguity by hard-coding
/// Black. Every parsed drop therefore has to be re-coloured from the position.
/// Routing all parsing through this function is what makes that unmissable;
/// calling `Move::from_usi` directly anywhere else is a bug.
pub fn move_from_usi(s: &str, side: Color) -> Result<Move, shogi_usi_parser::Error> {
    let parsed = Move::from_usi(s)?;
    Ok(match parsed {
        Move::Drop { piece, to } => Move::Drop {
            piece: Piece::new(piece.piece_kind(), side),
            to,
        },
        normal => normal,
    })
}

/// Whether `color`'s king is currently attacked.
///
/// `shogi_legality_lite` has no `is_in_check`. What it has is
/// [`will_king_be_captured`], which asks whether the *side to move* can capture
/// the opponent's king — so the question has to be posed from the attacker's
/// seat, hence the flip.
///
/// Note what this is not: calling `will_king_be_captured` on a position straight
/// after a move asks whether the mover left their own king en prise, which
/// `is_legal_partial` already forbids. Getting this backwards yields a
/// `gave_check` flag that is always false and a perpetual-check rule that never
/// fires.
///
/// [`will_king_be_captured`]: shogi_legality_lite::prelegality::will_king_be_captured
#[must_use]
pub fn in_check(position: &PartialPosition, color: Color) -> bool {
    let mut probe = position.clone();
    probe.side_to_move_set(color.flip());
    // `None` means the king is missing, which only happens in hand-built
    // positions; "not in check" is the safe reading.
    shogi_legality_lite::prelegality::will_king_be_captured(&probe) == Some(true)
}

#[cfg(test)]
mod tests {
    use shogi_core::{PieceKind, Square, ToUsi};

    use super::*;

    fn sq(file: u8, rank: u8) -> Square {
        Square::new(file, rank).expect("valid square")
    }

    /// The trap this module exists for.
    #[test]
    fn a_drop_is_recoloured_to_the_side_to_move() {
        let black = move_from_usi("P*5e", Color::Black).expect("valid drop");
        let white = move_from_usi("P*5e", Color::White).expect("valid drop");
        assert_eq!(
            black,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::Black),
                to: sq(5, 5),
            }
        );
        assert_eq!(
            white,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::White),
                to: sq(5, 5),
            },
            "shogi_usi_parser hard-codes Black; this is the fix"
        );
    }

    #[test]
    fn a_normal_move_passes_through_untouched() {
        for side in [Color::Black, Color::White] {
            assert_eq!(
                move_from_usi("7g7f", side).expect("valid move"),
                Move::Normal {
                    from: sq(7, 7),
                    to: sq(7, 6),
                    promote: false,
                }
            );
        }
        assert_eq!(
            move_from_usi("2b3a+", Color::Black).expect("valid move"),
            Move::Normal {
                from: sq(2, 2),
                to: sq(3, 1),
                promote: true,
            }
        );
    }

    #[test]
    fn parsing_round_trips_through_to_usi() {
        for text in ["7g7f", "2b3a+", "P*5e", "B*4d"] {
            let mv = move_from_usi(text, Color::Black).expect("valid move");
            assert_eq!(mv.to_usi_owned(), text);
        }
    }

    #[test]
    fn trailing_junk_is_rejected() {
        // `from_usi` is the strict variant, so a typo cannot be half-accepted.
        assert!(move_from_usi("7g7f7", Color::Black).is_err());
        assert!(move_from_usi("7g7z", Color::Black).is_err());
        assert!(move_from_usi("", Color::Black).is_err());
    }

    #[test]
    fn the_start_position_has_neither_king_in_check() {
        let position = PartialPosition::startpos();
        assert!(!in_check(&position, Color::Black));
        assert!(!in_check(&position, Color::White));
    }

    #[test]
    fn a_checking_move_is_detected_for_the_side_that_is_checked() {
        // A lone White king on 5a with a Black rook dropped onto the same file.
        let position =
            PartialPosition::from_usi("sfen 4k4/9/9/9/9/9/9/9/4K4 b R 1").expect("valid sfen");
        let mut checked = position.clone();
        checked
            .make_move(Move::Drop {
                piece: Piece::new(PieceKind::Rook, Color::Black),
                to: sq(5, 5),
            })
            .expect("the drop is playable");

        assert!(
            in_check(&checked, Color::White),
            "White is checked on the file"
        );
        assert!(!in_check(&checked, Color::Black));
        // And the naive reading — asking the post-move position directly — is
        // the opposite question and answers false.
        assert_ne!(
            shogi_legality_lite::prelegality::will_king_be_captured(&checked),
            Some(true)
        );
    }
}
