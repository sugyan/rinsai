//! One game: a position, its history, and the gate every move passes through.

use shogi_core::{
    Bitboard, Color, IllegalMoveKind, Move, PartialPosition, Piece, PositionStatus, Square,
};
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

    /// The position the game began at.
    ///
    /// ⚠️ Not [`Self::position`], which answers the position *now*. A root
    /// taken from that one names the position the game has **reached**, and the
    /// `moves` list is then replayed onto a position it was not played from.
    #[must_use]
    pub fn initial_position(&self) -> &PartialPosition {
        self.positions.first().expect("positions is never empty")
    }

    /// The argument of a USI `position` command for the game so far, and the
    /// round-trip partner of [`Self::from_usi_position`]: `startpos [moves …]`
    /// where the root equals `PartialPosition::startpos()`, move number
    /// included, and `sfen <board> <side> <hands> <ply> [moves …]` otherwise.
    ///
    /// ⚠️ It answers with the game, not with the text the game was built from.
    /// A root spelled `sfen` that meets that equality comes back as `startpos`,
    /// and an omitted move number comes back as `1`.
    #[must_use]
    pub fn to_usi_position(&self) -> String {
        let root = self.initial_position();
        let mut args = if *root == PartialPosition::startpos() {
            "startpos".to_owned()
        } else {
            // `to_sfen_owned` writes the fields and not the keyword.
            format!("sfen {}", root.to_sfen_owned())
        };
        if !self.moves.is_empty() {
            args.push_str(" moves");
            for ply in &self.moves {
                args.push(' ');
                args.push_str(&shogi_core::ToUsi::to_usi_owned(&ply.mv));
            }
        }
        args
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
            // ⚠️ Not an outcome, and not a no-op either: a position the
            // rules library calls invalid never reaches the repetition arm
            // below, so such a game runs on past a fourfold repetition.
            PositionStatus::Invalid => None,
            _ if count >= 4 => Some(self.classify_repetition()),
            _ => None,
        };
        Ok(())
    }

    /// Take back the last move, clearing any outcome the game had.
    ///
    /// ⚠️ Including one that was adjudicated rather than played into: undoing
    /// past a flag fall revives the game, so a caller that told its players the
    /// result owes them a fresh one.
    pub fn undo(&mut self) -> Option<Move> {
        let ply = self.moves.pop()?;
        self.positions.pop();
        self.repetition.pop();
        self.outcome = None;
        Some(ply.mv)
    }

    pub fn resign(&mut self, loser: Color) {
        self.adjudicate(Outcome::Resignation { loser });
    }

    /// 時間切れ — `loser` did not move inside the time it had.
    pub fn flag_fall(&mut self, loser: Color) {
        self.adjudicate(Outcome::FlagFall { loser });
    }

    /// 反則負け — `loser` offered a move the referee refused.
    ///
    /// ⚠️ `kind` is [`play`](Self::play)'s own refusal, taken from the
    /// [`MoveError::Illegal`] it answered with. A kind the caller decided on
    /// instead states a verdict this crate did not reach, which is the whole
    /// reason the refereeing lives here.
    pub fn foul(&mut self, loser: Color, kind: IllegalMoveKind) {
        self.adjudicate(Outcome::IllegalMove { loser, kind });
    }

    /// 入玉宣言 — `winner` declared, and the declaration is **trusted as sent**.
    ///
    /// ⚠️ Nothing here implements the 27-point rule, so a wrong declaration is
    /// recorded as a full win. A caller that cannot afford that has to check the
    /// rule before calling, and this crate is not where that check lives yet.
    pub fn declare(&mut self, winner: Color) {
        self.adjudicate(Outcome::Declaration { winner });
    }

    /// The ply cap was reached. A draw, and the cap is the caller's — this crate
    /// holds no maximum of its own.
    pub fn max_moves(&mut self) {
        self.adjudicate(Outcome::MaxMoves);
    }

    /// `loser` stopped playing — crashed, wedged, or said something no move
    /// could be read out of.
    pub fn abandon(&mut self, loser: Color) {
        self.adjudicate(Outcome::Abandoned { loser });
    }

    /// Record an ending decided off the board.
    ///
    /// The first ending wins. A game already decided cannot be re-decided —
    /// otherwise a flag fall arriving behind a mate would overwrite it, and
    /// which one a game ended by would depend on message order.
    fn adjudicate(&mut self, outcome: Outcome) {
        if self.outcome.is_none() {
            self.outcome = Some(outcome);
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

    type Slide = ((u8, u8), (u8, u8));

    /// One call to an adjudicator, so a test can drive all six from a list.
    type Adjudication = fn(&mut Game);

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

    /// Pins the direction that costs a game its real ending: a rule-decided
    /// result replaced by one a caller declared afterwards. The neighbouring
    /// resignation tests only ever put two declared endings against each other.
    #[test]
    fn an_ending_the_board_produced_survives_one_declared_after_it() {
        let mut game = game_from("sfen 4k4/9/9/9/9/9/9/9/4R3K b G 1");
        game.play(Move::Drop {
            piece: Piece::new(PieceKind::Gold, Color::Black),
            to: sq(5, 2),
        })
        .expect("the drop is legal");
        game.flag_fall(Color::Black);
        assert_eq!(
            game.outcome(),
            Some(Outcome::Checkmate {
                winner: Color::Black
            })
        );
    }

    /// Each adjudicator's own write, which nothing else observes: every other
    /// call site hands them a game that is already decided, so the guard
    /// discards the value before anyone can look at it. Sabotage: an empty
    /// `max_moves` body, `declare` building `Abandoned`, `foul` dropping its
    /// `kind`, and `flag_fall` flipping its `loser` each pass the rest of the
    /// suite and fail here. Every side is `White`, so a flip shows up as
    /// `Black`.
    #[test]
    fn each_adjudication_records_its_own_ending() {
        let cases: [(Adjudication, Outcome); 6] = [
            (
                |g| g.resign(Color::White),
                Outcome::Resignation {
                    loser: Color::White,
                },
            ),
            (
                |g| g.flag_fall(Color::White),
                Outcome::FlagFall {
                    loser: Color::White,
                },
            ),
            (
                |g| g.foul(Color::White, IllegalMoveKind::TwoPawns),
                Outcome::IllegalMove {
                    loser: Color::White,
                    kind: IllegalMoveKind::TwoPawns,
                },
            ),
            (
                |g| g.declare(Color::White),
                Outcome::Declaration {
                    winner: Color::White,
                },
            ),
            (Game::max_moves, Outcome::MaxMoves),
            (
                |g| g.abandon(Color::White),
                Outcome::Abandoned {
                    loser: Color::White,
                },
            ),
        ];
        for (adjudicate, expected) in cases {
            let mut game = Game::startpos();
            adjudicate(&mut game);
            assert_eq!(game.outcome(), Some(expected));
        }
    }

    /// Catches any one of them assigning `outcome` directly instead of going
    /// through `adjudicate`: the guard lives in one place, so a test covering
    /// one method proves nothing about the others.
    #[test]
    fn every_adjudication_leaves_a_decided_game_alone() {
        let declared: [Adjudication; 5] = [
            |g| g.flag_fall(Color::White),
            |g| g.foul(Color::White, IllegalMoveKind::TwoPawns),
            |g| g.declare(Color::White),
            Game::max_moves,
            |g| g.abandon(Color::White),
        ];
        for adjudicate in declared {
            let mut game = Game::startpos();
            game.resign(Color::Black);
            adjudicate(&mut game);
            assert_eq!(
                game.outcome(),
                Some(Outcome::Resignation {
                    loser: Color::Black
                })
            );
        }
    }

    /// An adjudicated ending is not attached to a ply, so nothing about
    /// popping one obviously clears it. A caller that told its
    /// players the game was over is what makes the revival worth pinning.
    #[test]
    fn undoing_past_an_adjudicated_ending_revives_the_game() {
        let mut game = Game::startpos();
        game.play(normal((7, 7), (7, 6))).expect("legal");
        game.flag_fall(Color::White);
        assert!(game.outcome().is_some());
        assert!(game.undo().is_some());
        assert_eq!(game.outcome(), None);
        // The same move again: the position came back with the outcome.
        assert!(game.play(normal((7, 7), (7, 6))).is_ok());
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

    /// The same position as `a_perpetual_check_loses_for_the_checking_side`,
    /// with the rook going all the way home rather than on to the next
    /// checking square. One quiet move inside the window is the whole
    /// difference between losing and a draw.
    ///
    /// Sabotage: in `classify_repetition`, seed `all_checks` with
    /// `[false, false]` **and** accumulate with `|=` — both together, which is
    /// what turns the rule into "checked at least once" — and this is the only
    /// test in the workspace that goes red. ⚠️ Either half alone makes every
    /// window a draw instead, which the two perpetual-check tests catch and
    /// this one cannot.
    #[test]
    fn a_cycle_with_one_quiet_move_is_a_draw_rather_than_a_perpetual_check() {
        let mut game = game_from("sfen 4k4/9/9/9/9/9/9/9/K7R b - 1");
        let cycle = [
            ((1, 9), (1, 1)), // the rook checks along the first rank
            ((5, 1), (5, 2)), // the king steps off it
            ((1, 1), (1, 9)), // and the rook goes home, checking nothing
            ((5, 2), (5, 1)),
        ];
        for _ in 0..3 {
            for (from, to) in cycle {
                game.play(normal(from, to)).expect("cycle move is legal");
            }
        }
        // The premise stated exactly rather than assumed: which moves checked
        // is what the outcome below is derived from.
        let checks: Vec<bool> = game.moves().iter().map(|ply| ply.gave_check).collect();
        assert_eq!(
            checks,
            [
                true, false, false, false, true, false, false, false, true, false, false, false
            ]
        );
        assert_eq!(game.outcome(), Some(Outcome::Repetition));
    }

    /// `(true, true)` shares the `_` arm with "nobody checked", so a real game
    /// cannot tell the two apart — but the arm above it can be widened to
    /// `(true, _)` and every other test here stays green, so the arm still
    /// needs pinning. The window and the positions are a real 千日手; only the
    /// twelve flags are set by hand, no legal cycle producing that many
    /// consecutive cross-checks.
    ///
    /// Sabotage: widen `(true, false)` to `(true, _)` and this reports a
    /// perpetual-check loss for a cycle both sides checked throughout.
    #[test]
    fn a_window_in_which_both_sides_checked_throughout_is_still_a_draw() {
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

        for ply in &mut game.moves {
            ply.gave_check = true;
        }
        assert_eq!(game.classify_repetition(), Outcome::Repetition);
    }

    /// The mirror of `a_perpetual_check_loses_for_the_checking_side`, and the
    /// only test in this crate that names White as the loser.
    ///
    /// Sabotage: replace the side lookup in `classify_repetition` with `i % 2`
    /// and this fails, because the root has White to move and the parities no
    /// longer line up.
    #[test]
    fn a_perpetual_check_by_white_names_white_as_the_loser() {
        // Black king alone on 5i, White king tucked away on 1a, White rook 9a.
        let mut game = game_from("sfen r7k/9/9/9/9/9/9/9/4K4 w - 1");
        game.play(normal((9, 1), (9, 9)))
            .expect("rook to 9i checks");
        let cycle = [
            ((5, 9), (5, 8)), // Black king steps off the checked rank
            ((9, 9), (9, 8)), // White checks again
            ((5, 8), (5, 9)),
            ((9, 8), (9, 9)),
        ];
        for _ in 0..3 {
            for (from, to) in cycle {
                game.play(normal(from, to)).expect("cycle move is legal");
            }
        }
        assert_eq!(
            game.outcome(),
            Some(Outcome::PerpetualCheck {
                loser: Color::White
            }),
            "White gave check on every move of the cycle"
        );
        // The window opens at the first occurrence of the repeated position,
        // which here is the position after White's opening check — not the
        // root. Sabotage: return `Some(0)` from `first_occurrence` and this
        // assertion fails while every other repetition test stays green.
        assert_eq!(game.repetition.first_occurrence(), Some(1));
    }

    /// Three returns to the start position by different squares and — the part
    /// that matters — different numbers of moves. The index counts occurrences
    /// of a position, so a cycle of six has to count the same as a cycle of
    /// four.
    #[test]
    fn the_fourth_occurrence_counts_however_the_position_was_reached() {
        // Rank h is empty either side of the rook as far as the bishop on 8h,
        // and rank b likewise for White, so every one of these is a clear
        // slide. Two of them cross the file both kings stand on; what keeps
        // those quiet is the mover's own pawn wall on the rank ahead, not the
        // choice of squares.
        let short_way: &[Slide] = &[
            ((2, 8), (1, 8)),
            ((8, 2), (9, 2)),
            ((1, 8), (2, 8)),
            ((9, 2), (8, 2)),
        ];
        let long_way: &[Slide] = &[
            ((2, 8), (3, 8)),
            ((8, 2), (7, 2)),
            ((3, 8), (4, 8)),
            ((7, 2), (6, 2)),
            ((4, 8), (2, 8)),
            ((6, 2), (8, 2)),
        ];
        let middle_way: &[Slide] = &[
            ((2, 8), (5, 8)),
            ((8, 2), (5, 2)),
            ((5, 8), (2, 8)),
            ((5, 2), (8, 2)),
        ];

        let mut game = Game::startpos();
        for (from, to) in short_way.iter().chain(long_way) {
            game.play(normal(*from, *to)).expect("a clear rook slide");
        }
        assert_eq!(game.outcome(), None, "three occurrences is not yet 千日手");

        for (from, to) in middle_way {
            game.play(normal(*from, *to)).expect("a clear rook slide");
        }
        assert_eq!(game.ply(), 14);
        assert!(
            game.moves().iter().all(|ply| !ply.gave_check),
            "no rook leaves its own rank"
        );
        assert_eq!(game.outcome(), Some(Outcome::Repetition));
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

    /// Sabotage: return `positions.last()` from `initial_position` and the
    /// `the root moved` assertion fires.
    #[test]
    fn the_initial_position_stays_put_while_the_game_advances() {
        let mut game = Game::startpos();
        let root = game.initial_position().clone();
        assert_eq!(game.position(), &root, "nothing has been played yet");

        game.play(normal((7, 7), (7, 6))).expect("legal");
        game.play(normal((3, 3), (3, 4))).expect("legal");
        assert_eq!(game.initial_position(), &root, "the root moved");
        assert_ne!(game.position(), &root);

        game.undo();
        assert_eq!(game.initial_position(), &root, "undo moved the root");
    }

    /// The root's own move number is written, not the game's ply count.
    ///
    /// Sabotage: take the root from `self.position()` in `to_usi_position` and
    /// the `startpos moves 7g7f 3c3d` case reports the position after both
    /// moves, with both replayed onto it.
    #[test]
    fn a_game_says_which_position_argument_would_rebuild_it() {
        assert_eq!(Game::startpos().to_usi_position(), "startpos");

        let game = Game::from_usi_position("startpos moves 7g7f 3c3d").expect("valid");
        assert_eq!(game.to_usi_position(), "startpos moves 7g7f 3c3d");

        let game = Game::from_usi_position("sfen 4k4/9/9/9/9/9/9/9/K7R b - 7 moves 1i1a")
            .expect("the rook slides up a clear file");
        assert_eq!(
            game.to_usi_position(),
            "sfen 4k4/9/9/9/9/9/9/9/K7R b - 7 moves 1i1a"
        );

        // The move number is optional on the way in; the `sfen` form always
        // carries one on the way out.
        let game = Game::from_usi_position("sfen 4k4/9/9/9/9/9/9/9/K7R b -").expect("valid");
        assert_eq!(game.to_usi_position(), "sfen 4k4/9/9/9/9/9/9/9/K7R b - 1");
    }

    /// The spelling follows the position rather than the text the game was
    /// built from.
    ///
    /// Sabotage: force the `sfen` arm in `to_usi_position` and the first case
    /// comes back as the `sfen` form.
    /// `the_position_argument_a_game_writes_rebuilds_that_game` stayed green
    /// under that run — a round trip cannot see a respelling — which is why the
    /// spelling needs a test that reads the text.
    #[test]
    fn a_root_that_is_the_initial_position_is_written_as_startpos() {
        const BOARD: &str = "lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b -";

        let game = Game::from_usi_position(&format!("sfen {BOARD} 1 moves 7g7f")).expect("valid");
        assert_eq!(game.to_usi_position(), "startpos moves 7g7f");

        // The same board at another move number is a different root, and keeps
        // the spelling that can carry one.
        let game = Game::from_usi_position(&format!("sfen {BOARD} 5")).expect("valid");
        assert_eq!(game.to_usi_position(), format!("sfen {BOARD} 5"));
    }

    /// The writer and the parser are held to agree: what `to_usi_position`
    /// writes, `from_usi_position` reads back as the same game.
    ///
    /// It compares games rather than text, so a respelt root passes here and is
    /// pinned literally by the two tests above instead.
    ///
    /// Sabotage: drop the `sfen ` prefix in `to_usi_position` and the loop's
    /// first `sfen`-rooted case does not parse back at all —
    /// ``invalid token: `sfen` was expected``.
    #[test]
    fn the_position_argument_a_game_writes_rebuilds_that_game() {
        for args in [
            "startpos",
            "startpos moves 7g7f 3c3d 8h2b+ 3a2b",
            "sfen 4k4/9/9/9/9/9/9/9/K7R b - 7 moves 1i1a",
            "sfen 4k4/9/9/9/4P4/9/9/9/4K4 b P 1",
            // Drops, whose USI token carries no colour: the writer's only move
            // shape no other case here covers.
            "sfen 4k4/9/9/9/9/9/9/9/4K4 b Pp 1 moves P*5e P*5d",
        ] {
            let game = Game::from_usi_position(args).expect("valid");
            let rebuilt =
                Game::from_usi_position(&game.to_usi_position()).expect("what it wrote, it reads");
            assert_eq!(
                rebuilt.initial_position(),
                game.initial_position(),
                "{args}"
            );
            assert_eq!(rebuilt.position(), game.position(), "{args}");
            assert_eq!(rebuilt.ply(), game.ply(), "{args}");
            assert_eq!(rebuilt.outcome(), game.outcome(), "{args}");
        }
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
