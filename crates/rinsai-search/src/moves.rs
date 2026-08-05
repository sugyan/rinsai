//! Move helpers that shunsai deliberately does not provide.

use core::ops::ControlFlow;

use shogi_core::Move;
use shunsai::{MoveSet, Position};

/// The most legal moves any shogi position has.
///
/// It is the size a move buffer has to be, and shunsai's own benches use the
/// same number (`benches/suite/common.rs`).
pub const MAX_LEGAL_MOVES: usize = 593;

/// Whether `mv` is legal in `position`, without allocating.
///
/// shunsai has no `is_legal`: generation is always fully legal, so nothing
/// inside it ever needs to ask. A search engine does. Two callers exist —
/// moves arriving over USI (and, from E2, CSA), and from E0 step 3 a
/// transposition-table move before it is played, since a 64-bit key collision
/// would otherwise feed [`Position::do_move`] a move from a different position
/// and trip its `expect`s.
///
/// The obviously-correct version is `position.legal_moves().contains(&mv)`;
/// that allocates, and is kept as the test oracle rather than used here.
#[must_use]
pub fn is_legal(position: &Position, mv: Move) -> bool {
    position
        .generate_moves(|set| match (set, mv) {
            (
                MoveSet::Normal {
                    from,
                    promotions,
                    non_promotions,
                    ..
                },
                Move::Normal {
                    from: want_from,
                    to,
                    promote,
                },
            ) if from == want_from => {
                // The two boards overlap where promotion is optional: a square
                // in `promotions` alone is a compulsory promotion and one in
                // `non_promotions` alone cannot promote, so selecting by the
                // flag is exactly the legality question.
                let board = if promote { promotions } else { non_promotions };
                if board.contains(to) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            (
                MoveSet::Drop { piece, to },
                Move::Drop {
                    piece: want_piece,
                    to: want_to,
                },
            ) if piece == want_piece => {
                // Both `Piece`s carry their colour, so this also rejects a drop
                // by the side that is not to move — which is the safety net
                // under USI drop notation carrying no colour of its own.
                if to.contains(want_to) {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            }
            // Continue rather than reporting failure, so the walk stays correct
            // even if shunsai ever emits two `MoveSet`s sharing a `from`.
            // Nothing in shunsai's public documentation says how moves are
            // grouped into sets — one per origin is what it does today, but it
            // does not promise that — so this makes no assumption either way.
            _ => ControlFlow::Continue(()),
        })
        .is_break()
}

#[cfg(test)]
mod tests {
    use shogi_core::{Color, PartialPosition, Piece, PieceKind, Square};
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn position(sfen: &str) -> Position {
        Position::new(PartialPosition::from_usi(sfen).expect("test SFEN parses"))
    }

    /// `is_legal` must agree with the allocating oracle on every move the
    /// generator produces, and reject everything else — the property that
    /// makes it safe to use instead of `legal_moves().contains()`.
    fn agrees_with_oracle(position: &Position) {
        let legal = position.legal_moves();
        for mv in &legal {
            assert!(is_legal(position, *mv), "rejected a generated move {mv:?}");
        }

        // Every from-to pair on the board, promoting and not, plus every drop
        // of every hand piece to every square: a superset of the legal set, so
        // any move `is_legal` accepts outside `legal` is a false positive.
        for from in Square::all() {
            for to in Square::all() {
                for promote in [false, true] {
                    let mv = Move::Normal { from, to, promote };
                    assert_eq!(
                        is_legal(position, mv),
                        legal.contains(&mv),
                        "disagreed with the oracle on {mv:?}"
                    );
                }
            }
        }
        for color in Color::all() {
            for piece_kind in shogi_core::Hand::all_hand_pieces() {
                for to in Square::all() {
                    let mv = Move::Drop {
                        piece: Piece::new(piece_kind, color),
                        to,
                    };
                    assert_eq!(
                        is_legal(position, mv),
                        legal.contains(&mv),
                        "disagreed with the oracle on {mv:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn agrees_with_the_oracle_at_the_initial_position() {
        agrees_with_oracle(&Position::startpos());
    }

    /// A drop-heavy middlegame: both sides hold pieces, so the drop half of the
    /// walk is actually exercised, colours included.
    #[test]
    fn agrees_with_the_oracle_in_a_drop_heavy_position() {
        agrees_with_oracle(&position(
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
        ));
    }

    /// In check, generation is restricted to evasions. Anything else must be
    /// rejected — including moves that would be legal one ply earlier.
    #[test]
    fn agrees_with_the_oracle_while_in_check() {
        let position = position("sfen 4k4/9/4+R4/9/9/9/9/9/4K4 w - 1");
        assert!(position.in_check());
        agrees_with_oracle(&position);
    }

    /// Sabotage: swap the `promotions` / `non_promotions` selection and this is
    /// the assertion that fires — the two boards overlap almost everywhere, so
    /// only a compulsory promotion tells them apart.
    #[test]
    fn a_compulsory_promotion_cannot_be_declined() {
        // A black pawn on 5b can only move to 5a, where an unpromoted pawn
        // would have no move for the rest of the game. The white king is on 1a
        // rather than 5a so that the pawn's one destination is empty — a king
        // capture is never generated, which would make the fixture vacuous.
        let position = position("sfen 8k/4P4/9/9/9/9/9/9/4K4 b - 1");
        let from = Square::new(5, 2).expect("5b");
        let to = Square::new(5, 1).expect("5a");

        assert!(is_legal(
            &position,
            Move::Normal {
                from,
                to,
                promote: true
            }
        ));
        assert!(!is_legal(
            &position,
            Move::Normal {
                from,
                to,
                promote: false
            }
        ));
    }

    /// Sabotage: drop the colour comparison in the drop arm and this passes a
    /// wrong-colour drop. It is what turns USI's colourless drop notation from
    /// a silent board corruption into a rejected command.
    #[test]
    fn a_drop_by_the_wrong_colour_is_rejected() {
        let position = position("sfen 4k4/9/9/9/9/9/9/9/4K4 b Pp 1");
        let to = Square::new(5, 5).expect("5e");

        assert!(is_legal(
            &position,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::Black),
                to
            }
        ));
        assert!(!is_legal(
            &position,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::White),
                to
            }
        ));
    }
}
