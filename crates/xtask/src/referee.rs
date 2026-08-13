//! One refereed game between two seats.
//!
//! The referee holds a [`rinsai_game::Game`], so every move an engine sends
//! is legality-checked on an implementation independent of either player,
//! and the rule-decided endings — 詰み, 千日手, 連続王手の千日手 — are
//! adjudicated by the referee rather than trusted to the engines. A mated
//! side loses without being asked for a move; a side whose move is illegal,
//! or whose engine times out, dies or talks nonsense, loses on the spot.
//!
//! The clock is held here for the same reason: running out of time is a
//! result of the game, so it is adjudicated rather than asked about.

use std::time::Duration;

use rinsai_game::{Game, Outcome};
use shogi_core::{Color, ToUsi};

use crate::usi::{BestmoveAnswer, ClockSpec, EngineError, GoSpec, TimedAnswer, UsiEngine};

/// A game is at most this many plies from the opening's own root, the
/// opening's moves included; reaching the cap is a draw — floodgate's own
/// `Max_Moves:512` convention. ⚠️ For an opening rooted at a mid-game `sfen`
/// that is fewer plies of real game than the number says.
pub const MAX_GAME_PLIES: usize = 512;

/// Main time that drains, then a byoyomi period granted afresh every move.
///
/// ⚠️ Both sides start on the full main time however many moves the opening
/// already played: a harness game begins at the opening's root, so the plies
/// before it cost nobody anything.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimeControl {
    pub main: Duration,
    pub byoyomi: Duration,
}

/// What each side is given, for a whole game.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchControl {
    /// A node budget per side, bounded by a hang detector. Deterministic: no
    /// clock reading can change a move.
    Nodes {
        black: u64,
        white: u64,
        hang_timeout: Duration,
    },
    /// One clock, shared settings, tracked per side.
    Clock(TimeControl),
}

/// One side's ability to answer a position. [`UsiEngine`] is the real one;
/// tests script them.
pub trait Seat {
    /// The answer to `position {position_args}` under `spec`, with the time
    /// it took.
    fn bestmove(&mut self, position_args: &str, spec: GoSpec) -> TimedAnswer;
}

impl Seat for UsiEngine {
    fn bestmove(&mut self, position_args: &str, spec: GoSpec) -> TimedAnswer {
        self.go(position_args, spec)
    }
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
    /// The engine said nothing before the harness's hang detector fired — a
    /// wedged process rather than a lost game. ⚠️ Not [`Self::FlagFall`]: this
    /// one says the harness gave up, not that the clock ran out.
    Timeout,
    Died,
    Protocol,
    /// 時間切れ — the mover did not answer inside its own remaining time plus
    /// the byoyomi. A result of the game like any other, and scored as an
    /// ordinary loss.
    FlagFall,
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
    /// Wall time charged to each side over the whole game, Black's first.
    /// Filled under a node budget too, where it is a cost rather than a rule.
    pub spent: [Duration; Color::NUM],
    /// Plies the opening already carried. [`Self::move_times`] starts after
    /// them, so this is what says whose move its first entry was.
    pub opening_plies: usize,
    /// What each move cost, in the order they were asked for.
    ///
    /// ⚠️ **Not an index into [`Self::moves_usi`]**, and the two differ at
    /// both ends. It starts [`Self::opening_plies`] later, because the
    /// opening's moves cost nobody anything. And it ends one *longer* on every
    /// ending the referee reaches by asking — resignation, declaration, an
    /// illegal move, a flag fall, or any engine failure — each of which times
    /// an ask whose move was never played and so is not in the string.
    pub move_times: Vec<Duration>,
}

/// Both sides' clocks for one game.
struct Clock {
    control: MatchControl,
    main_left: [Duration; Color::NUM],
    spent: [Duration; Color::NUM],
}

impl Clock {
    fn new(control: MatchControl) -> Self {
        let main = match control {
            MatchControl::Nodes { .. } => Duration::ZERO,
            MatchControl::Clock(tc) => tc.main,
        };
        Self {
            control,
            main_left: [main; Color::NUM],
            spent: [Duration::ZERO; Color::NUM],
        }
    }

