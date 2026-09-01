//! 入玉宣言 under CSA's 宣言法 — the 27-point rule `bestmove win` means.
//!
//! ⚠️ **A refused claim loses the game**: a server scores it a foul, and so
//! does this project's own referee. Nothing here may answer `true` for a
//! position it has not counted.
//!
//! The referee owns the same rule on `shogi_core`'s accessors, in a crate that
//! does not link shunsai. This one reads shunsai's, and the two are held to one
//! answer by `the_two_implementations_agree` below.

use shogi_core::{Color, Hand, PieceKind, Square};
use shunsai::Position;

/// How far into the opponent's camp counts, in ranks.
const ZONE_RANKS: u8 = 3;

/// Pieces besides the king the declaring side needs inside the zone.
const REQUIRED_PIECES: u32 = 10;

/// 飛角 and their promotions, against everything else.
const MAJOR_POINTS: u32 = 5;
const OTHER_POINTS: u32 = 1;

/// Whether the **side to move** may declare 入玉宣言.
///
/// The side to move is not a parameter because a declaration replaces a move:
/// no other side has the question to ask. That is also what makes
/// [`Position::in_check`], which answers for the side to move alone, the right
/// reading here.
///
/// What is not here: the clock. A declaration also needs time left, and no
/// position carries that.
///
/// Named caller: the searcher, at the root.
pub(crate) fn can_declare(board: &Position) -> bool {
    let us = board.side_to_move();
    if !board
        .king_square(us)
        .is_some_and(|square| in_zone(square, us))
    {
        return false;
    }
    if board.in_check() {
        return false;
    }

    let mut pieces = 0;
    let mut points = 0;
    for square in board.player_bb(us) {
        if !in_zone(square, us) {
            continue;
        }
        let kind = board
            .piece_at(square)
            .expect("a player's bitboard names occupied squares")
            .piece_kind();
        if kind == PieceKind::King {
            continue;
        }
        pieces += 1;
        points += piece_points(kind);
    }
    if pieces < REQUIRED_PIECES {
        return false;
    }

    let hand = board.hand(us);
    for kind in Hand::all_hand_pieces() {
        points += u32::from(hand.count(kind).unwrap_or(0)) * piece_points(kind);
    }
    points >= required_points(us)
}

/// The points the declaring side needs. Black's extra point is what keeps both
/// sides from clearing the bar at once.
const fn required_points(color: Color) -> u32 {
    match color {
        Color::Black => 28,
        Color::White => 27,
    }
}

fn in_zone(square: Square, color: Color) -> bool {
    square.relative_rank(color) <= ZONE_RANKS
}

fn piece_points(kind: PieceKind) -> u32 {
    // `unpromote` is the promoted-piece test as well as the map, so 龍 and 馬
    // reach the same arm as 飛 and 角 without a table of their own.
    match kind.unpromote().unwrap_or(kind) {
        PieceKind::Rook | PieceKind::Bishop => MAJOR_POINTS,
        PieceKind::King => 0,
        _ => OTHER_POINTS,
    }
}

#[cfg(test)]
mod tests {
    use shogi_core::PartialPosition;
    use shogi_usi_parser::FromUsi;

    use super::*;

