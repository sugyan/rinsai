//! One refereed game between two seats.
//!
//! The referee holds a [`rinsai_game::Game`], so every move an engine sends
//! is legality-checked on an implementation independent of either player,
//! and the rule-decided endings — 詰み, 千日手, 連続王手の千日手 — are
//! adjudicated by the referee rather than trusted to the engines. A mated
//! side loses without being asked for a move; a side whose move is illegal,
//! or whose engine times out, dies or talks nonsense, loses on the spot.

use rinsai_game::{Game, Outcome};
use shogi_core::{Color, ToUsi};

use crate::usi::{BestmoveAnswer, EngineError};

/// A game is at most this many plies from the opening's own root, the
/// opening's moves included; reaching the cap is a draw — floodgate's own
/// `Max_Moves:512` convention. ⚠️ For an opening rooted at a mid-game `sfen`
/// that is fewer plies of real game than the number says.
pub const MAX_GAME_PLIES: usize = 512;

/// One side's ability to answer a position. [`crate::usi::UsiEngine`] behind
/// a node budget is the real one; tests script them.
pub trait Seat {
    /// The answer to `position {position_args}`.
    fn bestmove(&mut self, position_args: &str) -> Result<BestmoveAnswer, EngineError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Winner {
    Black,
    White,
    /// A draw.
    Neither,
}

impl Winner {
    fn of(color: Color) -> Self {
        match color {
            Color::Black => Self::Black,
            Color::White => Self::White,
        }
    }

