//! The seam between a protocol and a search.
//!
//! Nothing here knows what USI is: a [`Searcher`] is handed a [`SearchJob`] and
//! returns a [`BestMove`], and the caller — the engine binary today, a
//! self-play driver at E3 — decides what to do with it. The shapes are chosen
//! so that steps 2 to 5 add behaviour without changing a signature.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use shogi_core::Move;

use crate::game::Game;

/// What a `go` command asked for.
///
/// Every field is parsed from E0 step 1 and most are ignored until later steps,
/// so the grammar and its conformance tests are written once. `infinite` and
/// `ponder` are honoured immediately: an engine that answers `go infinite` at
/// once is visibly broken to any analysis GUI, and that bug would be invisible
/// in a design where "there is no search yet".
///
/// The deadline is deliberately *not* part of this. Real time management
/// decides — extend on a fail-low, cut short when the move is obvious — so the
/// search has to own the clock and see the raw parameters. Step 5 then changes
/// only what the searcher does with them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    pub btime: Option<Duration>,
    pub wtime: Option<Duration>,
    pub binc: Option<Duration>,
    pub winc: Option<Duration>,
    pub byoyomi: Option<Duration>,
    pub movetime: Option<Duration>,
    pub depth: Option<u32>,
    pub nodes: Option<u64>,
    pub infinite: bool,
    pub ponder: bool,
}

/// The flags that stop a search from outside it.
///
/// Atomics rather than a channel: a search polls `stopped()` every ~1024 nodes,
/// where a relaxed load of a rarely-written bool beats `try_recv` outright, and
/// at E2 one `Arc` broadcasts to every Lazy SMP thread where a channel would
/// need one per thread.
///
/// A **fresh set is created per `go`**, so a `stop` that arrives late cannot
/// abort the *next* search — the classic bug in globally-flagged engines.
#[derive(Debug, Default)]
pub struct SearchSignals {
    stop: AtomicBool,
    ponder_hit: AtomicBool,
    waiter: (Mutex<()>, Condvar),
}

impl SearchSignals {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The hot-path poll. Relaxed is correct: this flag only gates "abandon and
    /// return", and the result handoff is ordered by the output mutex and the
    /// worker's own channel.
    #[inline]
    #[must_use]
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    #[inline]
    #[must_use]
    pub fn ponder_hit(&self) -> bool {
        self.ponder_hit.load(Ordering::Relaxed)
    }

    /// Asks the search to return as soon as it can.
    pub fn stop(&self) {
        self.raise(&self.stop);
    }

    /// Tells a ponder search that the move it guessed was played.
    pub fn ponderhit(&self) {
        self.raise(&self.ponder_hit);
    }

    /// Blocks until either flag is raised. **Never call this on the hot path** —
    /// it exists so a search with nothing to do (`go infinite`, `go ponder`)
    /// can wait without spinning.
    pub fn wait_until_signalled(&self) {
        let (lock, cvar) = &self.waiter;
        let mut guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        while !self.stopped() && !self.ponder_hit() {
            guard = cvar.wait(guard).unwrap_or_else(|e| e.into_inner());
        }
    }

    /// Raises a flag under the waiter's lock, so a signal cannot slip between a
    /// waiter's last check and its `wait`.
    fn raise(&self, flag: &AtomicBool) {
        let (lock, cvar) = &self.waiter;
        let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
        flag.store(true, Ordering::Release);
        cvar.notify_all();
    }
}

/// One search to run.
#[derive(Debug)]
pub struct SearchJob {
    /// Increments per `go`, so a transcript can be matched to a job.
    pub id: u64,
    /// The searcher's own copy of the game — see [`Game`]'s `Clone`. Nothing is
    /// shared with the protocol thread, so there is no lock on the board and a
    /// `position` arriving mid-search cannot disturb a running one.
    pub game: Game,
    pub limits: Limits,
    pub signals: Arc<SearchSignals>,
}

