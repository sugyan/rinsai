//! One game: a position, its history, and the gate every move passes through.

use shogi_core::{Bitboard, Color, Move, PartialPosition, Piece, PositionStatus, Square};
use shogi_legality_lite as legality;

use crate::moves;
use crate::repetition::RepetitionIndex;
use crate::types::{MoveError, Outcome, Ply, PromotionChoice};

#[derive(Debug, Clone)]
pub struct Game {
    /// `positions[0]` is the initial position and `positions[i]` is the position
    /// after `moves[i - 1]`.
    ///
    /// Snapshots rather than replay, at one position per ply: undo is O(1), the
    /// repetition index can be decremented exactly, and jumping to an arbitrary
    /// ply is free.
    ///
    /// Invariant: `positions.len() == moves.len() + 1`, and never empty.
    positions: Vec<PartialPosition>,
    moves: Vec<Ply>,
    repetition: RepetitionIndex,
    /// `None` while the game is in progress.
    outcome: Option<Outcome>,
}

impl Game {
    #[must_use]
    pub fn startpos() -> Self {
        Self::from_position(PartialPosition::startpos())
    }

    #[must_use]
    pub fn from_position(initial: PartialPosition) -> Self {
        Self {
            repetition: RepetitionIndex::new(&initial),
            positions: vec![initial],
            moves: Vec::new(),
            outcome: None,
        }
    }

    #[must_use]
    pub fn position(&self) -> &PartialPosition {
        self.positions.last().expect("positions is never empty")
    }

    #[must_use]
    pub fn side_to_move(&self) -> Color {
        self.position().side_to_move()
    }

    /// Moves played so far — the game's ply count, not the SFEN move number.
    #[must_use]
    pub fn ply(&self) -> usize {
        self.moves.len()
    }

    #[must_use]
    pub fn moves(&self) -> &[Ply] {
        &self.moves
    }

    #[must_use]
    pub fn last_move(&self) -> Option<Move> {
        self.moves.last().map(|ply| ply.mv)
    }

    #[must_use]
    pub fn outcome(&self) -> Option<Outcome> {
        self.outcome
    }

    #[must_use]
    pub fn in_check(&self) -> bool {
        moves::in_check(self.position(), self.side_to_move())
    }

    /// Squares holding a king that is currently attacked.
    #[must_use]
    pub fn check_squares(&self) -> Bitboard {
        let mut bb = Bitboard::empty();
        for color in Color::all() {
            if moves::in_check(self.position(), color)
                && let Some(square) = self.position().king_position(color)
            {
                bb |= square;
            }
        }
        bb
    }

    /// Fully legal destinations from `from`, pin- and check-filtered.
    ///
    /// Promoting and non-promoting moves are merged here; [`Self::promotion`]
    /// separates them once a destination has been chosen.
    #[must_use]
    pub fn destinations(&self, from: Square) -> Bitboard {
        if self.outcome.is_some() {
            return Bitboard::empty();
        }
        legality::normal_from_candidates(self.position(), from)
    }

    #[must_use]
    pub fn drop_destinations(&self, piece: Piece) -> Bitboard {
        if self.outcome.is_some() {
            return Bitboard::empty();
        }
        legality::drop_candidates(self.position(), piece)
    }

    /// Which promotion choices `from -> to` offers.
    ///
    /// Asking the rules library twice is cheaper than reimplementing "歩 and 香
    /// on the last rank, 桂 on the last two" — and cannot drift from it.
    #[must_use]
    pub fn promotion(&self, from: Square, to: Square) -> PromotionChoice {
        let position = self.position();
        let plain = legality::is_legal_partial_lite(
            position,
            Move::Normal {
                from,
                to,
                promote: false,
            },
        );
        let promoted = legality::is_legal_partial_lite(
            position,
            Move::Normal {
                from,
                to,
                promote: true,
            },
        );
        match (plain, promoted) {
            (true, true) => PromotionChoice::Optional,
            (false, true) => PromotionChoice::Forced,
            _ => PromotionChoice::None,
        }
    }

