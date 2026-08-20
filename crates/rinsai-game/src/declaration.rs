//! 入玉宣言 (the 27-point rule): whether a declaration would be a win.
//!
//! Counted here from the board and the hand, because `shogi_legality_lite` has
//! no declaration surface to ask.

use std::fmt;

use shogi_core::{Color, Hand, PartialPosition, PieceKind, Square};

use crate::moves;

/// How far into the opponent's camp counts, in ranks.
const ZONE_RANKS: u8 = 3;

/// Pieces besides the king the declaring side needs inside the zone.
const REQUIRED_PIECES: u32 = 10;

/// 飛角 and their promotions, against everything else.
const MAJOR_POINTS: u32 = 5;
const OTHER_POINTS: u32 = 1;

/// Why a declaration would be refused.
///
/// Most positions refuse: this is the ordinary answer, not a fault.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeclarationError {
    /// The declaring side is not the side to move. A declaration is made in
    /// place of a move.
    NotToMove,
    /// The declaring king is outside the opponent's camp — or absent, which
    /// only a hand-built position can be.
    KingOutsideZone,
    /// The declaring king is attacked. 詰めろ and 必至 do not count.
    InCheck,
    /// `pieces` is what the declaring side has inside the zone, king excluded.
    TooFewPieces { pieces: u32 },
    /// `points` counts 飛角 and their promotions as five and everything else
    /// as one, over the zone and the whole hand, king excluded.
    TooFewPoints { points: u32, required: u32 },
}

impl fmt::Display for DeclarationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::NotToMove => f.write_str("手番ではありません"),
            Self::KingOutsideZone => f.write_str("玉が敵陣に入っていません"),
            Self::InCheck => f.write_str("王手が掛かっています"),
            Self::TooFewPieces { pieces } => {
                write!(f, "敵陣の駒が玉を除いて{pieces}枚しかありません")
            }
            Self::TooFewPoints { points, required } => {
                write!(f, "持点が{points}点で、{required}点に足りません")
            }
        }
    }
}

impl std::error::Error for DeclarationError {}

/// Whether `color` may declare 入玉宣言 in this position.
///
/// The refusal is the **first** condition the position breaks, in the rule's
/// own order, so a [`TooFewPoints`](DeclarationError::TooFewPoints) answer says
/// every other condition held.
///
/// Unlike [`in_check`](crate::in_check), the side to move is part of the
/// question: a declaration replaces a move, so a side that cannot move cannot
/// declare either.
///
/// What is not here: the clock. A declaration also needs time left, and no
/// position carries that.
pub fn can_declare(position: &PartialPosition, color: Color) -> Result<(), DeclarationError> {
    if position.side_to_move() != color {
        return Err(DeclarationError::NotToMove);
    }
    if !position
        .king_position(color)
        .is_some_and(|square| in_zone(square, color))
    {
        return Err(DeclarationError::KingOutsideZone);
    }
    if moves::in_check(position, color) {
        return Err(DeclarationError::InCheck);
    }

    let mut pieces = 0;
    let mut points = 0;
    for square in position.player_bitboard(color) {
        if !in_zone(square, color) {
            continue;
        }
        let kind = position
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
        return Err(DeclarationError::TooFewPieces { pieces });
    }

    let hand = position.hand_of_a_player(color);
    for kind in Hand::all_hand_pieces() {
        points += u32::from(hand.count(kind).unwrap_or(0)) * piece_points(kind);
    }
    let required = required_points(color);
    if points < required {
        return Err(DeclarationError::TooFewPoints { points, required });
    }
    Ok(())
}

/// The points the declaring side needs. A board holds 54, so Black's extra
/// point is what keeps both sides from clearing the bar at once.
const fn required_points(color: Color) -> u32 {
    match color {
        Color::Black => 28,
        Color::White => 27,
    }
}

fn in_zone(square: Square, color: Color) -> bool {
    square.relative_rank(color) <= ZONE_RANKS
}