    fn opponent_of(color: Color) -> Self {
        Self::of(color.flip())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// 詰み — the rules library folds stalemate in, which is shogi's rule.
    Checkmate,
    Resign,
    /// `bestmove win`, trusted as sent — the referee has no 27-point rule to
    /// check it against until E2 builds one. ⚠️ An engine that declares
    /// wrongly is scored a full win, so this arm is worth revisiting the
    /// first time a run's log shows one.
    Declaration,
    IllegalMove,
    Timeout,
    Died,
    Protocol,
    /// 千日手.
    Sennichite,
    /// 連続王手の千日手 — the loser is the perpetual checker.
    PerpetualCheck,
    MaxMoves,
}

#[derive(Debug, Clone)]
pub struct GameRecord {
    pub winner: Winner,
    pub reason: EndReason,
    /// Moves played from the opening's own root, the opening's included.
    pub plies: usize,
    /// The whole game in USI move tokens, opening included.
    pub moves_usi: String,
    /// What the offender sent, on the reasons where that is the story.
    pub detail: Option<String>,
}

/// Play one game from a USI `position` argument (`startpos moves …`).
///
/// `max_plies` is the draw cap; the runner passes [`MAX_GAME_PLIES`], tests
/// pass something small. An opening the referee cannot replay is an error —
/// the runner validates every opening before any engine spawns, so reaching
/// it here means the opening file and the referee disagree, which must stop
/// the run, not score a game.
pub fn play_game(
    black: &mut dyn Seat,
    white: &mut dyn Seat,
    opening: &str,
    max_plies: usize,
) -> Result<GameRecord, String> {
    let mut game =
        Game::from_usi_position(opening).map_err(|e| format!("unplayable opening: {e}"))?;
    let mut args = opening.trim().to_owned();
    let mut had_moves = args.split_whitespace().any(|t| t == "moves");

    let (winner, reason, detail) = loop {
        if let Some(outcome) = game.outcome() {
            break match outcome {
                Outcome::Checkmate { winner } => (Winner::of(winner), EndReason::Checkmate, None),
                Outcome::Repetition => (Winner::Neither, EndReason::Sennichite, None),
                Outcome::PerpetualCheck { loser } => {
                    (Winner::opponent_of(loser), EndReason::PerpetualCheck, None)
                }
                Outcome::Resignation { loser } => {
                    (Winner::opponent_of(loser), EndReason::Resign, None)
                }
            };
        }
        if game.ply() >= max_plies {
            break (Winner::Neither, EndReason::MaxMoves, None);
        }

        let mover = game.side_to_move();
        let seat: &mut dyn Seat = match mover {
            Color::Black => &mut *black,
            Color::White => &mut *white,
        };
        match seat.bestmove(&args) {
            Ok(BestmoveAnswer::Move(token)) => match game.play_usi(&token) {
                Ok(_) => {
                    if had_moves {
                        args.push(' ');
                    } else {
                        args.push_str(" moves ");
                        had_moves = true;
                    }
                    args.push_str(&token);
                }
                Err(e) => {
                    break (
                        Winner::opponent_of(mover),
                        EndReason::IllegalMove,
                        Some(format!(
                            "`{token}` in sfen {}: {e}",
                            game.position().to_sfen_owned()
                        )),
                    );
                }
            },
            Ok(BestmoveAnswer::Resign) => {
                break (Winner::opponent_of(mover), EndReason::Resign, None);
            }
            Ok(BestmoveAnswer::Win) => {
                break (Winner::of(mover), EndReason::Declaration, None);
            }
            Err(e) => {
                let reason = match e {
                    EngineError::Timeout { .. } => EndReason::Timeout,
                    EngineError::Died { .. } | EngineError::Spawn(_) => EndReason::Died,
                    EngineError::Protocol(_) => EndReason::Protocol,
                };
                break (Winner::opponent_of(mover), reason, Some(e.to_string()));
            }
        }
    };

    let moves_usi = game
        .moves()
        .iter()
        .map(|ply| ply.mv.to_usi_owned())
        .collect::<Vec<_>>()
        .join(" ");
    Ok(GameRecord {
        winner,
        reason,
        plies: game.ply(),
        moves_usi,
        detail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers from a script; records every position argument it was asked
    /// about, which is the only place that string is observable.
    struct Scripted {
        answers: std::vec::IntoIter<Result<BestmoveAnswer, EngineError>>,
        asked: usize,
        seen: Vec<String>,
    }

    impl Scripted {
        fn moves(tokens: &[&str]) -> Self {
            Self {
                answers: tokens
                    .iter()
                    .map(|t| Ok(BestmoveAnswer::Move((*t).to_owned())))
                    .collect::<Vec<_>>()
                    .into_iter(),
                asked: 0,
                seen: Vec::new(),
            }
        }

        fn one(answer: Result<BestmoveAnswer, EngineError>) -> Self {
            Self {
                answers: vec![answer].into_iter(),
                asked: 0,
                seen: Vec::new(),
            }
        }
    }

    impl Seat for Scripted {
        fn bestmove(&mut self, args: &str) -> Result<BestmoveAnswer, EngineError> {
            self.asked += 1;
            self.seen.push(args.to_owned());
            self.answers.next().expect("the script covers the game")
        }
    }

    fn interleave(black: &[&str], white: &[&str]) -> (Scripted, Scripted) {
        (Scripted::moves(black), Scripted::moves(white))
    }

    #[test]
    fn an_illegal_move_loses_the_game_for_the_side_that_sent_it() {
        // White answers with Black's own opening move.
        let (mut black, mut white) = interleave(&["7g7f"], &["7g7f"]);
        let record = play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES).expect("plays");
        assert_eq!(record.winner, Winner::Black);
        assert_eq!(record.reason, EndReason::IllegalMove);
        assert_eq!(record.plies, 1);
        assert!(record.detail.as_deref().is_some_and(|d| d.contains("7g7f")));
    }

    #[test]
    fn a_resignation_is_a_loss_for_the_resigning_side() {
        let mut black = Scripted::one(Ok(BestmoveAnswer::Resign));
        let mut white = Scripted::moves(&[]);
        let record = play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES).expect("plays");
        assert_eq!(record.winner, Winner::White);
        assert_eq!(record.reason, EndReason::Resign);
        assert_eq!(white.asked, 0);
    }

    #[test]
    fn a_declared_win_is_scored_for_the_declaring_side() {
        let mut black = Scripted::one(Ok(BestmoveAnswer::Win));
        let mut white = Scripted::moves(&[]);
        let record = play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES).expect("plays");
        assert_eq!(record.winner, Winner::Black);
        assert_eq!(record.reason, EndReason::Declaration);
    }

    /// The referee sees the mate itself: the mated seat must never be asked
    /// for the move it does not have.
    #[test]
    fn a_mated_side_loses_without_being_asked() {
        let mut black = Scripted::moves(&["G*5b"]);
        let mut white = Scripted::moves(&[]);
        let record = play_game(
            &mut black,
            &mut white,
            "sfen 4k4/9/9/9/9/9/9/9/4R3K b G 1",
            MAX_GAME_PLIES,
        )
        .expect("plays");
        assert_eq!(record.winner, Winner::Black);
        assert_eq!(record.reason, EndReason::Checkmate);
        assert_eq!(white.asked, 0, "the mated side was asked for a move");
    }

