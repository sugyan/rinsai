//! Material evaluation.
//!
//! Counts pieces, weights them, and reports the difference from the side to
//! move. That is the whole of E0's evaluation: no piece-square tables, no king
//! safety, no mobility. E3 replaces all of it with a network, so the value of
//! anything spent refining these numbers is bounded by how long they live.
//!
//! **The values are ours.** CLAUDE.md forbids reusing a table from a GPL
//! engine, so the ordering is derived from the rules of shogi — empty-board
//! destination counts, slider or stepper, colour-bound or not, promotion
//! potential — the magnitudes are interpolated on that ordering, and everything
//! is rounded to a multiple of five so that nobody mistakes them for fitted
//! values. They are a **starting point for SPSA at E4**, not a measurement.
//! The derivation is written out in CONVENTIONS.md; the invariants it implies are
//! asserted below.

use shogi_core::{Hand, PieceKind};
use shunsai::Position;

use crate::score::Score;

/// What a piece is worth standing on the board, indexed by
/// [`PieceKind::array_index`].
///
/// The king is zero: it is never captured in legal play and both sides have at
/// most one, so pricing it only invites a lone-king fixture to evaluate as a
/// forced win. A promoted minor is worth exactly a gold, because it *moves*
/// exactly as a gold and any other number would be a claim about the position
/// rather than about the material.
const BOARD: [i32; PieceKind::NUM] = [
    100,  // Pawn
    350,  // Lance
    400,  // Knight
    550,  // Silver
    600,  // Gold
    850,  // Bishop
    1000, // Rook
    0,    // King
    600,  // ProPawn      と
    600,  // ProLance     成香
    600,  // ProKnight    成桂
    600,  // ProSilver    成銀
    1150, // ProBishop    馬
    1350, // ProRook      龍
];

/// What the same piece is worth held in hand, indexed the same way. Only the
/// seven droppable kinds are non-zero.
///
/// Every entry is 10–15% above its board counterpart. A piece in hand has
/// maximal optionality — any empty square, subject only to 二歩, 打ち歩詰め and
/// the no-dead-piece rule — and cannot be captured while it is there; the
/// counterweight is that deploying it costs a tempo, which is why the premium
/// is a tenth and not a half. It is largest for the kinds whose *board*
/// placement is most constrained: a pawn or a lance commits to a file, and a
/// bishop on the board is routinely blocked where a bishop in hand never is.
const HAND: [i32; PieceKind::NUM] = [
    115,  // Pawn   +15%
    400,  // Lance  +14%
    450,  // Knight +13%
    610,  // Silver +11%
    660,  // Gold   +10%
    950,  // Bishop +12%
    1100, // Rook   +10%
    0, 0, 0, 0, 0, 0, 0,
];

/// The material balance in centipawns, **from the side to move**.
///
/// Positive is good for whoever is to move; a parent in the search negates its
/// child. There is no `clamp_to_eval` here on purpose — the largest balance any
/// legal position can hold is far below the mate band, and
/// `no_material_balance_can_reach_the_mate_band` asserts it. A clamp would turn
/// a broken value table into a quietly plausible score.
pub(crate) fn evaluate(position: &Position) -> Score {
    let us = position.side_to_move();
    let ours = position.player_bb(us);
    let theirs = position.player_bb(us.flip());

    let mut total: i32 = 0;
    for kind in PieceKind::all() {
        let value = BOARD[kind.array_index()];
        if value == 0 {
            continue; // the king, and only the king
        }
        // `piece_kind_bb` holds both colours, hence the two intersections.
        // `count()` is `Bitboard`'s inherent popcount, not `Iterator::count` —
        // `Bitboard` *is* the iterator, so the two are one method call apart.
        // `expect` rather than `unwrap_or(0)`: a board holds at most 81 pieces,
        // so this cannot fail — and if it ever does, a silent zero is a wrong
        // evaluation nobody would notice.
        let bb = position.piece_kind_bb(kind);
        let ours = i32::try_from((bb & ours).count()).expect("a bitboard holds at most 81 squares");
        let theirs =
            i32::try_from((bb & theirs).count()).expect("a bitboard holds at most 81 squares");
        total += value * (ours - theirs);
    }

    let our_hand = position.hand(us);
    let their_hand = position.hand(us.flip());
    for kind in Hand::all_hand_pieces() {
        let ours = i32::from(our_hand.count(kind).unwrap_or(0));
        let theirs = i32::from(their_hand.count(kind).unwrap_or(0));
        total += HAND[kind.array_index()] * (ours - theirs);
    }

    let score = Score::cp(total);
    debug_assert!(
        !score.is_mate(),
        "material evaluation reached the mate band: {score:?}"
    );
    score
}

#[cfg(test)]
mod tests {
    use shogi_core::{Color, Move, PartialPosition, Square};
    use shogi_usi_parser::FromUsi;

