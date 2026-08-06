//! The `info` line a search sends while it is thinking.
//!
//! Kept apart from the search so that the wire format has tests that do not
//! need a search to run, and apart from `search.rs` so that the seam stays
//! protocol-agnostic. The search builds one of these per completed iteration
//! and hands its `Display` to the [`InfoSink`](crate::search::InfoSink); it is
//! the sink's owner — the USI layer, or nobody — that decides where the text
//! goes.

use core::fmt;
use core::time::Duration;

use shogi_core::{Move, ToUsi};

use crate::score::{Depth, Score};

/// One iteration's worth of progress.
///
/// The fields are the ones a search at E0 step 2 can honestly fill.
/// **Deliberately absent**: `seldepth`, which without quiescence or extensions
/// would always equal `depth` and so would be a constant dressed as data
/// (step 3 gives it a meaning); `hashfull`, which needs the transposition table
/// (step 3); and `multipv` / `currmove`, which have no consumer at all.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchInfo<'a> {
    pub(crate) depth: Depth,
    pub(crate) score: Score,
    pub(crate) nodes: u64,
    pub(crate) elapsed: Duration,
    /// The principal variation, best move first. May be empty — a search
    /// stopped before it finished a single root move has nothing to show.
    pub(crate) pv: &'a [Move],
}

impl fmt::Display for SearchInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "info depth {} time {} nodes {} nps {}",
            self.depth,
            self.elapsed.as_millis(),
            self.nodes,
            self.nps(),
        )?;

        // USI reports mate distance in **plies**, which is exactly what
        // `mate_plies` returns — positive when the side to move mates.
        match self.score.mate_plies() {
            Some(plies) => write!(f, " score mate {plies}")?,
            None => write!(f, " score cp {}", self.score.get())?,
        }

        // The token is omitted entirely rather than written empty: `pv` with
        // nothing after it is not something a GUI has to accept.
        if !self.pv.is_empty() {
            f.write_str(" pv")?;
            for mv in self.pv {
                write!(f, " {}", mv.to_usi_owned())?;
            }
        }
        Ok(())
    }
}

impl SearchInfo<'_> {
    /// Nodes per second, or zero when no time has passed.
    ///
    /// Computed from nanoseconds rather than the millisecond figure printed
    /// beside it: the first iterations of a search routinely finish inside one
    /// millisecond, and dividing by the rounded-down millisecond count would
    /// divide by zero on exactly those.
    fn nps(&self) -> u128 {
        match self.elapsed.as_nanos() {
            0 => 0,
            nanos => u128::from(self.nodes) * 1_000_000_000 / nanos,
        }
    }
}

#[cfg(test)]
mod tests {
    use shogi_core::{Piece, Square};

    use super::*;

    /// Naming moves is fine here, and only here: the rule is that no test may
    /// name a move *the engine chose*, and every move below was built by the
    /// test itself.
    fn pv() -> Vec<Move> {
        vec![
            Move::Normal {
                from: Square::SQ_7G,
                to: Square::SQ_7F,
                promote: false,
            },
            Move::Drop {
                piece: Piece::B_S,
                to: Square::SQ_5B,
            },
        ]
    }

    fn info(score: Score, pv: &[Move]) -> String {
        SearchInfo {
            depth: 7,
            score,
            nodes: 123_456,
            elapsed: Duration::from_millis(200),
            pv,
        }
        .to_string()
    }

    #[test]
    fn an_info_line_reads_as_usi() {
        assert_eq!(
            info(Score::cp(-42), &pv()),
            "info depth 7 time 200 nodes 123456 nps 617280 score cp -42 pv 7g7f S*5b"
        );
    }

    #[test]
    fn a_mate_score_is_reported_in_plies() {
        assert!(info(Score::mate_in(3), &pv()).contains(" score mate 3 "));
        assert!(info(Score::mated_in(3), &pv()).contains(" score mate -3 "));
    }

    #[test]
    fn an_empty_pv_omits_the_pv_token() {
        let line = info(Score::ZERO, &[]);
        assert!(line.ends_with(" score cp 0"), "{line}");
        assert!(!line.contains("pv"), "{line}");
    }

    /// The first iteration of a real search finishes well inside a
    /// millisecond, so this is the ordinary case rather than a corner one.
    #[test]
    fn no_elapsed_time_does_not_divide_by_zero() {
        let line = SearchInfo {
            depth: 1,
            score: Score::ZERO,
            nodes: 31,
            elapsed: Duration::ZERO,
            pv: &[],
        }
        .to_string();
        assert_eq!(line, "info depth 1 time 0 nodes 31 nps 0 score cp 0");
    }
}
