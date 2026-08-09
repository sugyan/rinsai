//! The seam between a protocol and a search.
//!
//! Nothing here knows what USI is: a [`Searcher`] is handed a [`SearchJob`] and
//! returns a [`BestMove`], and the caller — the engine binary today, a
//! self-play driver at E3 — decides what to do with it. The shapes are chosen
//! so that steps 2 to 5 add behaviour without changing a signature.

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use shogi_core::Move;

use crate::game::Game;

/// What a `go` command asked for.
///
/// Every field is parsed, and most are ignored until later steps, so the
/// grammar and its conformance tests are written once.
///
/// ⚠️ The deadline is deliberately *not* part of this: time management extends
/// on a fail-low and cuts short on an obvious move, so the search has to own
/// the clock and see the raw parameters.
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
/// Atomics rather than a channel, because E2's Lazy SMP broadcasts one `Arc`
/// to every thread where a channel would need one per thread.
///
/// ⚠️ A **fresh set is created per `go`**, so a `stop` that arrives late cannot
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

    /// The hot-path poll. Relaxed is correct: this only gates "abandon and
    /// return", and the handoff is ordered by the output mutex and the channel.
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
/// A callback rather than a channel: the protocol thread is blocked reading
/// stdin and could not pump a receiver, and a channel cannot carry a borrowed
/// principal variation without allocating per event.
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
/// must survive between searches lives in the implementor, which the worker
/// thread owns for its whole life.
pub trait Searcher: Send {
    /// Runs one search, returning promptly once `job.signals.stopped()`.
    ///
    /// ⚠️ It **returns** a [`BestMove`] rather than printing one. That is what
    /// makes "exactly one `bestmove` per `go`" structural rather than a rule
    /// every code path has to remember.
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
    /// The signals of every job submitted and not yet finished, so [`Drop`] can
    /// stop all of what it is about to wait for.
    ///
    /// ⚠️ **Every** rather than the most recent: the worker is a single FIFO,
    /// so a second `go` queues behind the first, and remembering one set would
    /// stop the job that has not started while joining on the one running.
    ///
    /// ⚠️ `Weak`, not `Arc`. This field only *observes*; an `Arc` would keep
    /// every set alive until the next `submit` swept it, and a dead entry that
    /// still answers `stop()` is indistinguishable from a live one.
    outstanding: Mutex<Vec<Weak<SearchSignals>>>,
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
            outstanding: Mutex::new(Vec::new()),
        }
    }

    /// Queues a search. **Returns `false` if it could not be queued**, which
    /// means the worker is gone.
    ///
    /// The caller has already committed to one `bestmove` for this `go` and a
    /// dropped job produces none, so it has to be able to answer anyway.
    #[must_use]
    pub fn submit(&self, job: SearchJob) -> bool {
        {
            let mut outstanding = self.outstanding.lock().unwrap_or_else(|e| e.into_inner());
            // Housekeeping, not correctness: keeps the set proportional to
            // searches in flight rather than to searches ever run.
            outstanding.retain(|signals| signals.strong_count() > 0);
            outstanding.push(Arc::downgrade(&job.signals));
        }
        match &self.tx {
            Some(tx) => tx.send(Command::Go(Box::new(job))).is_ok(),
            None => false,
        }
    }

    /// Queues a `usinewgame`, in order with the searches around it. `false` if
    /// the worker is gone — nothing is owed to the GUI for one of these, so the
    /// caller may simply log it.
    pub fn new_game(&self) -> bool {
        match &self.tx {
            Some(tx) => tx.send(Command::NewGame).is_ok(),
            None => false,
        }
    }

    /// Stops every search that has not yet answered, then waits for the worker
    /// to drain the queue and exit.
    ///
    /// ⚠️ Raising `stop` here is what keeps [`Drop`] from blocking forever: a
    /// `go infinite` never ends on its own, so a `join` without a `stop` waits
    /// for something that will not happen. `stop` is idempotent.
    ///
    /// ⚠️ Closing the channel does **not** discard what is queued — `mpsc`
    /// delivers messages sent before the disconnect — so a job behind the
    /// running one still runs and still answers. Intended: it is what keeps
    /// "one `bestmove` per accepted `go`" true across a shutdown.
    pub fn shutdown(&mut self) {
        for signals in self
            .outstanding
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
        {
            // `None` is a job that already answered and was dropped — there is
            // nothing left to stop, which is the outcome `stop()` would have
            // produced anyway.
            if let Some(signals) = signals.upgrade() {
                signals.stop();
            }
        }
        drop(self.tx.take());
        if let Some(handle) = self.handle.take() {
            // An `Err` is the worker having panicked. `worker` catches the
            // searcher's own panics, so reaching this means the driver's loop
            // failed — report it rather than swallowing it.
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
        // ⚠️ Two catches, two different guarantees, both needed. The outer one
        // keeps the *worker* alive; the inner one below keeps the *job*
        // answered. Losing either ends the thread with this loop, after which
        // every later job goes into a closed channel and the engine answers the
        // handshake forever while never moving again — which, from a match
        // harness, reads as a clean exit after a game lost on time.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            match command {
                Command::NewGame => searcher.new_game(),
                Command::Go(job) => {
                    // The inner catch: "one answer per job" when a search goes
                    // wrong. Resigning beats hanging, and the panic message is
                    // already on stderr.
                    //
                    // ⚠️ `AssertUnwindSafe` (needed for the `&mut S`, not
                    // `unsafe`) carries a real residual risk: a searcher that
                    // panics may have left its own state inconsistent.
                    // `usinewgame` clears it, and one bad game beats a dead
                    // engine.
                    let best = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        searcher.search(&job, sink)
                    }))
                    .unwrap_or_else(|_| {
                        eprintln!("rinsai: the search panicked; answering `resign` for this move");
                        BestMove::Resign
                    });

                    // USI forbids answering a ponder search before `ponderhit`
                    // or `stop`. A *protocol* obligation, not a search one, so
                    // holding it here makes every future searcher correct by
                    // construction rather than by remembering.
                    if job.limits.ponder {
                        while !job.signals.ponder_hit() && !job.signals.stopped() {
                            job.signals.wait_until_signalled();
                        }
                    }

                    emit(best);
                }
            }
        }));
        if outcome.is_err() {
            // The emit closure lowered the counter before the write, so the
            // protocol layer is not left believing a search is still running.
            eprintln!("rinsai: the search thread recovered from a panic outside the search");
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
            assert!(driver.submit(job(
                id,
                Limits {
                    depth: Some(id as u32),
                    ..Limits::default()
                },
            )));
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
    /// Sabotage: remove the inner `catch_unwind` and this hangs on the second
    /// job instead. See `worker` for what that failure looks like from outside.
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
        assert!(driver.submit(job(1, Limits::default())));
        assert!(driver.submit(job(2, Limits::default())));
        driver.shutdown();

        // One answer per job, the first of them from the panic path.
        assert_eq!(
            *answers.lock().unwrap_or_else(|e| e.into_inner()),
            vec!["resign", "resign"]
        );
    }

    /// A panic *outside* the searcher — here in `new_game`, at step 3b a
    /// transposition table being cleared — must not take the worker with it.
    ///
    /// The inner `catch_unwind` only covers `search`. Sabotage: remove the
    /// outer catch and the `go` below is never answered.
    ///
    /// (The panic message on stderr during this test is expected.)
    #[test]
    fn a_panic_outside_the_search_does_not_kill_the_worker() {
        struct BadNewGame;
        impl Searcher for BadNewGame {
            fn search(&mut self, _job: &SearchJob, _out: &dyn InfoSink) -> BestMove {
                BestMove::Resign
            }
            fn new_game(&mut self) {
                panic!("AUDIT: injected new_game panic");
            }
        }

        let (tx, rx) = std::sync::mpsc::channel();
        let mut driver = SearchDriver::spawn(BadNewGame, Arc::new(SilentSink), move |_| {
            let _ = tx.send(());
        });
        assert!(driver.new_game());
        assert!(driver.submit(job(1, Limits::default())));
        assert!(
            rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "the worker died with `new_game` and never answered the `go`"
        );
        driver.shutdown();
    }

    /// Once the worker is gone, `submit` must say so rather than swallowing the
    /// job — the caller has already promised the GUI an answer.
    #[test]
    fn submit_reports_that_a_gone_worker_took_no_job() {
        struct Idle;
        impl Searcher for Idle {
            fn search(&mut self, _job: &SearchJob, _out: &dyn InfoSink) -> BestMove {
                BestMove::Resign
            }
        }

        let mut driver = SearchDriver::spawn(Idle, Arc::new(SilentSink), |_| {});
        assert!(driver.submit(job(1, Limits::default())));
        driver.shutdown();
        assert!(
            !driver.submit(job(2, Limits::default())),
            "reported success after the worker was gone"
        );
        assert!(!driver.new_game());
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
            assert!(driver.submit(job(
                1,
                Limits {
                    infinite: true,
                    ..Limits::default()
                },
            )));
            drop(driver);
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "dropping the driver blocked on a search that never ends"
        );
    }

    /// …and it must stop **every** search that has not answered, not just the
    /// last one submitted.
    ///
    /// A driver remembering only the most recent job's signals raises `stop` on
    /// the one that has not started and joins on the one that is running —
    /// which, for a search that ends only on `stop`, never returns. ⚠️
    /// `usi::run` cannot reach this because `Engine::go` stops the previous
    /// search first, but [`SearchDriver`] is public API and E3's self-play
    /// driver is its second caller.
    ///
    /// Sabotage: replace the sweep in `submit` with `outstanding.clear()` and
    /// this times out.
    #[test]
    fn shutdown_stops_every_outstanding_search_not_only_the_last() {
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
            let mut driver = SearchDriver::spawn(Waiter, Arc::new(SilentSink), |_| {});
            let infinite = Limits {
                infinite: true,
                ..Limits::default()
            };
            // The first runs; the second queues behind it.
            assert!(driver.submit(job(1, infinite)));
            assert!(driver.submit(job(2, infinite)));
            driver.shutdown();
            let _ = tx.send(());
        });
        assert!(
            rx.recv_timeout(Duration::from_secs(10)).is_ok(),
            "shutdown blocked on a search it never stopped"
        );
    }

    /// USI forbids answering a ponder search before `ponderhit` or `stop`, and
    /// the driver holds that rather than each searcher. Sabotage: remove the
    /// ponder wait from `worker` and the answer arrives before `ponderhit`.
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
        assert!(driver.submit(SearchJob {
            id: 1,
            game: Game::from_startpos(),
            limits: Limits {
                ponder: true,
                ..Limits::default()
            },
            signals: Arc::clone(&signals),
        }));

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
