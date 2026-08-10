//! The `info` line a search sends while it is thinking.
//!
//! Apart from the search so the wire format has tests that do not need a
//! search to run, and apart from `search.rs` so that seam stays
//! protocol-agnostic.

use core::fmt;
use core::time::Duration;

use shogi_core::{Move, ToUsi};

use crate::score::{Depth, Score};

/// One iteration's worth of progress.
///
/// The fields are the ones a search at E0 step 3a can honestly fill.
/// **Deliberately absent**: `hashfull`, which needs the transposition table
/// (step 3b), and `multipv` / `currmove`, which have no consumer at all.
#[derive(Clone, Copy, Debug)]
pub(crate) struct SearchInfo<'a> {
    pub(crate) depth: Depth,
    /// The deepest ply this *iteration* reached, quiescence included.
    pub(crate) seldepth: usize,
    pub(crate) score: Score,
    pub(crate) nodes: u64,
    pub(crate) elapsed: Duration,
    /// The principal variation, best move first. May be empty — a search
    /// stopped before it finished a single root move has nothing to show.
    pub(crate) pv: &'a [Move],
}

impl fmt::Display for SearchInfo<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // `seldepth` sits immediately after `depth`, where a GUI expects it;
        // `hashfull` joins before `score` at step 3b. ⚠️ Every field is printed
        // unconditionally, including when uninteresting: a token that comes and
        // goes makes the line's shape depend on the position.
        write!(
            f,
            "info depth {} seldepth {} time {} nodes {} nps {}",
            self.depth,
            self.seldepth,
            self.elapsed.as_millis(),
            self.nodes,
            self.nps(),
        )?;

        // USI reports mate distance in **plies**, which is exactly what
        // `mate_plies` returns — positive when the side to move mates.
        //
        // ⚠️ The two sentinels are excluded rather than trusted to stay out:
        // both sit above the mate floor, so `INFINITE` prints as
        // `score mate -1` and `NONE` as `score mate -2` — the engine announcing
        // it is being mated next move, which a GUI believes.
        //
        // An assertion inside a `Display` makes a formatter that can panic. It
        // earns the place by being a `debug_assert` on a genuine bug rather
        // than on a caller's input: no sentinel is a caller's to print.
        debug_assert!(
            !matches!(self.score, Score::INFINITE | Score::NONE),
            "a sentinel reached the wire: {:?}",
            self.score
        );
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
    /// ⚠️ From nanoseconds, not the millisecond figure printed beside it: the
    /// first iterations routinely finish inside one millisecond, and the
    /// rounded-down count would be zero on exactly those.
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
            seldepth: 11,
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
            "info depth 7 seldepth 11 time 200 nodes 123456 nps 617280 score cp -42 pv 7g7f S*5b"
        );
    }

    /// Sabotage: make the `seldepth` token conditional on it exceeding `depth`
    /// and this fires.
    #[test]
    fn seldepth_is_printed_even_when_it_equals_depth() {
        let line = SearchInfo {
            depth: 4,
            seldepth: 4,
            score: Score::ZERO,
            nodes: 1,
            elapsed: Duration::from_millis(1),
            pv: &[],
        }
        .to_string();
        assert!(line.starts_with("info depth 4 seldepth 4 time "), "{line}");
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
        // A substring check, and it stays sound only while no other token
        // contains "pv" — re-checked when `seldepth` joined the line, and to be
        // re-checked again when `hashfull` does at step 3b.
        assert!(!line.contains("pv"), "{line}");
    }

    /// The first iteration of a real search finishes well inside a
    /// millisecond, so this is the ordinary case rather than a corner one.
    #[test]
    fn no_elapsed_time_does_not_divide_by_zero() {
        let line = SearchInfo {
            depth: 1,
            seldepth: 1,
            score: Score::ZERO,
            nodes: 31,
            elapsed: Duration::ZERO,
            pv: &[],
        }
        .to_string();
        assert_eq!(
            line,
            "info depth 1 seldepth 1 time 0 nodes 31 nps 0 score cp 0"
        );
    }
}