/// A search's answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BestMove {
    Play {
        mv: Move,
        /// The move the opponent is expected to reply with. Always `None` until
        /// E2 gives ponder something to stand on: a ponder move the engine
        /// cannot actually back up is worse than none.
        ponder: Option<Move>,
    },
    /// The side to move has no legal move.
    Resign,
    // `Win` (入玉宣言) joins at E2, with the declaration rules.
}

/// Where `info` lines go while a search runs.
///
/// A callback rather than a channel, for four reasons. The protocol thread is
/// blocked reading stdin and cannot pump a receiver, so `info` would either
/// freeze mid-search or need a third thread just to drain it. A channel cannot
/// carry a borrowed principal variation without allocating per event. E3's
/// self-play wants a silent search, and a no-op implementation of this compiles
/// to nothing. And shunsai already made the same choice one layer down, in
/// `generate_moves`.
pub trait InfoSink: Send + Sync {
    fn info(&self, line: &str);
}

/// A no-op sink, for a search that nobody is watching.
#[derive(Clone, Copy, Debug, Default)]
pub struct SilentSink;

impl InfoSink for SilentSink {
    fn info(&self, _line: &str) {}
}

/// Something that can search a position.
///
/// A trait rather than a concrete type because it is the seam the conformance
/// tests inject through: a recording implementation turns "did `go infinite`
/// reach the search?" into a value assertion instead of a sleep. Anything that
/// must survive between searches — the transposition table from step 3, the
/// history tables from E1, E3's accumulator stack — lives in the implementor,
/// which the worker thread owns for its whole life.
pub trait Searcher: Send {
    /// Runs one search, returning promptly once `job.signals.stopped()`.
    ///
    /// Note that this **returns** a [`BestMove`] rather than printing one. That
    /// is what makes "exactly one `bestmove` per `go`" a structural property of
    /// the driver rather than a rule every code path has to remember.
    fn search(&mut self, job: &SearchJob, out: &dyn InfoSink) -> BestMove;

    /// Clears whatever carries between games. Called on `usinewgame`.
    fn new_game(&mut self) {}
}

enum Command {
    Go(Box<SearchJob>),
    NewGame,
}

/// Owns the search thread.
///
/// One long-lived worker draining a FIFO, not a thread per `go`: the searcher's
/// state has somewhere to live, and even a protocol-violating double `go`
/// produces its two answers in the order they were asked for.
#[derive(Debug)]
pub struct SearchDriver {
    tx: Option<Sender<Command>>,
    handle: Option<JoinHandle<()>>,
}

impl SearchDriver {
    /// Starts the worker. `emit` is called with each search's answer, on the
    /// worker thread — it is supplied by the caller so that nothing in this
    /// crate has to know how a `bestmove` is spelled.
    pub fn spawn<S, E>(searcher: S, sink: Arc<dyn InfoSink>, emit: E) -> Self
    where
        S: Searcher + 'static,
        E: Fn(BestMove) + Send + 'static,
    {
        let (tx, rx) = channel();
        let handle = thread::Builder::new()
            .name("rinsai-search".to_owned())
            .spawn(move || worker(searcher, &rx, sink.as_ref(), &emit))
            .expect("the operating system can start a thread");
        Self {
            tx: Some(tx),
            handle: Some(handle),
        }
    }