    /// The single referee. Every move — a person's, an engine's `bestmove`, a
    /// replayed game record's — arrives here and nowhere else.
    pub fn play(&mut self, mv: Move) -> Result<(), MoveError> {
        if self.outcome.is_some() {
            return Err(MoveError::GameOver);
        }
        legality::is_legal_partial(self.position(), mv).map_err(MoveError::Illegal)?;

        let mut next = self.position().clone();
        next.make_move(mv)
            .expect("is_legal_partial implies make_move succeeds");

        let kifu = shogi_official_kifu::display_single_move_kansuji(self.position(), mv)
            .unwrap_or_else(|| shogi_core::ToUsi::to_usi_owned(&mv));
        // After the move the side to move is the opponent, so this asks exactly
        // "did the move just played give check".
        let gave_check = moves::in_check(&next, next.side_to_move());

        let count = self.repetition.push(&next);
        self.positions.push(next);
        self.moves.push(Ply {
            mv,
            gave_check,
            kifu,
        });

        // Mate is settled before repetition: a mating move that happens to be
        // the fourth occurrence of a position is mate, not 千日手.
        self.outcome = match legality::status_partial(self.position()) {
            PositionStatus::BlackWins => Some(Outcome::Checkmate {
                winner: Color::Black,
            }),
            PositionStatus::WhiteWins => Some(Outcome::Checkmate {
                winner: Color::White,
            }),
            // `Invalid` is not an outcome: a hand-built position the rules
            // library cannot classify stays in progress rather than ending.
            PositionStatus::Invalid => None,
            _ if count >= 4 => Some(self.classify_repetition()),
            _ => None,
        };
        Ok(())
    }

    /// Take back the last move, clearing any outcome it produced.
    pub fn undo(&mut self) -> Option<Move> {
        let ply = self.moves.pop()?;
        self.positions.pop();
        self.repetition.pop();
        self.outcome = None;
        Some(ply.mv)
    }

    pub fn resign(&mut self, loser: Color) {
        if self.outcome.is_none() {
            self.outcome = Some(Outcome::Resignation { loser });
        }
    }

    /// Tell apart 千日手 from 連続王手の千日手 once a position has occurred four
    /// times.
    ///
    /// The window runs from the *first* occurrence of the repeated position to
    /// now — the whole cycle. Engines commonly walk back only to the previous
    /// occurrence instead; this reading is strictly harder to trigger, since it
    /// demands the checks be continuous across all three cycles, which is the
    /// safe direction for a referee: it can never award a perpetual-check win
    /// that did not happen.
    fn classify_repetition(&self) -> Outcome {
        let first = self
            .repetition
            .first_occurrence()
            .expect("only called once a position has occurred four times");
        let mut all_checks = [true, true];
        let mut moved = [false, false];
        for (i, ply) in self.moves.iter().enumerate().skip(first) {
            let side = self.positions[i].side_to_move().array_index();
            moved[side] = true;
            all_checks[side] &= ply.gave_check;
        }
        match (moved[0] && all_checks[0], moved[1] && all_checks[1]) {
            (true, false) => Outcome::PerpetualCheck {
                loser: Color::Black,
            },
            (false, true) => Outcome::PerpetualCheck {
                loser: Color::White,
            },
            _ => Outcome::Repetition,
        }
    }
}

#[cfg(test)]
mod tests {
    use shogi_core::{IllegalMoveKind, PieceKind};
    use shogi_usi_parser::FromUsi;

    use super::*;

    fn sq(file: u8, rank: u8) -> Square {
        Square::new(file, rank).expect("valid square")
    }

    fn normal(from: (u8, u8), to: (u8, u8)) -> Move {
        Move::Normal {
            from: sq(from.0, from.1),
            to: sq(to.0, to.1),
            promote: false,
        }
    }

    fn game_from(sfen: &str) -> Game {
        Game::from_position(PartialPosition::from_usi(sfen).expect("valid sfen"))
    }