/// A king is worth nothing here: the rule counts every other piece and excludes
/// it.
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
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn position(sfen: &str) -> PartialPosition {
        PartialPosition::from_usi(sfen).expect("valid sfen")
    }

    /// Black entered on the 1-file with twelve pieces behind the king and
    /// exactly the 28 points Black needs. Every fixture below is this board
    /// with one condition broken.
    const DECLARABLE_BLACK: &str = "sfen +R+R+B+BGGGGK/SSSS5/9/9/9/9/9/9/k8 b - 1";

    /// The mirror, one point lower: White needs 27, and eleven pieces carrying
    /// four majors is exactly that.
    const DECLARABLE_WHITE: &str = "sfen K8/9/9/9/9/9/9/sss6/+r+r+b+bggggk w - 1";

    /// What the fixtures assert about themselves, so a fixture that stops
    /// holding says so rather than passing for the wrong reason.
    fn zone_census(position: &PartialPosition, color: Color) -> (u32, u32) {
        let mut pieces = 0;
        let mut points = 0;
        for square in position.player_bitboard(color) {
            if !in_zone(square, color) {
                continue;
            }
            let kind = position.piece_at(square).expect("occupied").piece_kind();
            if kind == PieceKind::King {
                continue;
            }
            pieces += 1;
            points += piece_points(kind);
        }
        (pieces, points)
    }

    /// The claim `required_points` makes: the two bars cannot both be cleared,
    /// because together they ask for more than the board holds.
    ///
    /// Sabotage: `Color::Black => 27` and this fails on the strict inequality.
    #[test]
    fn the_two_bars_together_ask_for_more_than_a_board_holds() {
        let start =
            position("sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1");
        let total: u32 = Square::all()
            .filter_map(|square| start.piece_at(square))
            .map(|piece| piece_points(piece.piece_kind()))
            .sum();
        assert_eq!(total, 54, "the pieces a game starts with");
        assert!(required_points(Color::Black) + required_points(Color::White) > total);
    }

    /// The zone is read per side. Sabotage: `square.rank() <= ZONE_RANKS` in
    /// `in_zone`, and this fails on its White line having passed its Black one.
    #[test]
    fn each_side_declares_from_its_own_end_of_the_board() {
        assert_eq!(
            can_declare(&position(DECLARABLE_BLACK), Color::Black),
            Ok(())
        );
        assert_eq!(
            can_declare(&position(DECLARABLE_WHITE), Color::White),
            Ok(())
        );
    }

    #[test]
    fn a_side_that_is_not_to_move_cannot_declare() {
        assert_eq!(
            can_declare(&position(DECLARABLE_BLACK), Color::White),
            Err(DeclarationError::NotToMove)
        );
        assert_eq!(
            can_declare(&position(DECLARABLE_WHITE), Color::Black),
            Err(DeclarationError::NotToMove)
        );
    }

    /// The opening position, where the one thing wrong is the king.
    #[test]
    fn a_king_that_has_not_entered_cannot_declare() {
        let start = PartialPosition::startpos();
        assert_eq!(
            can_declare(&start, Color::Black),
            Err(DeclarationError::KingOutsideZone)
        );
        // The same board as DECLARABLE_BLACK with the king a rank short of the
        // zone, so the count and the points are still the ones that pass.
        let outside = position("sfen +R+R+B+BGGGG1/SSSS5/9/8K/9/9/9/9/k8 b - 1");
        assert_eq!(zone_census(&outside, Color::Black), (12, 28));
        assert_eq!(
            can_declare(&outside, Color::Black),
            Err(DeclarationError::KingOutsideZone)
        );
    }

    /// A rook down the file the king entered on: everything else is the
    /// declarable fixture.
    #[test]
    fn a_checked_king_cannot_declare() {
        let checked = position("sfen +R+R+B+BGGGGK/SSSS5/9/9/9/9/9/9/k7r b - 1");
        assert_eq!(zone_census(&checked, Color::Black), (12, 28));
        assert_eq!(
            can_declare(&checked, Color::Black),
            Err(DeclarationError::InCheck)
        );
    }

    /// Nine pieces and ten, at the same 28 points — a silver moves from the
    /// hand to the board and nothing else changes.
    ///
    /// ⚠️ The king stands inside the zone in both, which is what makes the
    /// nine-piece half able to fail: counting the king would make it ten.
    ///
    /// Sabotage: drop the `PieceKind::King` skip from the counting loop and
    /// this fails, with `a_piece_outside_the_zone_counts_for_nothing` red
    /// beside it.
    #[test]
    fn the_king_does_not_count_towards_the_ten() {
        let nine = position("sfen +R+R+B+BGGGGK/8S/9/9/9/9/9/9/k8 b 3S 1");
        assert_eq!(zone_census(&nine, Color::Black), (9, 25));
        assert_eq!(
            can_declare(&nine, Color::Black),
            Err(DeclarationError::TooFewPieces { pieces: 9 })
        );

        let ten = position("sfen +R+R+B+BGGGGK/7SS/9/9/9/9/9/9/k8 b 2S 1");
        assert_eq!(zone_census(&ten, Color::Black), (10, 26));
        assert_eq!(can_declare(&ten, Color::Black), Ok(()));
    }

    /// Black at 27 is refused where White at 27 is not.
    ///
    /// Sabotage: `Color::Black => 27` and this fails.
    /// `a_white_declaration_one_point_short_is_refused` is the other half —
    /// `Color::White => 28` fails that one and leaves this green.
    #[test]
    fn a_black_declaration_one_point_short_is_refused() {
        let short = position("sfen +R+R+B+BGGG1K/SSSS5/9/9/9/9/9/9/k8 b - 1");
        assert_eq!(zone_census(&short, Color::Black), (11, 27));
        assert_eq!(
            can_declare(&short, Color::Black),
            Err(DeclarationError::TooFewPoints {
                points: 27,
                required: 28,
            })
        );
    }

    /// And the mirror of it, one point below White's own bar.
    #[test]
    fn a_white_declaration_one_point_short_is_refused() {
        let short = position("sfen K8/9/9/9/9/9/9/ss7/+r+r+b+bggggk w - 1");
        assert_eq!(zone_census(&short, Color::White), (10, 26));
        assert_eq!(
            can_declare(&short, Color::White),
            Err(DeclarationError::TooFewPoints {
                points: 26,
                required: 27,
            })
        );
    }

    /// Twelve pieces either way; only what they are differs. The fixture is
    /// chosen so the majors carry more than half the total: at one point each
    /// the declarable board falls to 12, well under the bar.
    #[test]
    fn a_dragon_and_a_horse_are_worth_what_a_rook_and_a_bishop_are() {
        let generals = position("sfen GGGG4K/SSSS5/NNLL5/9/9/9/9/9/k8 b - 1");
        assert_eq!(zone_census(&generals, Color::Black), (12, 12));
        assert_eq!(
            can_declare(&generals, Color::Black),
            Err(DeclarationError::TooFewPoints {
                points: 12,
                required: 28,
            })
        );
        assert_eq!(
            zone_census(&position(DECLARABLE_BLACK), Color::Black),
            (12, 28)
        );
    }

    /// Pieces behind the zone are not the declaring side's to count.
    ///
    /// ⚠️ Every other fixture here keeps the declaring side wholly inside the
    /// zone, where dropping the mask changes nothing. These two are the ones
    /// that can fail: each is over both bars when the whole board is counted
    /// and under one when only the zone is.
    ///
    /// Sabotage: drop the `in_zone` guard from the counting loop and this is
    /// the only test in the workspace that goes red.
    #[test]
    fn a_piece_outside_the_zone_counts_for_nothing() {
        // Nine in the zone and three behind it, which together would be twelve.
        let short_of_ten = position("sfen +R+R+B+BGGGGK/8S/9/9/SSS6/9/9/9/k8 b - 1");
        assert_eq!(zone_census(&short_of_ten, Color::Black), (9, 25));
        assert_eq!(
            can_declare(&short_of_ten, Color::Black),
            Err(DeclarationError::TooFewPieces { pieces: 9 })
        );

        // Ten in the zone, two points behind it, and 28 only if both count.
        let short_of_the_points = position("sfen +R+R+B+BGGGGK/7SS/9/9/SS7/9/9/9/k8 b - 1");
        assert_eq!(zone_census(&short_of_the_points, Color::Black), (10, 26));
        assert_eq!(
            can_declare(&short_of_the_points, Color::Black),
            Err(DeclarationError::TooFewPoints {
                points: 26,
                required: 28,
            })
        );
    }

    /// The board is three points short and the hand is a bishop, so this passes
    /// only if a piece in hand is counted and counted as a major.
    ///
    /// Sabotage: stop summing the hand and this fails.
    #[test]
    fn a_major_in_hand_is_worth_five_like_one_on_the_board() {
        let held = position("sfen +R+R+BGGGGSK/SSS6/9/9/9/9/9/9/k8 b B 1");
        assert_eq!(zone_census(&held, Color::Black), (11, 23));
        assert_eq!(can_declare(&held, Color::Black), Ok(()));

        // The same board holding a gold instead: one point, and the bar is
        // four points away.
        let gold = position("sfen +R+R+BGGGGSK/SSS6/9/9/9/9/9/9/k8 b G 1");
        assert_eq!(
            can_declare(&gold, Color::Black),
            Err(DeclarationError::TooFewPoints {
                points: 24,
                required: 28,
            })
        );
    }

    /// Each position breaks two conditions, and the answer is the earlier one.
    /// A caller reads a refusal as "this one, and everything before it held".
    #[test]
    fn the_refusal_is_the_first_condition_the_rule_reaches() {
        // Checked, and with nothing at all in the zone.
        let checked = position("sfen 8K/9/9/9/9/9/9/9/k7r b - 1");
        assert_eq!(zone_census(&checked, Color::Black), (0, 0));
        assert_eq!(
            can_declare(&checked, Color::Black),
            Err(DeclarationError::InCheck)
        );

        // Outside the zone, and checked there.
        let outside = position("sfen 9/9/9/8K/9/9/9/9/k7r b - 1");
        assert_eq!(
            can_declare(&outside, Color::Black),
            Err(DeclarationError::KingOutsideZone)
        );
    }

    /// Every refusal says something, and no two say the same thing.
    ///
    /// Written as an exhaustive `match` rather than a table, so a sixth variant
    /// fails to compile here.
    #[test]
    fn every_refusal_reads_a_sentence_of_its_own() {
        let mut seen = std::collections::HashSet::new();
        for refusal in [
            DeclarationError::NotToMove,
            DeclarationError::KingOutsideZone,
            DeclarationError::InCheck,
            DeclarationError::TooFewPieces { pieces: 9 },
            DeclarationError::TooFewPoints {
                points: 27,
                required: 28,
            },
        ] {
            let sentence = match refusal {
                DeclarationError::NotToMove => "手番ではありません",
                DeclarationError::KingOutsideZone => "玉が敵陣に入っていません",
                DeclarationError::InCheck => "王手が掛かっています",
                DeclarationError::TooFewPieces { .. } => "敵陣の駒が玉を除いて9枚しかありません",
                DeclarationError::TooFewPoints { .. } => "持点が27点で、28点に足りません",
            };
            assert_eq!(refusal.to_string(), sentence, "{refusal:?}");
            assert!(seen.insert(sentence), "{refusal:?} repeats {sentence:?}");
        }
    }
}
