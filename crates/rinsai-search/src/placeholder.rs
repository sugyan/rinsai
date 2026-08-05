//! The E0 step 1 stand-in for a search.
//!
//! It exists so the protocol layer has something real to drive, and it is
//! **deleted at step 2** when material evaluation and alpha-beta arrive. Its
//! only jobs are to answer with a legal move and to respect `go infinite` and
//! `go ponder`, because an engine that answers those immediately is visibly
//! broken to a GUI whether or not it can search.

use core::ops::ControlFlow;

use crate::search::{BestMove, InfoSink, SearchJob, Searcher};

/// Answers with a legal move, chosen without any search at all.
#[derive(Clone, Copy, Debug, Default)]
pub struct PlaceholderSearcher;

impl Searcher for PlaceholderSearcher {
    fn search(&mut self, job: &SearchJob, out: &dyn InfoSink) -> BestMove {
        let position = job.game.position();

        // The first move the generator offers. shunsai's public documentation
        // says nothing about the order moves are generated in, so *which* move
        // this is is an unspecified implementation detail that may change with
        // any revision — nothing may depend on it, and no test may name it.
        let mut first = None;
        let _ = position.generate_moves(|set| {
            first = set.into_iter().next();
            ControlFlow::Break(())
        });

        let best = match first {
            Some(mv) => BestMove::Play { mv, ponder: None },
            None => BestMove::Resign,
        };
        out.info("info string rinsai has no search yet (E0 step 1): playing a legal move");

        // `infinite` means "keep searching until told to stop", which is the
        // searcher's own business. `ponder` is *not* handled here: when a
        // ponder search may answer is a protocol rule, and the driver holds it
        // for every searcher (see `search::worker`).
        while job.limits.infinite && !job.signals.stopped() && !job.signals.ponder_hit() {
            job.signals.wait_until_signalled();
        }

        best
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use shogi_core::PartialPosition;
    use shogi_usi_parser::FromUsi;

    use super::*;
    use crate::game::Game;
    use crate::moves::is_legal;
    use crate::search::{Limits, SearchJob, SearchSignals, SilentSink};

    fn job(game: Game, limits: Limits) -> SearchJob {
        SearchJob {
            id: 0,
            game,
            limits,
            signals: Arc::new(SearchSignals::new()),
        }
    }

    #[test]
    fn answers_with_a_legal_move() {
        let game = Game::from_startpos();
        let job = job(game.clone(), Limits::default());
        let best = PlaceholderSearcher.search(&job, &SilentSink);

        match best {
            BestMove::Play { mv, ponder } => {
                assert!(is_legal(game.position(), mv));
                assert_eq!(ponder, None);
            }
            BestMove::Resign => panic!("the initial position has 30 legal moves"),
        }
    }

    /// A lone king with nothing to move and no hand has no legal move at all —
    /// no mate geometry needed to construct it.
    #[test]
    fn resigns_when_there_is_no_legal_move() {
        let partial = PartialPosition::from_usi("sfen 4k4/9/9/9/9/9/9/9/9 b - 1")
            .expect("the fixture parses");
        let job = job(
            Game::from_partial(partial).expect("the fixture is a valid position"),
            Limits::default(),
        );
        assert_eq!(
            PlaceholderSearcher.search(&job, &SilentSink),
            BestMove::Resign
        );
    }

    /// Waiting for `stop` is the one behaviour this stand-in exists to get
    /// right, so it needs a test that can actually fail.
    ///
    /// The obvious shape — spawn, `stop()`, join, assert a move came back —
    /// **cannot**: a searcher that ignores `infinite` entirely returns the same
    /// move just as happily, so the assertion holds either way. The property is
    /// "it has *not* answered yet", which is a negative and cannot be proved by
    /// a test. What can be done is to give the wrong behaviour a generous
    /// window to show itself in: a searcher that answers without being told
    /// does so in microseconds, so nothing arriving in 200 ms is decisive in
    /// practice. Sabotage: delete the wait loop and `recv_timeout` below
    /// succeeds where it must time out.
    fn waits_until_signalled(limits: Limits, release: impl FnOnce(&SearchSignals)) {
        use std::sync::mpsc;
        use std::time::Duration;

        let job = Arc::new(job(Game::from_startpos(), limits));
        let signals = Arc::clone(&job.signals);
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(PlaceholderSearcher.search(&job, &SilentSink));
        });

        assert!(
            rx.recv_timeout(Duration::from_millis(200)).is_err(),
            "the search answered without being told to"
        );
        release(&signals);
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(10))
                .expect("the search answers once released"),
            BestMove::Play { .. }
        ));
    }

    #[test]
    fn go_infinite_waits_for_stop() {
        waits_until_signalled(
            Limits {
                infinite: true,
                ..Limits::default()
            },
            SearchSignals::stop,
        );
    }

    /// A searcher does **not** hold a ponder answer back — the driver does.
    ///
    /// The split is deliberate. `infinite` is about when a search stops
    /// searching, which is the searcher's own business; `ponder` is about when
    /// an answer may be *sent*, which is a protocol rule, and holding it in the
    /// driver makes every future searcher correct without remembering. Step 2's
    /// search will return the moment it hits its depth or node limit, and that
    /// must not become a `bestmove` the GUI never asked for — the test for that
    /// is `search::tests::a_ponder_search_that_returns_early_still_waits_to_answer`.
    #[test]
    fn go_ponder_is_not_the_searchers_business() {
        let job = job(
            Game::from_startpos(),
            Limits {
                ponder: true,
                ..Limits::default()
            },
        );
        assert!(matches!(
            PlaceholderSearcher.search(&job, &SilentSink),
            BestMove::Play { .. }
        ));
    }

    /// The converse, and the one that keeps the pair honest: an ordinary `go`
    /// must **not** wait. Without this, "wait for everything" would pass the
    /// two tests above and hang every real game.
    #[test]
    fn an_ordinary_go_answers_immediately() {
        let job = job(Game::from_startpos(), Limits::default());
        assert!(matches!(
            PlaceholderSearcher.search(&job, &SilentSink),
            BestMove::Play { .. }
        ));
    }
}