    /// Queues a search. Dropped silently if the worker is already gone, which
    /// only happens during shutdown.
    pub fn submit(&self, job: SearchJob) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::Go(Box::new(job)));
        }
    }

    /// Queues a `usinewgame`, in order with the searches around it.
    pub fn new_game(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::NewGame);
        }
    }

    /// Drops the queue and waits for the worker to finish what it is doing.
    ///
    /// The caller must have raised `stop` on any running search first, or this
    /// waits for that search to finish on its own terms.
    pub fn shutdown(&mut self) {
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for SearchDriver {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker<S: Searcher>(
    mut searcher: S,
    rx: &Receiver<Command>,
    sink: &dyn InfoSink,
    emit: &dyn Fn(BestMove),
) {
    for command in rx {
        match command {
            Command::NewGame => searcher.new_game(),
            Command::Go(job) => {
                // `emit` is the last statement on every path, because `search`
                // returns rather than prints. One job in, one answer out.
                let best = searcher.search(&job, sink);
                emit(best);
            }
        }
    }
}

impl fmt::Display for BestMove {
    /// Not the USI wire format — that belongs to the protocol layer. This is
    /// for logs and assertion messages.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use shogi_core::ToUsi;
        match self {
            Self::Resign => f.write_str("resign"),
            Self::Play { mv, ponder: None } => f.write_str(&mv.to_usi_owned()),
            Self::Play {
                mv,
                ponder: Some(p),
            } => write!(f, "{} (ponder {})", mv.to_usi_owned(), p.to_usi_owned()),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicU32;

    use super::*;

    /// A searcher that records what it was given and answers immediately.
    struct Recorder(Arc<Mutex<Vec<(u64, Limits)>>>, Arc<AtomicU32>);

    impl Searcher for Recorder {
        fn search(&mut self, job: &SearchJob, out: &dyn InfoSink) -> BestMove {
            self.0
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push((job.id, job.limits));
            out.info("info string recorded");
            BestMove::Resign
        }

        fn new_game(&mut self) {
            self.1.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn job(id: u64, limits: Limits) -> SearchJob {
        SearchJob {
            id,
            game: Game::from_startpos(),
            limits,
            signals: Arc::new(SearchSignals::new()),
        }
    }

    /// Jobs must come out in the order they went in — this is what makes even a
    /// protocol-violating double `go` produce two answers in the right order.
    #[test]
    fn jobs_are_answered_in_submission_order() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let games = Arc::new(AtomicU32::new(0));
        let answers = Arc::new(Mutex::new(Vec::new()));
        let sink: Arc<dyn InfoSink> = Arc::new(SilentSink);

        let recorded = Arc::clone(&answers);
        let mut driver = SearchDriver::spawn(
            Recorder(Arc::clone(&seen), Arc::clone(&games)),
            sink,
            move |best| {
                recorded
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(best.to_string());
            },
        );

        driver.new_game();
        for id in 0..5 {
            driver.submit(job(
                id,
                Limits {
                    depth: Some(id as u32),
                    ..Limits::default()
                },
            ));
        }
        driver.shutdown();

        let seen = seen.lock().unwrap_or_else(|e| e.into_inner());
        assert_eq!(
            seen.iter().map(|(id, _)| *id).collect::<Vec<_>>(),
            vec![0, 1, 2, 3, 4]
        );
        assert_eq!(seen[3].1.depth, Some(3));
        assert_eq!(games.load(Ordering::Relaxed), 1);
        assert_eq!(answers.lock().unwrap_or_else(|e| e.into_inner()).len(), 5);
    }

    /// A waiting search must wake on `stop`, whether the signal arrives before
    /// or after it starts waiting — the lost-wakeup race the waiter's lock
    /// exists to close.
    #[test]
    fn a_waiting_search_wakes_on_stop() {
        let signals = Arc::new(SearchSignals::new());
        assert!(!signals.stopped());

        // Signal first, then wait: must return immediately.
        signals.stop();
        signals.wait_until_signalled();
        assert!(signals.stopped());

        // Wait first, then signal, from another thread.
        let signals = Arc::new(SearchSignals::new());
        let waiter = Arc::clone(&signals);
        let handle = thread::spawn(move || {
            waiter.wait_until_signalled();
            waiter.stopped()
        });
        signals.stop();
        assert!(handle.join().expect("the waiter thread finished"));
    }

    #[test]
    fn ponderhit_also_wakes_a_waiting_search() {
        let signals = Arc::new(SearchSignals::new());
        let waiter = Arc::clone(&signals);
        let handle = thread::spawn(move || {
            waiter.wait_until_signalled();
            (waiter.stopped(), waiter.ponder_hit())
        });
        signals.ponderhit();
        assert_eq!(
            handle.join().expect("the waiter thread finished"),
            (false, true)
        );
    }
}