    #[test]
    fn the_fourfold_repetition_is_adjudicated_a_draw() {
        let (mut black, mut white) = interleave(
            &["2h3h", "3h2h", "2h3h", "3h2h", "2h3h", "3h2h"],
            &["8b7b", "7b8b", "8b7b", "7b8b", "8b7b", "7b8b"],
        );
        let record = play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES).expect("plays");
        assert_eq!(record.winner, Winner::Neither);
        assert_eq!(record.reason, EndReason::Sennichite);
        assert_eq!(record.plies, 12, "the game ends at the fourth occurrence");
    }

    #[test]
    fn a_perpetual_checker_loses_by_the_verdict_not_by_material() {
        // Black is a whole rook up and checking forever; the verdict, not
        // the material, decides.
        let (mut black, mut white) = interleave(
            &["1i1a", "1a1b", "1b1a", "1a1b", "1b1a", "1a1b", "1b1a"],
            &["5a5b", "5b5a", "5a5b", "5b5a", "5a5b", "5b5a"],
        );
        let record = play_game(
            &mut black,
            &mut white,
            "sfen 4k4/9/9/9/9/9/9/9/K7R b - 1",
            MAX_GAME_PLIES,
        )
        .expect("plays");
        assert_eq!(record.winner, Winner::White);
        assert_eq!(record.reason, EndReason::PerpetualCheck);
    }

    /// The cap is a parameter so this does not need 512 scripted plies; the
    /// runner passes [`MAX_GAME_PLIES`].
    #[test]
    fn the_ply_cap_without_a_result_is_a_draw() {
        let (mut black, mut white) = interleave(&["2h3h", "3h2h"], &["8b7b", "7b8b"]);
        let record = play_game(&mut black, &mut white, "startpos", 4).expect("plays");
        assert_eq!(record.winner, Winner::Neither);
        assert_eq!(record.reason, EndReason::MaxMoves);
        assert_eq!(record.plies, 4);
    }

    /// An opening that already carries moves keeps them: the final record is
    /// the whole game from startpos, and the played moves append after them.
    #[test]
    fn an_opening_with_moves_is_extended_not_replaced() {
        let mut black = Scripted::one(Ok(BestmoveAnswer::Resign));
        let mut white = Scripted::moves(&[]);
        let record = play_game(
            &mut black,
            &mut white,
            "startpos moves 7g7f 3c3d",
            MAX_GAME_PLIES,
        )
        .expect("plays");
        assert_eq!(record.plies, 2);
        assert_eq!(record.moves_usi, "7g7f 3c3d");
    }

    /// The `position` argument is the only channel carrying game state to
    /// both players, and it is built by string append rather than by asking
    /// the board — so it is worth asserting literally, at every ply, from
    /// both a bare root and one that already carries moves.
    ///
    /// Sabotage: change either the `" moves "` separator or the `' '` join in
    /// `play_game` and this fails; nothing else in the workspace does,
    /// because every other test discards the argument.
    #[test]
    fn each_seat_is_asked_about_the_game_so_far_in_usi() {
        let (mut black, mut white) = interleave(&["7g7f", "2g2f"], &["3c3d", "8c8d"]);
        play_game(&mut black, &mut white, "startpos", 4).expect("plays");
        assert_eq!(
            black.seen,
            ["startpos", "startpos moves 7g7f 3c3d"],
            "Black is asked from the bare root, then after two plies"
        );
        assert_eq!(
            white.seen,
            ["startpos moves 7g7f", "startpos moves 7g7f 3c3d 2g2f"]
        );

        let (mut black, mut white) = interleave(&["2g2f"], &["8c8d"]);
        play_game(&mut black, &mut white, "startpos moves 7g7f 3c3d", 4).expect("plays");
        assert_eq!(black.seen, ["startpos moves 7g7f 3c3d"]);
        assert_eq!(white.seen, ["startpos moves 7g7f 3c3d 2g2f"]);
    }

    #[test]
    fn a_seat_error_maps_to_a_loss_with_the_matching_reason() {
        for (error, reason) in [
            (
                EngineError::Timeout {
                    waiting_for: "bestmove",
                },
                EndReason::Timeout,
            ),
            (
                EngineError::Died {
                    waiting_for: "bestmove",
                },
                EndReason::Died,
            ),
            (
                EngineError::Protocol("gibberish".to_owned()),
                EndReason::Protocol,
            ),
        ] {
            let mut black = Scripted::one(Err(error));
            let mut white = Scripted::moves(&[]);
            let record =
                play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES).expect("plays");
            assert_eq!(record.winner, Winner::White);
            assert_eq!(record.reason, reason);
            assert!(record.detail.is_some());
        }
    }
}
