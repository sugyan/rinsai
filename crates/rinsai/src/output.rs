//! The engine's one way of speaking.

use std::fmt;
use std::io::Write;
use std::sync::{Arc, Mutex};

use rinsai_search::{BestMove, InfoSink};
use shogi_core::ToUsi;

/// Every line the engine writes goes through here.
///
/// The mutex is what keeps the search thread's `info` from interleaving with
/// the protocol thread's `bestmove`: one lock spans a whole line.
///
/// **Every line is flushed.** That is not hygiene — stdout is line-buffered
/// only on a terminal and block-buffered when piped to a GUI or a match
/// harness, which is the classic USI engine hang. Lock poisoning is absorbed
/// rather than propagated, so a panic somewhere else cannot silence the engine.
///
/// Write errors — a broken pipe means the GUI is gone — are dropped. stdin will
/// be at EOF anyway and the loop is about to end; panicking on the way out
/// would only turn a tidy exit into a crash report.
pub struct Output<W: Write + Send>(Arc<Mutex<W>>);

impl<W: Write + Send> Output<W> {
    pub fn new(sink: W) -> Self {
        Self(Arc::new(Mutex::new(sink)))
    }

    /// Writes one line and flushes it.
    pub fn line(&self, line: &str) {
        let mut sink = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let _ = writeln!(sink, "{line}");
        let _ = sink.flush();
    }

    /// `info string …` — the channel a GUI shows in its log.
    pub fn info_string(&self, message: &str) {
        self.line(&format!("info string {message}"));
    }

    /// The **only** place a `bestmove` is written. Keeping it here, called from
    /// exactly one closure in `main`, is what makes "one `bestmove` per `go`"
    /// checkable by `grep` rather than by review.
    pub fn bestmove(&self, best: BestMove) {
        self.line(&match best {
            BestMove::Resign => "bestmove resign".to_owned(),
            BestMove::Play { mv, ponder: None } => format!("bestmove {}", mv.to_usi_owned()),
            BestMove::Play {
                mv,
                ponder: Some(p),
            } => format!("bestmove {} ponder {}", mv.to_usi_owned(), p.to_usi_owned()),
        });
    }
}

impl<W: Write + Send> Clone for Output<W> {
    fn clone(&self) -> Self {
        Self(Arc::clone(&self.0))
    }
}

impl<W: Write + Send> fmt::Debug for Output<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Output(..)")
    }
}

impl<W: Write + Send> InfoSink for Output<W> {
    fn info(&self, line: &str) {
        self.line(line);
    }
}
