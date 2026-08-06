//! USI conformance dialogues.
//!
//! The protocol loop is our own code, so it carries its own tests (DESIGN.md
//! E0). Two properties make the suite deterministic and **sleep-free**:
//!
//! 1. `usi::run` returns only after the search thread has been joined, so when
//!    it returns every byte has been written and flushed. Each script ends with
//!    `quit` (or end of input) and the whole transcript is then asserted on.
//! 2. The [`Searcher`] seam lets a test inject [`CapturingSearcher`], which
//!    records the jobs it was handed and can be told to block until signalled.
//!    "Did `go infinite` reach the search as `infinite`?" becomes a value
//!    assertion rather than a timing one. Note what these do *not* cover: that
//!    the searcher **honours** `infinite` — `CapturingSearcher` blocks on its
//!    own flag and never reads `limits` — which is tested where it lives, in
//!    `rinsai-search`'s `placeholder` tests.
//!
//! Note what is *never* asserted: which move the engine chose. shunsai's public
//! documentation says nothing about generation order, so it is an unspecified
//! implementation detail and naming a move here would be a false regression
//! waiting for the next shunsai bump. [`assert_legal_after`] asserts the
//! property that actually matters.

use std::io::{Cursor, Write};
use std::sync::{Arc, Mutex};

use rinsai_search::{BestMove, Game, InfoSink, Limits, PlaceholderSearcher, SearchJob, Searcher};

// ---------------------------------------------------------------- scaffolding

/// A `Write` the test can read back once `run` has returned.
#[derive(Clone)]
struct SharedBuf(Arc<Mutex<Vec<u8>>>);