    #[test]
    fn the_position_count_invariant_survives_arbitrary_play_and_undo() {
        let mut game = Game::startpos();
        assert_eq!(game.positions.len(), game.moves.len() + 1);
        game.play(normal((7, 7), (7, 6))).expect("legal");
        game.play(normal((3, 3), (3, 4))).expect("legal");
        assert_eq!(game.positions.len(), game.moves.len() + 1);
        game.undo();
        assert_eq!(game.positions.len(), game.moves.len() + 1);
        game.undo();
        assert_eq!(game.positions.len(), game.moves.len() + 1);
        assert!(
            game.undo().is_none(),
            "cannot undo past the initial position"
        );
        assert_eq!(game.positions.len(), 1);
    }

    /// A refused move must not leave a trace. Sabotage note: moving the
    /// legality check after the push fails here.
    #[test]
    fn an_illegal_move_leaves_the_game_untouched() {
        let mut game = Game::startpos();
        game.play(normal((7, 7), (7, 6))).expect("legal");
        let before = game.clone();

        // 7六 to 7五 is not a pawn move for White, and it is Black's pawn anyway.
        let err = game.play(normal((7, 6), (7, 5))).unwrap_err();
        assert!(matches!(err, MoveError::Illegal(_)), "got {err:?}");

        assert_eq!(game.positions, before.positions);
        assert_eq!(game.moves.len(), before.moves.len());
        assert_eq!(game.outcome, before.outcome);
    }

    #[test]
    fn undo_restores_the_repetition_index_exactly() {
        let mut game = Game::startpos();
        game.play(normal((7, 7), (7, 6))).expect("legal");
        game.undo();
        // Playing the same move again must produce the same count, which is only
        // true if the pop was exact.
        game.play(normal((7, 7), (7, 6))).expect("legal");
        game.undo();
        assert_eq!(game.repetition.first_occurrence(), Some(0));
    }

    #[test]
    fn a_move_that_mates_ends_the_game() {
        // White king alone on 5a; a Black rook on the 5-file defends a gold
        // dropped in front of the king. 頭金.
        let mut game = game_from("sfen 4k4/9/9/9/9/9/9/9/4R3K b G 1");
        game.play(Move::Drop {
            piece: Piece::new(PieceKind::Gold, Color::Black),
            to: sq(5, 2),
        })
        .expect("the drop is legal");
        assert_eq!(
            game.outcome(),
            Some(Outcome::Checkmate {
                winner: Color::Black
            })
        );
    }

    #[test]
    fn playing_after_the_game_is_over_is_refused() {
        let mut game = Game::startpos();
        game.resign(Color::Black);
        assert_eq!(game.play(normal((7, 7), (7, 6))), Err(MoveError::GameOver));
    }

    #[test]
    fn resigning_twice_keeps_the_first_result() {
        let mut game = Game::startpos();
        game.resign(Color::Black);
        game.resign(Color::White);
        assert_eq!(
            game.outcome(),
            Some(Outcome::Resignation {
                loser: Color::Black
            })
        );
    }

    #[test]
    fn a_finished_game_offers_no_destinations() {
        let mut game = Game::startpos();
        game.resign(Color::Black);
        assert!(game.destinations(sq(7, 7)).is_empty());
        assert!(
            game.drop_destinations(Piece::new(PieceKind::Pawn, Color::Black))
                .is_empty()
        );
    }

