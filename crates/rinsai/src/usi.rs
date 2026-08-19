//! The USI protocol loop.
//!
//! # The one invariant
//!
//! **Every accepted `go` produces exactly one `bestmove`, and nothing else ever
//! produces one — including when the search panics.** Structural rather than
//! disciplined: a `go` allocates exactly one job, the worker's loop body emits
//! on every path because [`Searcher::search`] *returns* an answer instead of
//! printing one and a panic is caught and answered, and [`Output::bestmove`]
//! has exactly two callers, both here — the worker's emit closure built in
//! [`run`], and `Engine::go`'s fallback for when the worker is gone.
//!
//! ⚠️ To audit it, grep `crates/*/src` for the **string literal**, opening
//! double-quote included; it must occur in `output.rs` and nowhere else.
//! Grepping the bare word matches this sentence and proves nothing.
//!
//! # Error policy, stated once
//!
//! Bad input never changes engine state and never stops the loop, and is always
//! reported on stderr, where it cannot desync the protocol. It is *additionally*
//! reported as `info string error: …` only when the GUI's model of the engine
//! would otherwise be wrong — in practice, a refused `position`. Unknown
//! commands are ignored silently, as the specification requires.

// Never panic on input. ⚠️ These are `restriction` lints, so `-D warnings`
// alone does not enable them; this attribute is what makes the rule
// compiler-checked rather than reviewed.
#![deny(clippy::unwrap_used, clippy::expect_used)]
// Tests may assert loudly; the rule above is about input handling.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod command;
mod options;

use std::io::{BufRead, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use rinsai_search::{BestMove, Game, SearchDriver, SearchJob, SearchSignals, Searcher};

use crate::output::Output;
use command::{GuiCommand, parse_line};
use options::{OPTIONS, Options, Slot};

/// Runs the protocol until `quit` or end of input.
///
/// Generic over its streams so the conformance tests can drive it over
/// in-memory pipes. It returns only after the search thread has been joined, so
/// every byte has been written and flushed by then — which is what lets those
/// tests assert on a complete transcript without sleeping.
pub fn run<R, W, S>(input: R, output: W, searcher: S)
where
    R: BufRead,
    W: Write + Send + 'static,
    S: Searcher + 'static,
{
    let out = Output::new(output);
    let sink = Arc::new(out.clone());
    let emitter = out.clone();

    // The protocol thread cannot see the worker finish, so the worker tells it:
    // up before a job is submitted, down as its answer is sent. A plain
    // `searching` flag cleared only by `stop` leaves the engine convinced a
    // finished search is still running.
    //
    // ⚠️ The counter drops **before** the line is written, and the order
    // matters. The other way round leaves a window where the `bestmove` is on
    // the wire while the engine still believes it is searching, so a GUI doing
    // exactly the right thing gets accused of a protocol violation. This way
    // the window holds the opposite state, where a `go` queues behind this job
    // and a `stop` is ignored — both correct for a finished search.
    let outstanding = Arc::new(AtomicUsize::new(0));
    let finished = Arc::clone(&outstanding);
    let driver = SearchDriver::spawn(searcher, sink, move |best| {
        finished.fetch_sub(1, Ordering::AcqRel);
        emitter.bestmove(best);
    });

    let mut engine = Engine {
        out,
        driver,
        game: Game::from_startpos(),
        options: Options::default(),
        outstanding,
        pondering: false,
        next_id: 0,
        current: None,
    };

    // ⚠️ Read bytes, not `String`. `BufRead::lines` yields `Err(InvalidData)`
    // for any line that is not valid UTF-8, indistinguishable from a real I/O
    // error — so one stray byte ends the engine silently, exit status 0,
    // nothing in the log. Not exotic input: a Japanese Windows GUI speaks
    // CP932, so `setoption name EvalFile value C:\将棋\eval` is the shape that
    // arrives, and E3 will ask for one.
    let mut input = input;
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match input.read_until(b'\n', &mut buf) {
            Ok(0) => break,
            Ok(_) => {}
            Err(e) => {
                warn(&format!("stdin: {e}"));
                break;
            }
        }
        let line = String::from_utf8_lossy(&buf);
        if matches!(line, std::borrow::Cow::Owned(_)) {
            warn("stdin: a line was not valid UTF-8 and was read with replacements");
        }
        let Some(command) = parse_line(&line) else {
            continue;
        };
        if engine.handle(command) == Flow::Quit {
            break;
        }
    }

    // End of input is `quit`: GUIs and harnesses do simply close the pipe.
    engine.shutdown();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Flow {
    Continue,
    Quit,
}

struct Engine<W: Write + Send + 'static> {
    out: Output<W>,
    driver: SearchDriver,
    /// The protocol thread's own game. A running search holds a separate copy,
    /// so a `position` arriving mid-search cannot disturb it — which is why
    /// there is no lock on the board anywhere in this program.
    game: Game,
    options: Options,
    /// Searches submitted but not yet answered. Raised here, lowered by the
    /// worker — the only thing that tells the protocol thread a search ended on
    /// its own rather than by `stop`.
    outstanding: Arc<AtomicUsize>,
    /// Whether the outstanding search was asked for with `ponder`.
    pondering: bool,
    next_id: u64,
    /// The signals of the search currently running. A *fresh* set is made per
    /// `go`, which is what stops a late `stop` from aborting the next search.
    current: Option<Arc<SearchSignals>>,
}