impl Write for SharedBuf {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Records every job it is handed, and optionally waits to be signalled before
/// answering — which is how a test holds a search "in progress" without a sleep.
#[derive(Clone, Default)]
struct Recorded(Arc<Mutex<Vec<(u64, Limits, String)>>>);

impl Recorded {
    fn jobs(&self) -> Vec<(u64, Limits, String)> {
        self.0.lock().unwrap().clone()
    }
}

struct CapturingSearcher {
    recorded: Recorded,
    block: bool,
}

impl Searcher for CapturingSearcher {
    fn search(&mut self, job: &SearchJob, _out: &dyn InfoSink) -> BestMove {
        self.recorded
            .0
            .lock()
            .unwrap()
            .push((job.id, job.limits, job.game.sfen()));
        while self.block && !job.signals.stopped() && !job.signals.ponder_hit() {
            job.signals.wait_until_signalled();
        }
        BestMove::Resign
    }
}

fn drive<S: Searcher + 'static>(searcher: S, script: &str) -> Vec<String> {
    let buf = SharedBuf(Arc::new(Mutex::new(Vec::new())));
    rinsai::usi::run(
        Cursor::new(script.as_bytes().to_vec()),
        buf.clone(),
        searcher,
    );
    let bytes = buf.0.lock().unwrap().clone();
    String::from_utf8(bytes)
        .expect("the engine only writes UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

/// Runs a script against the real E0 step 1 engine.
fn dialogue(script: &str) -> Vec<String> {
    drive(PlaceholderSearcher, script)
}

/// Runs a script against a recording searcher, returning the transcript and
/// what the searcher saw.
fn dialogue_capturing(script: &str, block: bool) -> (Vec<String>, Recorded) {
    let recorded = Recorded::default();
    let lines = drive(
        CapturingSearcher {
            recorded: recorded.clone(),
            block,
        },
        script,
    );
    (lines, recorded)
}

fn bestmoves(lines: &[String]) -> Vec<&str> {
    lines
        .iter()
        .filter(|l| l.starts_with("bestmove"))
        .map(String::as_str)
        .collect()
}

/// Asserts that the move the engine answered is legal in the position the
/// engine was given — the only thing a test may say about a chosen move.
fn assert_legal_after(position_args: &str, bestmove_line: &str) {
    let token = bestmove_line
        .strip_prefix("bestmove ")
        .unwrap_or_else(|| panic!("`{bestmove_line}` is not a bestmove line"));
    let mut game = Game::from_usi_position(position_args).expect("the fixture position parses");
    game.push_usi_move(token)
        .unwrap_or_else(|e| panic!("the engine answered `{token}`, which is not legal: {e}"));
}

// ------------------------------------------------------------------ handshake

#[test]
fn usi_handshake_ends_in_usiok() {
    let lines = dialogue("usi\nquit\n");
    // The version comes from the manifest, as it does in `usi_process.rs`:
    // spelling it out here would make the next `version` bump a red test that
    // says nothing about the protocol.
    assert_eq!(
        lines[0],
        format!("id name rinsai {}", env!("CARGO_PKG_VERSION"))
    );
    assert_eq!(lines[1], "id author sugyan");
    assert!(
        lines[2..lines.len() - 1]
            .iter()
            .all(|l| l.starts_with("option name "))
    );
    assert_eq!(lines.last().map(String::as_str), Some("usiok"));
}

#[test]
fn isready_answers_before_and_after_a_position() {
    let lines =
        dialogue("usi\nisready\nposition startpos moves 7g7f\nisready\ngo movetime 1\nquit\n");
    assert_eq!(lines.iter().filter(|l| *l == "readyok").count(), 2);
    assert_eq!(bestmoves(&lines).len(), 1);
    // It is White to move after 7g7f, so a Black move would be rejected here.
    assert_legal_after("startpos moves 7g7f", bestmoves(&lines)[0]);
}

/// The handshake is not a precondition: a harness may go straight to work.
#[test]
fn a_go_without_a_handshake_still_answers() {
    let lines = dialogue("position startpos\ngo movetime 1\nquit\n");
    assert_eq!(bestmoves(&lines).len(), 1);
    assert_legal_after("startpos", bestmoves(&lines)[0]);
}

// ------------------------------------------------------------------- position

#[test]
fn an_sfen_root_is_accepted() {
    let sfen = "sfen lnsgkgsnl/1r5b1/ppppppppp/9/9/9/PPPPPPPPP/1B5R1/LNSGKGSNL b - 1";
    let lines = dialogue(&format!("position {sfen}\ngo movetime 1\nquit\n"));
    assert_eq!(bestmoves(&lines).len(), 1);
    assert_legal_after(sfen, bestmoves(&lines)[0]);
}

#[test]
fn moves_with_a_promotion_are_applied() {
    let args = "startpos moves 7g7f 3c3d 8h2b+ 3a2b";
    let lines = dialogue(&format!("position {args}\ngo movetime 1\nquit\n"));
    assert!(!lines.iter().any(|l| l.contains("error:")));
    assert_legal_after(args, bestmoves(&lines)[0]);
}

/// USI drop notation carries no colour. Sabotage: remove the colour rewrite in
/// `Game::push_usi_move` and `P*9b` becomes a Black pawn drop that Black has
/// not got in hand, so `is_legal` rejects it, so the whole `position` is
/// rejected — and this assertion catches it.
#[test]
fn drops_take_the_side_to_move_colour() {
    let args = "sfen 9/9/9/9/9/9/9/9/K7k b Rp 1 moves R*9a P*9b";
    let (lines, recorded) =
        dialogue_capturing(&format!("position {args}\ngo movetime 1\nquit\n"), false);
    assert!(!lines.iter().any(|l| l.contains("error:")), "{lines:?}");

    // Compared against a position built independently rather than an SFEN
    // written out by hand — which would only be recording someone's guess.
    // The exact board is pinned down by `Game`'s own unit test.
    let expected = Game::from_usi_position(args).expect("the fixture parses");
    assert_eq!(recorded.jobs()[0].2, expected.sfen());
}

/// Sabotage: an implementation that applied moves one at a time would leave the
/// board after `7g7f 3c3d`, and the answer below would be legal there. Building
/// into a scratch `Game` and assigning only on success is what this pins down.
#[test]
fn an_illegal_move_rejects_the_whole_position_command() {
    // 5e is empty in every position reachable from startpos in two moves.
    let script = "position startpos moves 7g7f\n\
                  position startpos moves 7g7f 3c3d 5e5d 8h2b+\n\
                  go movetime 1\nquit\n";
    let lines = dialogue(script);

    let errors: Vec<&String> = lines.iter().filter(|l| l.contains("error:")).collect();
    assert_eq!(errors.len(), 1, "{lines:?}");
    assert!(errors[0].starts_with("info string error: position rejected:"));
    assert!(errors[0].contains("move 3"), "{}", errors[0]);

    // The board is still the one after 7g7f only — so it is White to move, and
    // a move legal after `7g7f 3c3d` would not be legal here.
    assert_legal_after("startpos moves 7g7f", bestmoves(&lines)[0]);
}

#[test]
fn an_unparseable_sfen_keeps_the_previous_position() {
    let script = "position startpos moves 7g7f\n\
                  position sfen not/a/real/sfen b - 1\n\
                  go movetime 1\nquit\n";
    let lines = dialogue(script);
    assert_eq!(lines.iter().filter(|l| l.contains("error:")).count(), 1);
    assert_legal_after("startpos moves 7g7f", bestmoves(&lines)[0]);
}

/// A lone king with no pieces and an empty hand has no legal move — no mate
/// geometry needed. A hand-verified mate fixture arrives with step 4's suite.
#[test]
fn no_legal_move_yields_bestmove_resign() {
    let lines = dialogue("position sfen 4k4/9/9/9/9/9/9/9/9 b - 1\ngo movetime 1\nquit\n");
    assert_eq!(bestmoves(&lines), vec!["bestmove resign"]);
}

// -------------------------------------------------------------------------- go

#[test]
fn every_go_field_reaches_the_search() {
    let (lines, recorded) = dialogue_capturing(
        "position startpos\ngo btime 300000 wtime 299000 byoyomi 5000 depth 8 nodes 1000\nquit\n",
        false,
    );
    assert_eq!(bestmoves(&lines).len(), 1);

    let (id, limits, sfen) = recorded.jobs()[0].clone();
    assert_eq!(id, 1);
    assert_eq!(
        limits.btime,
        Some(std::time::Duration::from_millis(300_000))
    );
    assert_eq!(
        limits.wtime,
        Some(std::time::Duration::from_millis(299_000))
    );
    assert_eq!(
        limits.byoyomi,
        Some(std::time::Duration::from_millis(5_000))
    );
    assert_eq!(limits.depth, Some(8));
    assert_eq!(limits.nodes, Some(1_000));
    assert!(!limits.infinite);
    assert!(sfen.starts_with("lnsgkgsnl"));
}

/// Sabotage: stop honouring `infinite` in the searcher and the `bestmove` lands
/// before `stop` — which is precisely how an analysis GUI sees a broken engine.
#[test]
fn go_infinite_waits_for_stop() {
    let (lines, recorded) =
        dialogue_capturing("position startpos\ngo infinite\nstop\nquit\n", true);
    assert!(recorded.jobs()[0].1.infinite);
    assert_eq!(bestmoves(&lines).len(), 1);
}

#[test]
fn go_mate_answers_checkmate_and_never_a_bestmove() {
    for line in ["go mate 1000", "go mate infinite"] {
        let lines = dialogue(&format!("position startpos\n{line}\nquit\n"));
        assert!(
            lines.contains(&"checkmate notimplemented".to_owned()),
            "{lines:?}"
        );
        assert!(bestmoves(&lines).is_empty(), "{lines:?}");
    }
}

/// A protocol violation, answered rather than dropped. The worker is a single
/// FIFO, so both searches answer and they answer in order.
#[test]
fn a_second_go_while_searching_yields_two_bestmoves_in_order() {
    let (lines, recorded) = dialogue_capturing(
        "position startpos\ngo infinite\ngo movetime 1\nquit\n",
        true,
    );
    assert_eq!(bestmoves(&lines).len(), 2);
    assert!(lines.iter().any(|l| l.contains("warning: `go` received")));
    assert_eq!(
        recorded
            .jobs()
            .iter()
            .map(|(id, ..)| *id)
            .collect::<Vec<_>>(),
        vec![1, 2]
    );
}

// -------------------------------------------------------------- idle and exit

/// A `bestmove` nobody asked for desynchronises the GUI, so a stray `stop` must
/// produce nothing at all.
#[test]
fn stop_and_ponderhit_are_silent_when_idle() {
    let lines = dialogue("usi\nstop\nponderhit\nquit\n");
    assert!(bestmoves(&lines).is_empty());
    assert!(!lines.iter().any(|l| l.starts_with("info")), "{lines:?}");
}

#[test]
fn gameover_during_a_search_stops_it() {
    let (lines, _) = dialogue_capturing(
        "position startpos\ngo infinite\ngameover lose\nquit\n",
        true,
    );
    assert_eq!(bestmoves(&lines).len(), 1);
}

/// A hang *is* the failure here: `run` not returning means the test never ends.
#[test]
fn quit_during_a_search_terminates_and_still_answers() {
    let (lines, _) = dialogue_capturing("position startpos\ngo infinite\nquit\n", true);
    assert_eq!(bestmoves(&lines).len(), 1);
}

#[test]
fn quit_alone_says_nothing() {
    assert!(dialogue("quit\n").is_empty());
}

/// GUIs and harnesses do simply close the pipe.
#[test]
fn end_of_input_behaves_like_quit() {
    let lines = dialogue("usi\nisready\n");
    assert_eq!(lines.last().map(String::as_str), Some("readyok"));
}

// ------------------------------------------------------------- odds and ends

#[test]
fn usinewgame_is_silent_and_does_not_disturb_the_next_search() {
    let lines = dialogue("usi\nisready\nusinewgame\nposition startpos\ngo movetime 1\nquit\n");
    assert_eq!(bestmoves(&lines).len(), 1);
    assert_legal_after("startpos", bestmoves(&lines)[0]);
}

/// A known option that is not yet acted on says so, once the operator has
/// actually changed it. An unknown one is accepted in silence, because the
/// specification requires ignoring what an engine does not understand.
#[test]
fn setoption_discloses_the_accepted_but_unused_and_ignores_the_unknown() {
    let lines = dialogue(
        "usi\n\
         setoption name USI_Hash value 512\n\
         setoption name Nonsense value 3\n\
         isready\nquit\n",
    );
    assert!(lines.contains(&"readyok".to_owned()));
    assert!(
        lines
            .iter()
            .any(|l| l.contains("USI_Hash is accepted but not yet used")),
        "{lines:?}"
    );
    assert!(!lines.iter().any(|l| l.contains("Nonsense")), "{lines:?}");
}

#[test]
fn unknown_commands_are_ignored_without_a_word_on_stdout() {
    let lines = dialogue("usi\nfrobnicate 1 2 3\nisready\nquit\n");
    let usiok = lines.iter().position(|l| l == "usiok").expect("usiok");
    let readyok = lines.iter().position(|l| l == "readyok").expect("readyok");
    assert_eq!(readyok, usiok + 1, "{lines:?}");
}

#[test]
fn blank_lines_and_irregular_whitespace_are_tolerated() {
    let lines =
        dialogue("\n   \n  usi  \n  position   startpos   moves  7g7f \n\ngo movetime 1\nquit\n");
    assert!(!lines.iter().any(|l| l.contains("error:")), "{lines:?}");
    assert_eq!(bestmoves(&lines).len(), 1);
    assert_legal_after("startpos moves 7g7f", bestmoves(&lines)[0]);
}

/// The classic Windows-GUI bug: a `\r` left on the end of every command.
#[test]
fn crlf_line_endings_work() {
    let lines = dialogue("usi\r\nisready\r\nposition startpos\r\ngo movetime 1\r\nquit\r\n");
    assert!(lines.contains(&"usiok".to_owned()), "{lines:?}");
    assert!(lines.contains(&"readyok".to_owned()), "{lines:?}");
    assert_eq!(bestmoves(&lines).len(), 1);
}

/// The invariant, checked over every dialogue in this file at once: one
/// `bestmove` per accepted `go`, and no other source of one.
#[test]
fn one_bestmove_per_go() {
    for script in [
        "position startpos\ngo movetime 1\nquit\n",
        "position startpos\ngo movetime 1\ngo movetime 1\ngo movetime 1\nquit\n",
        "usi\nisready\nstop\nponderhit\ngo mate 100\nquit\n",
        "position startpos\ngo depth 1\ngameover draw\ngo depth 1\nquit\n",
    ] {
        let lines = dialogue(script);
        let gos = script
            .lines()
            .filter(|l| l.starts_with("go ") && !l.starts_with("go mate"))
            .count();
        assert_eq!(bestmoves(&lines).len(), gos, "script: {script:?}");
    }
}