    /// The `go` the mover gets this move.
    fn spec(&self, mover: Color) -> GoSpec {
        match self.control {
            MatchControl::Nodes {
                black,
                white,
                hang_timeout,
            } => GoSpec::Nodes {
                nodes: match mover {
                    Color::Black => black,
                    Color::White => white,
                },
                hang_timeout,
            },
            MatchControl::Clock(tc) => GoSpec::Clock(ClockSpec {
                mover,
                btime: self.main_left[Color::Black.array_index()],
                wtime: self.main_left[Color::White.array_index()],
                byoyomi: tc.byoyomi,
            }),
        }
    }

    /// Charge one move to `mover`.
    ///
    /// The byoyomi is spent first and does not deplete — every move gets the
    /// whole period — so only the excess over it comes off the main time,
    /// which is never restored. `spent` accumulates under either control,
    /// where it is a cost rather than a rule.
    ///
    /// ⚠️ Bookkeeping only. Whether the mover ran out is decided by whether it
    /// answered at all, not by comparing this total against the allowance —
    /// see [`play_game`]. Nothing here can flag.
    fn charge(&mut self, mover: Color, elapsed: Duration) {
        let side = mover.array_index();
        self.spent[side] = self.spent[side].saturating_add(elapsed);
        let MatchControl::Clock(tc) = self.control else {
            return;
        };
        // Cannot underflow: `elapsed` never exceeds the allowance the seat's
        // wait was cut at, and that allowance is this side's main time plus
        // the byoyomi.
        self.main_left[side] -= elapsed.saturating_sub(tc.byoyomi);
    }
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
    control: MatchControl,
) -> Result<GameRecord, String> {
    let mut game =
        Game::from_usi_position(opening).map_err(|e| format!("unplayable opening: {e}"))?;
    let mut args = opening.trim().to_owned();
    let mut had_moves = args.split_whitespace().any(|t| t == "moves");
    let opening_plies = game.ply();
    let mut clock = Clock::new(control);
    let mut move_times: Vec<Duration> = Vec::new();

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
        let spec = clock.spec(mover);
        let TimedAnswer { answer, elapsed } = seat.bestmove(&args, spec);
        move_times.push(elapsed);
        clock.charge(mover, elapsed);
        match answer {
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
            // ⚠️ Only silence becomes a flag fall, and only under a clock.
            // An engine that died or talked nonsense **failed**, however long
            // it took to do it — routing those here on the strength of the
            // clock would file a crash as an ordinary loss and, because a flag
            // fall is not abnormal, drop its post-mortem with it.
            Err(EngineError::Timeout { .. }) if matches!(spec, GoSpec::Clock(_)) => {
                let allowance = spec.wait();
                // ⚠️ Three decimals, not whole milliseconds: an engine whose
                // last move overran by a fraction of one is the ordinary
                // case, and rounded it would read "took 200 ms of the 200 ms
                // it had", which states no reason for the loss it explains.
                let ms = |d: Duration| d.as_secs_f64() * 1_000.0;
                break (
                    Winner::opponent_of(mover),
                    EndReason::FlagFall,
                    Some(format!(
                        "answered nothing in the {:.3} ms it had left",
                        ms(allowance)
                    )),
                );
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
        opening_plies,
        spent: clock.spent,
        move_times,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers from a script; records every position argument it was asked
    /// about and every [`GoSpec`] it was given, which are the only places
    /// those are observable. The time each move costs is dictated rather than
    /// measured, so no test here reads a wall clock.
    struct Scripted {
        answers: std::vec::IntoIter<Result<BestmoveAnswer, EngineError>>,
        elapsed: std::vec::IntoIter<Duration>,
        asked: usize,
        seen: Vec<String>,
        specs: Vec<GoSpec>,
    }

    impl Scripted {
        fn moves(tokens: &[&str]) -> Self {
            Self::new(
                tokens
                    .iter()
                    .map(|t| Ok(BestmoveAnswer::Move((*t).to_owned())))
                    .collect(),
            )
        }

        fn one(answer: Result<BestmoveAnswer, EngineError>) -> Self {
            Self::new(vec![answer])
        }

        fn new(answers: Vec<Result<BestmoveAnswer, EngineError>>) -> Self {
            Self {
                answers: answers.into_iter(),
                elapsed: Vec::new().into_iter(),
                asked: 0,
                seen: Vec::new(),
                specs: Vec::new(),
            }
        }

        /// The same seat, taking these times in order; once the list runs out
        /// every later move is free.
        fn taking(mut self, ms: &[u64]) -> Self {
            self.elapsed = ms
                .iter()
                .map(|n| Duration::from_millis(*n))
                .collect::<Vec<_>>()
                .into_iter();
            self
        }

        /// The clocks this seat was told, as `(btime_ms, wtime_ms)`.
        fn clocks_seen(&self) -> Vec<(u128, u128)> {
            self.specs
                .iter()
                .map(|spec| match spec {
                    GoSpec::Clock(c) => (c.btime.as_millis(), c.wtime.as_millis()),
                    GoSpec::Nodes { .. } => panic!("a node budget carries no clock"),
                })
                .collect()
        }
    }

    impl Seat for Scripted {
        fn bestmove(&mut self, args: &str, spec: GoSpec) -> TimedAnswer {
            self.asked += 1;
            self.seen.push(args.to_owned());
            self.specs.push(spec);
            TimedAnswer {
                answer: self.answers.next().expect("the script covers the game"),
                elapsed: self.elapsed.next().unwrap_or(Duration::ZERO),
            }
        }
    }

    fn interleave(black: &[&str], white: &[&str]) -> (Scripted, Scripted) {
        (Scripted::moves(black), Scripted::moves(white))
    }

    /// The control the endings tests run under: they are about verdicts, not
    /// budgets, and a node budget is the one that cannot flag.
    fn nodes() -> MatchControl {
        MatchControl::Nodes {
            black: 1_000,
            white: 1_000,
            hang_timeout: Duration::from_secs(30),
        }
    }

    fn clock(main_ms: u64, byoyomi_ms: u64) -> MatchControl {
        MatchControl::Clock(TimeControl {
            main: Duration::from_millis(main_ms),
            byoyomi: Duration::from_millis(byoyomi_ms),
        })
    }

    #[test]
    fn an_illegal_move_loses_the_game_for_the_side_that_sent_it() {
        // White answers with Black's own opening move.
        let (mut black, mut white) = interleave(&["7g7f"], &["7g7f"]);
        let record =
            play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES, nodes()).expect("plays");
        assert_eq!(record.winner, Winner::Black);
        assert_eq!(record.reason, EndReason::IllegalMove);
        assert_eq!(record.plies, 1);
        assert!(record.detail.as_deref().is_some_and(|d| d.contains("7g7f")));
    }

    #[test]
    fn a_resignation_is_a_loss_for_the_resigning_side() {
        let mut black = Scripted::one(Ok(BestmoveAnswer::Resign));
        let mut white = Scripted::moves(&[]);
        let record =
            play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES, nodes()).expect("plays");
        assert_eq!(record.winner, Winner::White);
        assert_eq!(record.reason, EndReason::Resign);
        assert_eq!(white.asked, 0);
    }

    #[test]
    fn a_declared_win_is_scored_for_the_declaring_side() {
        let mut black = Scripted::one(Ok(BestmoveAnswer::Win));
        let mut white = Scripted::moves(&[]);
        let record =
            play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES, nodes()).expect("plays");
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
            nodes(),
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
        let record =
            play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES, nodes()).expect("plays");
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
            nodes(),
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
        let record = play_game(&mut black, &mut white, "startpos", 4, nodes()).expect("plays");
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
            nodes(),
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
        play_game(&mut black, &mut white, "startpos", 4, nodes()).expect("plays");
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
        play_game(
            &mut black,
            &mut white,
            "startpos moves 7g7f 3c3d",
            4,
            nodes(),
        )
        .expect("plays");
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
            let record = play_game(&mut black, &mut white, "startpos", MAX_GAME_PLIES, nodes())
                .expect("plays");
            assert_eq!(record.winner, Winner::White);
            assert_eq!(record.reason, reason);
            assert!(record.detail.is_some());
        }
    }

    // ------------------------------------------------------------- the clock

    /// The byoyomi is granted afresh every move, so a move inside it costs no
    /// main time at all, and only the excess over it does. Both halves of the
    /// rule, over a sequence, because the failure is arithmetic rather than
    /// categorical: a wrong rule still produces a plausible clock.
    ///
    /// Sabotage: making the byoyomi a depleting pot — `main_left[side] -=
    /// elapsed` — failed this test on its first row, leaving 9.6s where 10s
    /// was due, and three more besides
    /// (`a_seat_that_answers_keeps_its_move_however_long_it_took`,
    /// `a_seat_that_never_answers_under_a_clock_flags_rather_than_hangs`,
    /// `an_engine_that_failed_keeps_its_own_ending_under_a_clock`), which
    /// panicked: subtracting a whole `elapsed` the allowance permitted
    /// underflows the `Duration`, where subtracting only the excess cannot.
    #[test]
    fn the_byoyomi_costs_no_main_time_and_only_the_excess_costs_any() {
        let mut clock = Clock::new(clock(10_000, 1_000));
        for (spend, black_left) in [(400, 10_000), (1_000, 10_000), (1_800, 9_200), (0, 9_200)] {
            clock.charge(Color::Black, Duration::from_millis(spend));
            assert_eq!(
                clock.main_left[Color::Black.array_index()],
                Duration::from_millis(black_left),
                "after spending {spend} ms"
            );
        }
        // An unused period does not accumulate: four moves under the byoyomi
        // bought nothing, so the allowance is still the main time plus one
        // period rather than five.
        let GoSpec::Clock(spec) = clock.spec(Color::Black) else {
            panic!("a clock control yields a clock spec")
        };
        assert_eq!(spec.allowance(), Duration::from_millis(9_200 + 1_000));
    }

    /// Each side has its own clock. A single shared one is invisible for as
    /// long as the two sides spend alike, which is most of a real game.
    ///
    /// Sabotage: indexing by `Color::Black` instead of by the mover in
    /// `Clock::charge` failed this test,
    /// `each_seat_is_told_both_clocks_as_the_referee_holds_them` — where the
    /// merged clock reaches the engines — and
    /// `a_seat_that_answers_keeps_its_move_however_long_it_took`.
    #[test]
    fn one_side_spending_does_not_move_the_other_sides_clock() {
        let mut clock = Clock::new(clock(10_000, 0));
        clock.charge(Color::Black, Duration::from_millis(300));
        clock.charge(Color::White, Duration::from_millis(500));
        assert_eq!(
            clock.main_left[Color::Black.array_index()],
            Duration::from_millis(9_700)
        );
        assert_eq!(
            clock.main_left[Color::White.array_index()],
            Duration::from_millis(9_500)
        );
    }

    /// An answer that arrived is played, however much of the allowance it
    /// took. Only silence flags \u{2014} so a seat that spends every millisecond it
    /// has and still answers keeps its move.
    ///
    /// ⚠️ This is the boundary the arithmetic version of this test got wrong:
    /// judging on `elapsed >= allowance` discarded a legal move whenever the
    /// harness's own parse pushed the total over, which a review reproduced in
    /// 1243 of 2000 trials at 300 µs of margin.
    #[test]
    fn a_seat_that_answers_keeps_its_move_however_long_it_took() {
        let mut black = Scripted::moves(&["7g7f"]).taking(&[11_000]);
        let mut white = Scripted::moves(&["3c3d"]).taking(&[11_000]);
        let record =
            play_game(&mut black, &mut white, "startpos", 2, clock(10_000, 1_000)).expect("plays");
        assert_eq!(record.reason, EndReason::MaxMoves);
        assert_eq!(record.plies, 2, "both answers were played");
        assert_eq!(record.moves_usi, "7g7f 3c3d");
    }

    /// A node budget has no clock to run out of, whatever the moves cost:
    /// `spent` still accumulates, and the main time is untouched.
    #[test]
    fn a_fixed_node_game_is_never_on_the_clock() {
        let mut clock = Clock::new(nodes());
        clock.charge(Color::Black, Duration::from_secs(3_600));
        assert_eq!(
            clock.spent[Color::Black.array_index()],
            Duration::from_secs(3_600)
        );
        assert_eq!(
            clock.main_left[Color::Black.array_index()],
            Duration::ZERO,
            "a node budget seeds no main time to spend"
        );
    }

    /// The clock reaches the engine only through the `go` line, and it is
    /// built from state the referee mutates every ply — so this asserts the
    /// whole sequence, at every ply, the way
    /// `each_seat_is_asked_about_the_game_so_far_in_usi` does for `position`.
    ///
    /// Sabotage: sending the *mover's* remaining time as `btime` regardless of
    /// colour (`btime: self.main_left[mover.array_index()]`) in `Clock::spec`
    /// failed this test, with White's specs reading
    /// `[(10000, 10000), (9700, 9700)]` — both entries collapsed onto the
    /// mover's own clock.
    #[test]
    fn each_seat_is_told_both_clocks_as_the_referee_holds_them() {
        let (black, white) = interleave(&["7g7f", "2g2f"], &["3c3d", "8c8d"]);
        let mut black = black.taking(&[500, 700]);
        let mut white = white.taking(&[300, 200]);
        play_game(&mut black, &mut white, "startpos", 4, clock(10_000, 0)).expect("plays");
        // Black spends 500 then 700; White 300 then 200. Each side is told
        // both remaining times, and only the mover's has moved since it last
        // saw them.
        assert_eq!(black.clocks_seen(), [(10_000, 10_000), (9_500, 9_700)]);
        assert_eq!(white.clocks_seen(), [(9_500, 10_000), (8_800, 9_700)]);
    }

    /// A legal move that took too long is not a move. The charge decides
    /// before the answer is read, so the board must not advance and the point
    /// must go to the opponent.
    ///
    /// An engine that **failed** keeps its own ending under a clock. Only
    /// silence is a flag fall, so a crash or a nonsense line is still reported
    /// as the failure it is.
    ///
    /// ⚠️ This is what stops a clocked run from laundering a wedged engine
    /// into an ordinary loss: a flag fall is not abnormal, so a crash filed as
    /// one loses both the ⚠️ tally and the stderr post-mortem, and reads in
    /// the log exactly like an engine that merely thought too long.
    ///
    /// Sabotage: route every `Err` to `EndReason::FlagFall` \u{2014} the shape this
    /// replaced \u{2014} and both rows here fail.
    #[test]
    fn an_engine_that_failed_keeps_its_own_ending_under_a_clock() {
        for (error, reason) in [
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
            let mut black = Scripted::one(Err(error)).taking(&[1_000]);
            let mut white = Scripted::moves(&[]);
            let record =
                play_game(&mut black, &mut white, "startpos", 4, clock(0, 1_000)).expect("plays");
            assert_eq!(record.winner, Winner::White);
            assert_eq!(record.reason, reason, "under a clock");
        }
    }

    /// A seat that never answers under a clock has lost on time, not hung:
    /// the wait it blew through was its own allowance. ⚠️ This is the hole
    /// the issue exists to close — the same silence under a node budget is
    /// `Timeout`, and `a_seat_error_maps_to_a_loss_with_the_matching_reason`
    /// pins that half.
    #[test]
    fn a_seat_that_never_answers_under_a_clock_flags_rather_than_hangs() {
        let mut black = Scripted::one(Err(EngineError::Timeout {
            waiting_for: "bestmove",
        }))
        .taking(&[1_000]);
        let mut white = Scripted::moves(&[]);
        let record =
            play_game(&mut black, &mut white, "startpos", 4, clock(0, 1_000)).expect("plays");
        assert_eq!(record.winner, Winner::White);
        assert_eq!(record.reason, EndReason::FlagFall);
    }
}
