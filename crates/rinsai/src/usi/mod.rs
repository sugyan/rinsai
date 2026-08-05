//! The USI protocol loop.
//!
//! # The one invariant
//!
//! **Every accepted `go` produces exactly one `bestmove`, and nothing else ever
//! produces one.** It holds structurally rather than by discipline: a `go`
//! allocates exactly one job, the worker's loop body emits on every path
//! because [`Searcher::search`] *returns* an answer instead of printing one,
//! and [`Output::bestmove`] is called from exactly one closure, constructed
//! here. `grep -rn 'bestmove' crates/` should find it in `output.rs` and
//! nowhere else.
//!
//! # Error policy, stated once
//!
//! Bad input never changes engine state and never stops the loop. It is always
//! reported on stderr — every GUI and harness captures that into a log, and it
//! cannot desync the protocol. It is *additionally* reported as
//! `info string error: …` only when the GUI's model of the engine would
//! otherwise be wrong, which in practice means a `position` was refused. Unknown
//! commands are ignored silently, as the specification requires: GUIs send
//! their own extensions, and echoing them back would be constant noise.

// Never panic on input. These are `restriction` lints, so `-D warnings` alone
// does not enable them — this attribute is what makes the rule compiler-checked
// rather than reviewed.
#![deny(clippy::unwrap_used, clippy::expect_used)]
// Tests may assert loudly; the rule above is about input handling.
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used))]

mod command;
mod options;

use std::io::{BufRead, Write};
use std::sync::Arc;

use rinsai_search::{Game, SearchDriver, SearchJob, SearchSignals, Searcher};

use crate::output::Output;
use command::{GuiCommand, parse_line};
use options::{OPTIONS, Options};

/// Runs the protocol until `quit` or end of input.
///
/// Generic over its streams so the conformance tests can drive it over
/// in-memory pipes rather than spawning a process per case. It returns only
/// after the search thread has been joined, so when it returns every byte has
/// been written and flushed — which is what lets those tests assert on a
/// complete transcript without sleeping.
pub fn run<R, W, S>(input: R, output: W, searcher: S)
where
    R: BufRead,
    W: Write + Send + 'static,
    S: Searcher + 'static,
{
    let out = Output::new(output);
    let sink = Arc::new(out.clone());
    let emitter = out.clone();
    let driver = SearchDriver::spawn(searcher, sink, move |best| emitter.bestmove(best));

    let mut engine = Engine {
        out,
        driver,
        game: Game::from_startpos(),
        options: Options::default(),
        searching: false,
        pondering: false,
        next_id: 0,
        current: None,
    };

    for line in input.lines() {
        let Ok(line) = line else {
            // A read error on stdin is the GUI going away mid-line. There is
            // nobody left to tell.
            break;
        };
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
    searching: bool,
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
            GuiCommand::UsiNewGame => self.driver.new_game(),
            GuiCommand::Position(args) => self.set_position(&args),
            GuiCommand::Go(limits) => self.go(limits),
            GuiCommand::GoMate => {
                // Mate solving is `tsumeshogi-solver`'s territory (DESIGN.md
                // §2). `notimplemented` is the specification's own token for
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
        // where slow initialisation belongs: the transposition table at step 3,
        // the evaluation network at E3.
        for (name, planned) in self.options.unhonoured_changes() {
            self.out.info_string(&format!(
                "warning: option {name} is accepted but not yet used (planned: {planned})"
            ));
        }
        self.out.line("readyok");
    }

    fn set_option(&mut self, name: &str, value: Option<&str>) {
        // Not state-affecting from the GUI's point of view, so stderr only.
        if let Err(e) = self.options.set(name, value) {
            warn(&format!("setoption: {e}"));
        }
    }

    fn set_position(&mut self, args: &str) {
        match Game::from_usi_position(args) {
            // Assigned only on complete success. A half-applied position would
            // leave the engine on a board neither side believes in, and it
            // would then play a legal-but-nonsense move that reads as a search
            // bug rather than as a rejected command.
            Ok(game) => self.game = game,
            Err(e) => {
                let message = format!("error: position rejected: {e}");
                warn(&message);
                // The one case that *is* state-affecting: if the GUI thinks the
                // engine moved on and it did not, the game silently diverges.
                self.out.info_string(&message);
            }
        }
        if self.searching {
            warn("`position` arrived while a search was running");
        }
    }

    fn go(&mut self, limits: rinsai_search::Limits) {
        if self.searching {
            // A protocol violation. Answer it rather than dropping it: the
            // worker is a single FIFO, so both searches answer, in order.
            let message = "warning: `go` received while a search was running";
            warn(message);
            self.out.info_string(message);
            self.stop();
        }
        let signals = Arc::new(SearchSignals::new());
        self.current = Some(Arc::clone(&signals));
        self.searching = true;
        self.pondering = limits.ponder;
        self.next_id += 1;
        self.driver.submit(SearchJob {
            id: self.next_id,
            game: self.game.clone(),
            limits,
            signals,
        });
    }

    fn stop(&mut self) {
        match self.current.take() {
            Some(signals) => {
                signals.stop();
                self.searching = false;
                self.pondering = false;
            }
            // Silently, and deliberately: a `bestmove` nobody asked for
            // desynchronises the GUI, which is worse than a lost `stop`.
            None => warn("`stop` with no search running"),
        }
    }

    fn ponderhit(&mut self) {
        match (self.pondering, self.current.as_ref()) {
            (true, Some(signals)) => {
                signals.ponderhit();
                self.pondering = false;
            }
            _ => warn("`ponderhit` with no ponder search running"),
        }
    }

    fn game_over(&mut self) {
        if self.searching {
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
