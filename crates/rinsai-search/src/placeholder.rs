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

        // The first move the generator offers. Note that shunsai's DESIGN.md:64
        // explicitly refuses to guarantee generation order, so *which* move this
        // is may change with any shunsai revision — nothing may depend on it,
        // and no test may name it.
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

        // `infinite` and `ponder` both mean "do not answer until told to".
        while (job.limits.infinite || job.limits.ponder)
            && !job.signals.stopped()
            && !job.signals.ponder_hit()
        {
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
        let job = job(Game::from_partial(partial), Limits::default());
        assert_eq!(
            PlaceholderSearcher.search(&job, &SilentSink),
            BestMove::Resign
        );
    }

    /// Sabotage: drop the wait loop and this returns at once, which is exactly
    /// the bug an analysis GUI sees as a broken engine.
    #[test]
    fn go_infinite_waits_for_stop() {
        let job = Arc::new(job(
            Game::from_startpos(),
            Limits {
                infinite: true,
                ..Limits::default()
            },
        ));
        let signals = Arc::clone(&job.signals);

        let searching = std::thread::spawn(move || PlaceholderSearcher.search(&job, &SilentSink));
        // The searcher cannot have returned yet; `stop` is what lets it.
        signals.stop();
        assert!(matches!(
            searching.join().expect("the search thread finished"),
            BestMove::Play { .. }
        ));
    }

    #[test]
    fn go_ponder_waits_for_ponderhit() {
        let job = Arc::new(job(
            Game::from_startpos(),
            Limits {
                ponder: true,
                ..Limits::default()
            },
        ));
        let signals = Arc::clone(&job.signals);

        let searching = std::thread::spawn(move || PlaceholderSearcher.search(&job, &SilentSink));
        signals.ponderhit();
        assert!(matches!(
            searching.join().expect("the search thread finished"),
            BestMove::Play { .. }
        ));
    }
}