    #[test]
    fn promotion_is_optional_in_the_zone_forced_on_the_last_rank_and_absent_outside() {
        // A lone Black pawn free to walk up the 5-file.
        let game = game_from("sfen 4k4/9/4P4/9/9/9/9/9/4K4 b - 1");
        assert_eq!(
            game.promotion(sq(5, 3), sq(5, 2)),
            PromotionChoice::Optional,
            "5三歩 to 5二 is inside the promotion zone"
        );

        // The enemy king is tucked into a corner so 5一 is genuinely empty:
        // walking a pawn onto the king would be a capture, not a promotion.
        let game = game_from("sfen k8/4P4/9/9/9/9/9/9/4K4 b - 1");
        assert_eq!(
            game.promotion(sq(5, 2), sq(5, 1)),
            PromotionChoice::Forced,
            "a pawn reaching the last rank has nowhere to go unpromoted"
        );

        let game = game_from("sfen 4k4/9/9/9/4P4/9/9/9/4K4 b - 1");
        assert_eq!(
            game.promotion(sq(5, 5), sq(5, 4)),
            PromotionChoice::None,
            "outside the zone there is nothing to choose"
        );
    }

    #[test]
    fn a_forced_promotion_is_still_reachable_as_a_destination() {
        let game = game_from("sfen k8/4P4/9/9/9/9/9/9/4K4 b - 1");
        assert!(
            game.destinations(sq(5, 2)).contains(sq(5, 1)),
            "the merged candidate set includes promote-only moves"
        );
    }

    /// Shuffling rooks back and forth returns to the start position; the fourth
    /// arrival is 千日手 and nobody was checking.
    #[test]
    fn a_four_fold_repetition_is_a_draw() {
        let mut game = Game::startpos();
        let cycle = [
            ((2, 8), (3, 8)),
            ((8, 2), (7, 2)),
            ((3, 8), (2, 8)),
            ((7, 2), (8, 2)),
        ];
        for _ in 0..3 {
            for (from, to) in cycle {
                game.play(normal(from, to)).expect("rook shuffle is legal");
            }
        }
        assert_eq!(game.outcome(), Some(Outcome::Repetition));
    }

    /// The same shape, but Black checks on every one of its moves — so Black
    /// loses rather than the game being drawn.
    #[test]
    fn a_perpetual_check_loses_for_the_checking_side() {
        // White king alone on 5a, Black king tucked away on 9i, Black rook on 1i.
        let mut game = game_from("sfen 4k4/9/9/9/9/9/9/9/K7R b - 1");
        game.play(normal((1, 9), (1, 1)))
            .expect("rook to 1a checks");
        let cycle = [
            ((5, 1), (5, 2)), // White king steps off the checked rank
            ((1, 1), (1, 2)), // Black checks again
            ((5, 2), (5, 1)),
            ((1, 2), (1, 1)),
        ];
        for _ in 0..3 {
            for (from, to) in cycle {
                if game.outcome().is_some() {
                    break;
                }
                game.play(normal(from, to)).expect("cycle move is legal");
            }
        }
        assert_eq!(
            game.outcome(),
            Some(Outcome::PerpetualCheck {
                loser: Color::Black
            }),
            "Black gave check on every move of the cycle"
        );
    }

    #[test]
    fn check_squares_names_the_attacked_king() {
        let mut game = game_from("sfen 4k4/9/9/9/9/9/9/9/K7R b - 1");
        assert!(game.check_squares().is_empty());
        game.play(normal((1, 9), (1, 1)))
            .expect("rook to 1a checks");
        assert_eq!(game.check_squares(), Bitboard::single(sq(5, 1)));
        assert!(game.in_check(), "White is to move and is checked");
    }

    #[test]
    fn kifu_text_is_recorded_in_official_notation() {
        let mut game = Game::startpos();
        game.play(normal((7, 7), (7, 6))).expect("legal");
        assert_eq!(game.moves()[0].kifu, "▲７六歩");
        assert!(!game.moves()[0].gave_check);
    }

    #[test]
    fn two_pawns_is_reported_with_its_own_reason() {
        // Black already has a pawn on the 5-file and tries to drop another.
        let mut game = game_from("sfen 4k4/9/9/9/4P4/9/9/9/4K4 b P 1");
        let err = game
            .play(Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::Black),
                to: sq(5, 3),
            })
            .unwrap_err();
        assert_eq!(err, MoveError::Illegal(IllegalMoveKind::TwoPawns));
        assert_eq!(err.to_string(), "二歩です");
    }
}