    use super::*;
    use crate::moves::is_legal;
    use crate::score::MAX_PLY;

    fn position(sfen: &str) -> Position {
        Position::new(PartialPosition::from_usi(sfen).expect("test SFEN parses"))
    }

    /// The same balance, computed the obvious slow way.
    ///
    /// It reads the **same tables** as [`evaluate`], deliberately: what the
    /// differential is here to check is the *bitboard traversal* — which colour
    /// a set bit belongs to, whether a promoted kind is counted once or twice,
    /// whether hands are added from the right side — and duplicating the
    /// numbers as well would only test that two copies of a typo agree.
    /// The numbers are pinned separately, by `the_table_is_indexed_by_kind`.
    fn evaluate_naively(position: &Position) -> Score {
        let us = position.side_to_move();
        let mut total: i32 = 0;

        for square in Square::all() {
            if let Some(piece) = position.piece_at(square) {
                let (kind, color) = piece.to_parts();
                let value = BOARD[kind.array_index()];
                total += if color == us { value } else { -value };
            }
        }
        for color in Color::all() {
            let hand = position.hand(color);
            for kind in Hand::all_hand_pieces() {
                let value = HAND[kind.array_index()] * i32::from(hand.count(kind).unwrap_or(0));
                total += if color == us { value } else { -value };
            }
        }

        Score::cp(total)
    }

    #[test]
    fn the_initial_position_is_materially_equal() {
        assert_eq!(evaluate(&Position::startpos()), Score::ZERO);
    }

    /// Every value sits where `array_index()` will look for it.
    ///
    /// This is the test that lets the oracle above share the tables: a
    /// mis-ordered array is caught here, by name, rather than by two
    /// implementations agreeing on the same wrong number.
    #[test]
    fn the_table_is_indexed_by_kind() {
        let expected = [
            (PieceKind::Pawn, 100, 115),
            (PieceKind::Lance, 350, 400),
            (PieceKind::Knight, 400, 450),
            (PieceKind::Silver, 550, 610),
            (PieceKind::Gold, 600, 660),
            (PieceKind::Bishop, 850, 950),
            (PieceKind::Rook, 1000, 1100),
            (PieceKind::King, 0, 0),
            (PieceKind::ProPawn, 600, 0),
            (PieceKind::ProLance, 600, 0),
            (PieceKind::ProKnight, 600, 0),
            (PieceKind::ProSilver, 600, 0),
            (PieceKind::ProBishop, 1150, 0),
            (PieceKind::ProRook, 1350, 0),
        ];
        assert_eq!(expected.len(), PieceKind::NUM);
        for (kind, board, hand) in expected {
            assert_eq!(BOARD[kind.array_index()], board, "board value of {kind:?}");
            assert_eq!(HAND[kind.array_index()], hand, "hand value of {kind:?}");
        }
    }

    /// The ordering the rules of shogi force, plus the two modelling choices.
    #[test]
    fn the_table_is_ordered_the_way_shogi_is() {
        let v = |kind: PieceKind| BOARD[kind.array_index()];
        assert!(v(PieceKind::Rook) > v(PieceKind::Bishop));
        assert!(
            v(PieceKind::Bishop) > v(PieceKind::Gold)
                && v(PieceKind::Gold) > v(PieceKind::Silver)
                && v(PieceKind::Silver) > v(PieceKind::Knight)
                && v(PieceKind::Knight) > v(PieceKind::Lance)
                && v(PieceKind::Lance) > v(PieceKind::Pawn)
        );
        assert!(v(PieceKind::ProRook) > v(PieceKind::ProBishop));
        assert!(v(PieceKind::ProBishop) > v(PieceKind::Rook));

        // A promoted minor moves exactly as a gold, so it is worth one.
        for kind in [
            PieceKind::ProPawn,
            PieceKind::ProLance,
            PieceKind::ProKnight,
            PieceKind::ProSilver,
        ] {
            assert_eq!(v(kind), v(PieceKind::Gold), "{kind:?}");
        }

        // Held is worth more than placed, for every droppable kind.
        for kind in Hand::all_hand_pieces() {
            let (board, hand) = (BOARD[kind.array_index()], HAND[kind.array_index()]);
            assert!(
                hand > board,
                "{kind:?}: hand {hand} not above board {board}"
            );
            assert!(hand * 100 <= board * 115, "{kind:?}: premium above 15%");
        }
    }

    /// Round numbers, because they are chosen rather than fitted. A value that
    /// is not a multiple of five is a typo or a stray tuning result.
    #[test]
    fn every_value_is_a_round_number() {
        for kind in PieceKind::all() {
            assert_eq!(BOARD[kind.array_index()] % 5, 0, "board {kind:?}");
            assert_eq!(HAND[kind.array_index()] % 5, 0, "hand {kind:?}");
        }
    }