impl<W: Write + Send + 'static> Engine<W> {
    fn handle(&mut self, command: GuiCommand) -> Flow {
        match command {
            GuiCommand::Usi => self.greet(),
            GuiCommand::IsReady => self.ready(),
            GuiCommand::SetOption { name, value } => self.set_option(&name, value.as_deref()),
            GuiCommand::UsiNewGame => {
                if !self.driver.new_game() {
                    warn("the search thread is gone; `usinewgame` was not delivered");
                }
            }
            GuiCommand::Position(args) => self.set_position(&args),
            GuiCommand::Go(limits) => self.go(limits),
            GuiCommand::GoMate => {
                // Mate solving is `tsumeshogi-solver`'s territory.
                // `notimplemented` is the specification's own token for
                // exactly this, and it is not a `bestmove`.
                self.out.line("checkmate notimplemented");
            }
            GuiCommand::Stop => self.stop(),
            GuiCommand::PonderHit => self.ponderhit(),
            GuiCommand::GameOver => self.game_over(),
            GuiCommand::Quit => return Flow::Quit,
            GuiCommand::Unknown(line) => warn(&format!("unknown command: {line}")),
        }
        Flow::Continue
    }

    fn greet(&self) {
        self.out
            .line(&format!("id name rinsai {}", env!("CARGO_PKG_VERSION")));
        self.out.line("id author sugyan");
        for spec in OPTIONS {
            self.out.line(&spec.to_string());
        }
        self.out.line("usiok");
    }

    fn ready(&self) {
        // The specification lets an engine take arbitrarily long here, which is
        // where slow initialisation belongs. ⚠️ Nothing uses that yet: the
        // transposition table is allocated in `main`, and a resize goes to the
        // worker rather than through here.
        for (name, planned) in self.options.unhonoured_changes() {
            self.out.info_string(&format!(
                "warning: option {name} is accepted but not yet used (planned: {planned})"
            ));
        }
        self.out.line("readyok");
    }

    fn set_option(&mut self, name: &str, value: Option<&str>) {
        match self.options.set(name, value) {
            // Queued behind whatever the worker is doing, never waited on —
            // `SearchDriver::set_hash` carries why.
            Ok(Slot::HashMb) => {
                if !self.driver.set_hash(self.options.hash_mb()) {
                    warn("the search thread is gone; `USI_Hash` was not delivered");
                }
            }
            Ok(Slot::DeliveryMarginMs) => {
                if !self.driver.set_margin(self.options.delivery_margin()) {
                    warn("the search thread is gone; `DeliveryMargin` was not delivered");
                }
            }
            Ok(Slot::Ponder) => {}
            // Not state-affecting from the GUI's point of view, so stderr only.
            Err(e) => warn(&format!("setoption: {e}")),
        }
    }

    fn set_position(&mut self, args: &str) {
        match Game::from_usi_position(args) {
            // ⚠️ Assigned only on complete success: a half-applied position
            // leaves the engine on a board neither side believes in, playing a
            // legal-but-nonsense move that reads as a search bug.
            Ok(game) => self.game = game,
            Err(e) => {
                let message = format!("error: position rejected: {e}");
                warn(&message);
                // The one case that *is* state-affecting: if the GUI thinks the
                // engine moved on and it did not, the game silently diverges.
                self.out.info_string(&message);
            }
        }
        if self.searching() {
            warn("`position` arrived while a search was running");
        }
    }

    /// Whether a search has been submitted and not yet answered.
    fn searching(&self) -> bool {
        self.outstanding.load(Ordering::Acquire) > 0
    }

    fn go(&mut self, limits: rinsai_search::Limits) {
        if self.searching() {
            // A protocol violation. Answer it rather than dropping it: the
            // worker is a single FIFO, so both searches answer, in order.
            let message = "warning: `go` received while a search was running";
            warn(message);
            self.out.info_string(message);
            self.stop();
        }
        let signals = Arc::new(SearchSignals::new());
        self.current = Some(Arc::clone(&signals));
        self.pondering = limits.ponder;
        self.next_id += 1;
        // Raised *before* the submit, so the worker can never lower it first.
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        let queued = self.driver.submit(SearchJob {
            id: self.next_id,
            game: self.game.clone(),
            limits,
            signals,
        });
        if !queued {
            // The worker is gone and we owe exactly one answer, so undo the
            // bookkeeping and answer here. Without it the counter stays above
            // zero for the rest of the session and the engine looks busy
            // forever, losing on time in silence.
            self.outstanding.fetch_sub(1, Ordering::AcqRel);
            self.current = None;
            self.pondering = false;
            let message = "error: the search thread is gone; answering `resign`";
            warn(message);
            self.out.info_string(message);
            self.out.bestmove(BestMove::Resign);
        }
    }

    fn stop(&mut self) {
        // Note the counter is *not* lowered here: a stopped search is still
        // running until it answers, and it is its answer that lowers it.
        match self.current.take().filter(|_| self.searching()) {
            Some(signals) => {
                signals.stop();
                self.pondering = false;
            }
            // Silently, and deliberately: a `bestmove` nobody asked for
            // desynchronises the GUI, which is worse than a lost `stop`.
            None => warn("`stop` with no search running"),
        }
    }

    fn ponderhit(&mut self) {
        match (self.pondering && self.searching(), self.current.as_ref()) {
            (true, Some(signals)) => {
                signals.ponderhit();
                self.pondering = false;
            }
            _ => warn("`ponderhit` with no ponder search running"),
        }
    }

    fn game_over(&mut self) {
        if self.searching() {
            // The worker still emits that search's `bestmove`; every GUI
            // discards it. An unconditional "one bestmove per go" is worth more
            // than a tidy transcript.
            self.stop();
        }
        self.game = Game::from_startpos();
    }

    fn shutdown(&mut self) {
        if let Some(signals) = self.current.take() {
            signals.stop();
        }
        self.driver.shutdown();
    }
}

/// Everything diagnostic goes to stderr, where a GUI's log will capture it and
/// where it cannot corrupt the protocol.
fn warn(message: &str) {
    eprintln!("rinsai: {message}");
}
