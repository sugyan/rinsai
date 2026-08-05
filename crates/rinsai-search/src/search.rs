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
/// Atomics rather than a channel. The load-bearing reason is E2: one `Arc`
/// broadcasts to every Lazy SMP thread, where a channel would need one per
/// thread. A relaxed load of a rarely-written bool should also be cheaper than
/// `try_recv` on the polling path — **unmeasured, and there is no polling path
/// yet**; the interval (1024 nodes is the usual starting point) is step 2's to
/// choose and step 5's to measure against the time-management SPRT.
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
    /// The most recently submitted job's signals, so [`Drop`] can stop a search
    /// it is about to wait for. Touched twice per `go` and never on a search's
    /// hot path, so the lock costs nothing.
    latest: Mutex<Option<Arc<SearchSignals>>>,
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
            latest: Mutex::new(None),
        }
    }

    /// Queues a search. Dropped silently if the worker is already gone, which
    /// only happens during shutdown.
    pub fn submit(&self, job: SearchJob) {
        *self.latest.lock().unwrap_or_else(|e| e.into_inner()) = Some(Arc::clone(&job.signals));
        if let Some(tx) = &self.tx {
            // The receiver is gone only if the worker thread is gone, which
            // means it panicked — `worker` otherwise runs until the channel
            // closes. Nothing here can recover from that, so the job is
            // dropped; the panic itself is already on stderr.
            let _ = tx.send(Command::Go(Box::new(job)));
        }
    }

    /// Queues a `usinewgame`, in order with the searches around it.
    pub fn new_game(&self) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(Command::NewGame);
        }
    }

    /// Stops any running search, drops the queue, and waits for the worker.
    ///
    /// Raising `stop` here rather than requiring the caller to have done it is
    /// what keeps [`Drop`] from blocking forever: a `go infinite` never ends on
    /// its own, so a `join` without a `stop` waits for something that will not
    /// happen. The caller may of course have stopped it already; `stop` is
    /// idempotent.
    pub fn shutdown(&mut self) {
        if let Some(signals) = self.latest.lock().unwrap_or_else(|e| e.into_inner()).take() {
            signals.stop();
        }
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            // An `Err` here is the worker having panicked. `worker` catches
            // panics from the searcher itself, so reaching this means the
            // driver's own loop failed — report it rather than swallowing it,
            // because the alternative is an engine that looks healthy and never
            // moves again.
            if handle.join().is_err() {
                eprintln!("rinsai: the search thread died; no further search will run");
            }
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
                // Catching the panic is what keeps "one answer per job" true
                // when a search goes wrong. Without it the worker thread dies
                // with this loop: the answer never comes, every later job is
                // sent into a closed channel, and the engine goes on answering
                // `usi` and `isready` normally while never moving again — a
                // failure that reads, from a match harness, as a clean exit
                // after a game lost on time. Resigning is a far better failure
                // than hanging, and the panic message is already on stderr.
                //
                // `AssertUnwindSafe` is needed for the `&mut S`, and is not
                // `unsafe`. The residual risk is real and worth naming: a
                // searcher that panics may have left its own state (from step 3,
                // a transposition table) inconsistent. `usinewgame` clears it,
                // and one bad game beats a dead engine.
                let best = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    searcher.search(&job, sink)
                }))
                .unwrap_or_else(|_| {
                    eprintln!("rinsai: the search panicked; answering `resign` for this move");
                    BestMove::Resign
                });

                // USI forbids answering a ponder search before `ponderhit` or
                // `stop`, and that is a *protocol* obligation, not a search one:
                // a real search returns as soon as it hits its depth or node
                // limit, which during ponder would be a `bestmove` the GUI never
                // asked for. Holding it here makes every future searcher correct
                // by construction rather than by remembering.
                if job.limits.ponder {
                    while !job.signals.ponder_hit() && !job.signals.stopped() {
                        job.signals.wait_until_signalled();
                    }
                }

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

    /// A panicking search must still answer, and must not take the worker with
    /// it.
    ///
    /// Without the `catch_unwind` the thread dies with the loop: the answer
    /// never comes, every later job is sent into a closed channel, and the
    /// engine goes on answering `usi` and `isready` while never moving again.
    /// From a match harness that reads as a clean exit after a game lost on
    /// time — the worst shape a failure can take. Sabotage: remove the
    /// `catch_unwind` and this test hangs on the second job instead.
    ///
    /// (The panic message on stderr during this test is expected.)
    #[test]
    fn a_panicking_search_resigns_and_the_worker_survives() {
        struct Bomb;
        impl Searcher for Bomb {
            fn search(&mut self, job: &SearchJob, _out: &dyn InfoSink) -> BestMove {
                assert!(job.id != 1, "AUDIT: injected search panic");
                BestMove::Resign
            }
        }

        let answers = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&answers);
        let mut driver = SearchDriver::spawn(Bomb, Arc::new(SilentSink), move |best| {
            recorded
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(best.to_string());
        });
        driver.submit(job(1, Limits::default()));
        driver.submit(job(2, Limits::default()));
        driver.shutdown();

        // One answer per job, the first of them from the panic path.
        assert_eq!(
            *answers.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["resign", "resign"]
        );
    }

    /// Dropping the driver with a search in flight must not block forever.
    ///
    /// `shutdown` joins the worker, and a `go infinite` never ends on its own,
    /// so a `join` without a `stop` waits for something that will not happen.
    /// Sabotage: remove the `stop` from `shutdown` and this times out.
    #[test]
    fn dropping_the_driver_stops_the_search_it_is_about_to_wait_for() {
        struct Waiter;
        impl Searcher for Waiter {
            fn search(&mut self, job: &SearchJob, _out: &dyn InfoSink) -> BestMove {
                while !job.signals.stopped() {
                    job.signals.wait_until_signalled();
                }
                BestMove::Resign
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let driver = SearchDriver::spawn(Waiter, Arc::new(SilentSink), |_| {});
            driver.submit(job(
                1,
                Limits {
                    infinite: true,
                    ..Limits::default()
                },
            ));
            drop(driver);
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "dropping the driver blocked on a search that never ends"
        );
    }

    /// USI forbids answering a ponder search before `ponderhit` or `stop`, and
    /// the driver holds that rather than each searcher: step 2's search returns
    /// as soon as it hits its depth or node limit, which during ponder would be
    /// a `bestmove` the GUI never asked for. Sabotage: remove the ponder wait
    /// from `worker` and the answer arrives before `ponderhit`.
    #[test]
    fn a_ponder_search_that_returns_early_still_waits_to_answer() {
        struct Immediate;
        impl Searcher for Immediate {
            fn search(&mut self, _job: &SearchJob, _out: &dyn InfoSink) -> BestMove {
                BestMove::Resign
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let signals = Arc::new(SearchSignals::new());
        let driver = SearchDriver::spawn(Immediate, Arc::new(SilentSink), move |_| {
            let _ = tx.send(());
        });
        driver.submit(SearchJob {
            id: 1,
            game: Game::from_startpos(),
            limits: Limits {
                ponder: true,
                ..Limits::default()
            },
            signals: Arc::clone(&signals),
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "answered a ponder search before `ponderhit`"
        );
        signals.ponderhit();
        assert!(
            rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "did not answer once `ponderhit` arrived"
        );
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