    /// The bound that makes `Score::clamp_to_eval` unnecessary at step 2.
    ///
    /// Every piece, counted at whichever of its three prices is highest, on one
    /// side and none on the other — a balance no legal position can exceed.
    #[test]
    fn no_material_balance_can_reach_the_mate_band() {
        let set = [
            (PieceKind::Pawn, 18),
            (PieceKind::Lance, 4),
            (PieceKind::Knight, 4),
            (PieceKind::Silver, 4),
            (PieceKind::Gold, 4),
            (PieceKind::Bishop, 2),
            (PieceKind::Rook, 2),
        ];
        let mut most = 0;
        for (kind, count) in set {
            let promoted = kind.promote().map_or(0, |p| BOARD[p.array_index()]);
            let dearest = BOARD[kind.array_index()]
                .max(HAND[kind.array_index()])
                .max(promoted);
            most += dearest * count;
        }
        assert!(
            !Score::cp(most).is_mate() && !Score::cp(-most).is_mate(),
            "the largest material balance ({most} cp) reaches the mate band, \
             which starts at {} cp",
            Score::MATE.get() - MAX_PLY as i32
        );
    }

    /// Same board, opposite side to move: the balance flips sign.
    ///
    /// Sabotage: drop the `us`/`us.flip()` distinction in either loop and this
    /// is the assertion that fires.
    #[test]
    fn evaluation_is_from_the_side_to_move() {
        let black = evaluate(&position("sfen 4k4/9/9/9/9/9/9/9/4K4 b R 1"));
        let white = evaluate(&position("sfen 4k4/9/9/9/9/9/9/9/4K4 w R 1"));
        assert_eq!(black, Score::cp(HAND[PieceKind::Rook.array_index()]));
        assert_eq!(white, -black);
    }

    /// Capturing gains the piece's board value **and** its unpromoted value in
    /// hand — the one non-obvious consequence of the model, and the reason
    /// と金 is worth more than a gold to take.
    #[test]
    fn a_capture_is_worth_the_board_value_plus_the_hand_value() {
        // A black rook on 5f, a white silver directly above it on 5e.
        let mut board = position("sfen 4k4/9/9/9/4s4/4R4/9/9/4K4 b - 1");
        let before = evaluate(&board);

        let mv = Move::Normal {
            from: Square::new(5, 6).expect("5f"),
            to: Square::new(5, 5).expect("5e"),
            promote: false,
        };
        assert!(is_legal(&board, mv), "the fixture's capture is legal");
        board.do_move(mv);

        // `evaluate` is from the side to move, and that has changed hands, so
        // the gain to the capturing side is the sum rather than the difference.
        let gain = (-evaluate(&board)).get() - before.get();
        assert_eq!(
            gain,
            BOARD[PieceKind::Silver.array_index()] + HAND[PieceKind::Silver.array_index()]
        );
    }

    /// The differential, over positions chosen to exercise each half.
    #[test]
    fn the_fast_path_agrees_with_the_oracle() {
        for sfen in [
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            // Both sides holding pieces, so the hand half is actually walked.
            "sfen l6nl/5+P1gk/2np1S3/p1p4Pp/3P2Sp1/1PPb2P1P/P5GS1/R8/LN4bKL w RGgsn5p 1",
            // Promoted pieces of both colours.
            "sfen 4k4/9/4+P4/4+r4/9/4+B4/4+n4/9/4K4 b GSgs 1",
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1",
            "sfen 9/9/9/9/9/9/9/9/9 b - 1",
        ] {
            let board = position(sfen);
            assert_eq!(evaluate(&board), evaluate_naively(&board), "{sfen}");
        }
    }

    /// …and over a whole game, so that positions nobody thought to write down
    /// get walked too.
    ///
    /// The playout is seeded and the generator is five lines inline rather than
    /// a `rand` dependency: every dependency is provenance-scan surface
    /// (CLAUDE.md), and this one would buy nothing a xorshift does not.
    #[test]
    fn the_fast_path_agrees_with_the_oracle_through_a_whole_game() {
        fn next(state: &mut u64) -> u64 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            *state
        }

        let mut board = Position::startpos();
        let mut state = 0x2026_0806_5348_4f47_u64;
        for ply in 0..200 {
            assert_eq!(
                evaluate(&board),
                evaluate_naively(&board),
                "disagreed at ply {ply} of the playout: {}",
                board.key()
            );
            let moves = board.legal_moves();
            let Ok(len) = u64::try_from(moves.len()) else {
                break;
            };
            if len == 0 {
                break; // mated; there is nothing left to play
            }
            let index = usize::try_from(next(&mut state) % len).expect("index fits");
            board.do_move(moves[index]);
        }
    }
}
