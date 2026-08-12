//! One game: a position, its history, and the gate every move passes through.

use shogi_core::{Bitboard, Color, Move, PartialPosition, Piece, PositionStatus, Square};
use shogi_legality_lite as legality;

use shogi_usi_parser::FromUsi;

use crate::moves;
use crate::repetition::RepetitionIndex;
use crate::types::{MoveError, Outcome, Ply, PromotionChoice, UsiMoveError, UsiPositionError};

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

    /// Build a game from the argument of a USI `position` command:
    /// `startpos [moves …]` or `sfen <board> <side> <hands> [<ply>] [moves …]`.
    ///
    /// Every move in the list goes through [`Self::play_usi`], so each is
    /// legality-checked and the game is adjudicated as it is replayed. A list
    /// that continues past a rule-decided ending is therefore refused at the
    /// first move after the end — a game record cannot both end by rule and
    /// keep going.
    pub fn from_usi_position(args: &str) -> Result<Self, UsiPositionError> {
        let mut tokens = args.split_whitespace().peekable();
        if tokens.peek().is_none() {
            return Err(UsiPositionError::Empty);
        }

        let mut root = String::new();
        while let Some(&token) = tokens.peek() {
            if token == "moves" {
                break;
            }
            if !root.is_empty() {
                root.push(' ');
            }
            root.push_str(token);
            tokens.next();
        }
        let initial = PartialPosition::from_usi(&root).map_err(UsiPositionError::Root)?;
        let mut game = Self::from_position(initial);

        if tokens.next().is_some() {
            for (index, token) in tokens.enumerate() {
                game.play_usi(token)
                    .map_err(|source| UsiPositionError::Move {
                        index,
                        token: token.to_owned(),
                        source,
                    })?;
            }
        }
        Ok(game)
    }

    /// Parse one USI move token for the side to move and [`play`](Self::play)
    /// it.
    ///
    /// Parsing goes through [`crate::move_from_usi`], which is what re-colours
    /// a drop — USI drop notation carries no colour, and the parser hard-codes
    /// Black.
    pub fn play_usi(&mut self, token: &str) -> Result<Move, UsiMoveError> {
        let mv = moves::move_from_usi(token, self.side_to_move()).map_err(UsiMoveError::Syntax)?;
        self.play(mv).map_err(UsiMoveError::Refused)?;
        Ok(mv)
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
    fn a_position_argument_replays_its_moves_onto_its_root() {
        let game = Game::from_usi_position("startpos moves 7g7f 3c3d").expect("valid");
        assert_eq!(game.ply(), 2);
        assert_eq!(game.side_to_move(), Color::Black);

        let game = Game::from_usi_position("sfen 4k4/9/9/9/9/9/9/9/4K4 b - 1").expect("valid");
        assert_eq!(game.ply(), 0);

        let game =
            Game::from_usi_position("  startpos   moves  7g7f ").expect("stray spaces are fine");
        assert_eq!(game.ply(), 1);
    }

    #[test]
    fn a_bad_root_and_a_bad_move_are_told_apart() {
        assert!(matches!(
            Game::from_usi_position(""),
            Err(UsiPositionError::Empty)
        ));
        assert!(matches!(
            Game::from_usi_position("sfen what"),
            Err(UsiPositionError::Root(_))
        ));
        assert!(matches!(
            Game::from_usi_position("startpos moves 7g7f xyzzy"),
            Err(UsiPositionError::Move {
                index: 1,
                source: UsiMoveError::Syntax(_),
                ..
            })
        ));
        assert!(matches!(
            Game::from_usi_position("startpos moves 7g7f 7g7f"),
            Err(UsiPositionError::Move {
                index: 1,
                source: UsiMoveError::Refused(MoveError::Illegal(_)),
                ..
            })
        ));
    }

    /// The rook shuffle ends the game at its twelfth move; a thirteenth is a
    /// move played into a finished game, and the referee refuses it.
    #[test]
    fn a_move_list_running_past_a_rule_decided_ending_is_refused() {
        let shuffle = "2h3h 8b7b 3h2h 7b8b 2h3h 8b7b 3h2h 7b8b 2h3h 8b7b 3h2h 7b8b";
        let ended = Game::from_usi_position(&format!("startpos moves {shuffle}")).expect("valid");
        assert_eq!(ended.outcome(), Some(Outcome::Repetition));

        let err = Game::from_usi_position(&format!("startpos moves {shuffle} 2h3h")).unwrap_err();
        assert!(matches!(
            err,
            UsiPositionError::Move {
                index: 12,
                source: UsiMoveError::Refused(MoveError::GameOver),
                ..
            }
        ));
    }

    /// The drop-recolouring trap, end to end: the token names no colour, so the
    /// side to move decides it.
    #[test]
    fn play_usi_recolours_a_drop_to_the_side_to_move() {
        let mut game = game_from("sfen 4k4/9/9/9/9/9/9/9/4K4 w p 1");
        let mv = game.play_usi("P*5e").expect("legal drop");
        assert_eq!(
            mv,
            Move::Drop {
                piece: Piece::new(PieceKind::Pawn, Color::White),
                to: sq(5, 5),
            }
        );
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