    /// Every fixture, with the answer both implementations owe it.
    ///
    /// ⚠️ **The table is hand-built around the boundaries, and it has to be.**
    /// A table drawn from played games would answer `false` on the king alone
    /// almost everywhere, and two implementations agreeing that a middlegame is
    /// not a declaration is an agreement that cannot fail. The rows here sit on
    /// the bars instead: the pairs one point, one piece and one rank apart are
    /// what make a constant that moved change an answer.
    const FIXTURES: &[(&str, bool, &str)] = &[
        (
            "sfen +R+R+B+BGGGGK/SSSS5/9/9/9/9/9/9/k8 b - 1",
            true,
            "Black, twelve pieces in the zone, exactly the 28 Black needs",
        ),
        (
            "sfen +R+R+B+BGGG1K/SSSS5/9/9/9/9/9/9/k8 b - 1",
            false,
            "the same board at 27 — which is White's bar, not Black's",
        ),
        (
            "sfen K8/9/9/9/9/9/9/sss6/+r+r+b+bggggk w - 1",
            true,
            "White, eleven pieces, exactly the 27 White needs",
        ),
        (
            "sfen K8/9/9/9/9/9/9/ss7/+r+r+b+bggggk w - 1",
            false,
            "the same board at 26",
        ),
        (
            "sfen +R+R+B+BGGGGK/8S/9/9/9/9/9/9/k8 b 3S 1",
            false,
            "nine pieces in the zone; the king may not make it ten",
        ),
        (
            "sfen +R+R+B+BGGGGK/7SS/9/9/9/9/9/9/k8 b 2S 1",
            true,
            "ten pieces, and the two silvers in hand carry it to 28",
        ),
        (
            "sfen RRBBGGGGK/SSSS5/9/9/9/9/9/9/k8 b - 1",
            true,
            "plain 飛角 score what 龍馬 do",
        ),
        (
            "sfen GGGG4K/SSSS5/NNLL5/9/9/9/9/9/k8 b - 1",
            false,
            "twelve pieces and no major: twelve points",
        ),
        (
            "sfen +R+R+BGGGGSK/SSS6/9/9/9/9/9/9/k8 b B 1",
            true,
            "23 on the board and a bishop in hand — a major in hand is five",
        ),
        (
            "sfen +R+R+BGGGGSK/SSS6/9/9/9/9/9/9/k8 b G 1",
            false,
            "the same board holding a gold instead",
        ),
        (
            "sfen +R+R+B+BGGGGK/8S/9/9/SSS6/9/9/9/k8 b - 1",
            false,
            "twelve pieces, but three of them behind the zone",
        ),
        (
            "sfen +R+R+B+BGGGGK/7SS/9/9/SS7/9/9/9/k8 b - 1",
            false,
            "28 points only if the two behind the zone are counted",
        ),
        (
            "sfen +R+R+B+BGGGG1/SSSS5/9/8K/9/9/9/9/k8 b - 1",
            false,
            "the declarable count, with the king a rank short of the zone",
        ),
        (
            "sfen +R+R+B+BGGGGK/SSS6/S8/9/9/9/9/9/k8 b - 1",
            true,
            "the twelfth piece stands on the third rank, and 28 needs it",
        ),
        (
            "sfen +R+R+B+BGGGG1/SSSS5/8K/9/9/9/9/9/k8 b - 1",
            true,
            "the king itself stands on the third rank",
        ),
        (
            "sfen +R+R+B+BGGGGK/SSSS5/9/9/9/9/9/9/k7r b - 1",
            false,
            "the declarable board, with a rook down the king's file",
        ),
        (
            "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1",
            false,
            "a game that has not started",
        ),
    ];

    fn partial(sfen: &str) -> PartialPosition {
        PartialPosition::from_usi(sfen).expect("valid sfen")
    }

    /// The table's own answers, so a table that drifted says so rather than
    /// letting two implementations agree on the wrong thing.
    ///
    /// Sabotage: fifteen mutations, each applied at its own site — `ZONE_RANKS`
    /// to 2 and to 4, `REQUIRED_PIECES` to 9 and to 11, `MAJOR_POINTS` to 1,
    /// `OTHER_POINTS` to 0, each arm of `required_points` moved to the other's
    /// value, each of the five blocks deleted (the king-in-zone guard, the 王手
    /// guard, the king skip, the zone mask, the hand loop), and each of the two
    /// bars made strict. Every one turned this test red, and
    /// `the_two_implementations_agree` with it.
    ///
    /// ⚠️ **`ZONE_RANKS` to 2 was green until two fixtures were added for it**:
    /// every row then put its pieces and its king on the first two ranks, so
    /// narrowing the zone changed nothing. The two rows naming the third rank
    /// are what that mutation goes through.
    #[test]
    fn the_fixtures_answer_what_they_claim() {
        for &(sfen, expected, why) in FIXTURES {
            let board = Position::new(partial(sfen));
            assert_eq!(can_declare(&board), expected, "{why}: {sfen}");
        }
    }

    /// Both bars are read from both sides: each colour has a fixture that
    /// declares and one that is refused.
    ///
    /// ⚠️ Sabotage: writing every row `false` leaves
    /// `the_two_implementations_agree` **green** — an agreement test cannot see
    /// a table that stopped deciding — and turns this one and
    /// `the_fixtures_answer_what_they_claim` red.
    #[test]
    fn the_fixtures_decide_in_both_directions() {
        for color in [Color::Black, Color::White] {
            for wanted in [true, false] {
                assert!(
                    FIXTURES.iter().any(|&(sfen, expected, _)| {
                        expected == wanted && partial(sfen).side_to_move() == color
                    }),
                    "no fixture has {color:?} answered {wanted}"
                );
            }
        }
    }

    /// Put every fixture through the referee's implementation as well, and
    /// demand one answer.
    ///
    /// The two read different boards — `shogi_core`'s accessors against
    /// shunsai's bitboards — and reach 王手 by different libraries, so this
    /// binds the numbers that are private to each: the zone, the ten pieces,
    /// the point map, and the 28/27 asymmetry.
    ///
    /// ⚠️ **The referee is asked about the side to move**, which is the only
    /// side `can_declare` here has an answer for. Its `NotToMove` refusal has
    /// no counterpart on this side and is not covered.
    #[test]
    fn the_two_implementations_agree() {
        for &(sfen, _, why) in FIXTURES {
            let position = partial(sfen);
            let refereed = rinsai_game::can_declare(&position, position.side_to_move());
            let searched = can_declare(&Position::new(position));
            assert_eq!(refereed.is_ok(), searched, "{why}: {sfen} — {refereed:?}");
        }
    }
}
