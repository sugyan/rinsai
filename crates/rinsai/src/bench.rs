//! `rinsai bench` — fixed positions at a fixed depth, reporting node counts.
//!
//! The search analogue of perft: **a patch that moves a count without meaning
//! to is a bug rather than an improvement**. The times beside them are for the
//! operator; nothing depends on them.
//!
//! **Everything that could move a count is fixed here, not inherited** — the
//! position set, the depth, and the table size.

// No protocol is running and the process exits when this returns — the same
// standing as `main`'s `--version`.
#![allow(clippy::print_stdout)]

use std::time::{Duration, Instant};

use rinsai_search::{
    Game, Limits, NegamaxSearcher, SearchJob, SearchSignals, Searcher, SilentSink,
};

/// Compiled in rather than read at runtime, so a count cannot depend on the
/// working directory the engine was launched from.
const POSITIONS: &str = include_str!("../../../positions/bench-v1.sfen");

/// The depth the frozen counts were taken at, and what `rinsai bench` with no
/// argument runs.
pub const BENCH_DEPTH: u32 = 4;

/// The table `bench` runs with, whatever `USI_Hash` says. ⚠️ Table size changes
/// node counts on its own, so a baseline at the operator's setting is one
/// nobody else can reproduce.
const BENCH_HASH_MB: usize = 16;

/// What every position is expected to cost at [`BENCH_DEPTH`], in order.
///
/// ⚠️ **A mismatch here is a finding, not a maintenance chore**: updating this
/// table is the last step of answering "which patch moved it", not the first.
/// The counts are at `BENCH_HASH_MB` and at no other table size.
const EXPECTED: &[u64] = &[
    3_525,   // startpos
    6_497,   // startpos moves 7g7f 3c3d
    4_194,   // startpos moves 2g2f 8c8d 2f2e 8d8e
    8_292,   // startpos moves 7g7f 3c3d 2g2f 4c4d 2f2e 2b3c
    249_462, // matsuri, the drop-heavy middlegame
    175,     // two lone kings
    99,      // 頭金
];

/// One position's result.
struct Row {
    args: String,
    nodes: u64,
    elapsed: Duration,
}

/// The position arguments in the committed set, comments and blanks dropped.
fn positions() -> Vec<&'static str> {
    POSITIONS
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Runs the set at `depth` and returns a row per position.
///
/// ⚠️ **`new_game` between positions, and it is not tidiness**: the table
/// survives a search by design, so without it the counts would depend on the
/// order of the file.
fn measure(depth: u32) -> Result<Vec<Row>, String> {
    let mut searcher = NegamaxSearcher::with_hash_mb(BENCH_HASH_MB);
    let mut rows = Vec::new();

    for args in positions() {
        let game = Game::from_usi_position(args).map_err(|e| format!("`{args}`: {e}"))?;
        searcher.new_game();

        let job = SearchJob {
            id: rows.len() as u64,
            game,
            limits: Limits {
                depth: Some(depth),
                ..Limits::default()
            },
            signals: std::sync::Arc::new(SearchSignals::new()),
        };
        let started = Instant::now();
        let _ = searcher.search(&job, &SilentSink);
        rows.push(Row {
            args: args.to_owned(),
            nodes: searcher.nodes(),
            elapsed: started.elapsed(),
        });
    }
    Ok(rows)
}

/// Runs the bench and prints it. `true` if everything a baseline exists for
/// matched — including at a depth with no baseline, where there is nothing to
/// disagree with.
#[must_use]
pub fn run(depth: u32) -> bool {
    let rows = match measure(depth) {
        Ok(rows) => rows,
        Err(e) => {
            eprintln!("rinsai: bench: the committed position set does not parse: {e}");
            return false;
        }
    };

    let verifying = depth == BENCH_DEPTH;
    println!(
        "rinsai bench — depth {depth}, hash {BENCH_HASH_MB} MiB, {} positions",
        rows.len()
    );
    if !verifying {
        println!("(no frozen counts at this depth; the baseline is depth {BENCH_DEPTH})");
    }
    println!();

    let mut ok = true;
    for (i, row) in rows.iter().enumerate() {
        let verdict = match (verifying, EXPECTED.get(i)) {
            (true, Some(&want)) if want == row.nodes => "ok".to_owned(),
            (true, Some(&want)) => {
                ok = false;
                format!("MISMATCH, expected {want}")
            }
            // A position with no baseline beside it means the set and the table
            // have drifted apart, which is itself the failure.
            (true, None) => {
                ok = false;
                "MISSING BASELINE".to_owned()
            }
            (false, _) => String::new(),
        };
        println!(
            "{:>2}  {:>12} nodes  {:>7} ms  {}  {}",
            i + 1,
            row.nodes,
            row.elapsed.as_millis(),
            row.args,
            verdict
        );
    }

    if verifying && EXPECTED.len() != rows.len() {
        ok = false;
        eprintln!(
            "rinsai: bench: {} positions against {} frozen counts",
            rows.len(),
            EXPECTED.len()
        );
    }

    let nodes: u64 = rows.iter().map(|row| row.nodes).sum();
    let elapsed: Duration = rows.iter().map(|row| row.elapsed).sum();
    let nps = rinsai_search::nps(nodes, elapsed);
    println!();
    println!(
        "total {nodes} nodes in {} ms ({nps} nps)",
        elapsed.as_millis()
    );
    if verifying {
        println!("{}", if ok { "counts match" } else { "COUNTS MOVED" });
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The regression test proper: the committed counts are what the search
    /// produces today.
    ///
    /// **It goes red for almost any search change, and that is its purpose.**
    /// When it fires, find out what moved before touching [`EXPECTED`].
    #[test]
    fn the_frozen_counts_are_what_the_search_produces() {
        let rows = measure(BENCH_DEPTH).expect("the committed set parses");
        let got: Vec<u64> = rows.iter().map(|row| row.nodes).collect();
        assert_eq!(got, EXPECTED, "the bench counts moved");
    }

    /// …and it reproduces. The counts are a regression test only because the
    /// search is deterministic, and nothing else in the suite says so.
    ///
    /// ⚠️ **No sabotage note, because none of the obvious mutations reach it.**
    /// `measure` builds a fresh searcher per call, so anything that changes both
    /// runs alike — dropping the `new_game`, resizing the table — leaves the two
    /// vectors equal; that one is caught by the frozen counts above. What this
    /// covers is a search seeded from something outside its inputs.
    #[test]
    fn the_bench_is_deterministic() {
        let first: Vec<u64> = measure(3)
            .expect("parses")
            .iter()
            .map(|r| r.nodes)
            .collect();
        let second: Vec<u64> = measure(3)
            .expect("parses")
            .iter()
            .map(|r| r.nodes)
            .collect();
        assert_eq!(first, second);
    }

    /// The set is what the file says, and the table beside it has not drifted.
    #[test]
    fn every_position_has_a_frozen_count_and_parses() {
        let set = positions();
        assert_eq!(set.len(), EXPECTED.len(), "{set:?}");
        for args in set {
            Game::from_usi_position(args).unwrap_or_else(|e| panic!("`{args}`: {e}"));
        }
    }
}
