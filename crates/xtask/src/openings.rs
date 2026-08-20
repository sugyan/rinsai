//! `gen-openings` — from cached floodgate records to a frozen opening set.
//!
//! The pipeline is a pure function of its inputs, the frozen constants and
//! the seed, so the emitted file is byte-reproducible; the header it writes
//! interpolates those constants and the run's own counters, so the file's
//! provenance cannot drift from the code that produced it.

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::ops::RangeInclusive;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use rinsai_game::{Game, position_key};
use rinsai_search::{
    BestMove, InfoSink, Limits, NegamaxSearcher, SearchJob, SearchSignals, Searcher,
};
use shogi_core::{Color, Move, Piece, Square, ToUsi};

use crate::csa::{self, CsaMove};
use crate::fetch;
use crate::rng;

/// A frozen set's constants. ⚠️ Changing any field after a set is frozen
/// means a new file, not a regeneration — and so does changing the pipeline
/// around it, which is what took v1 to v2. [`generate`]'s other two arguments
/// move the output as well, so they are not a lesser kind of input: the days
/// choose the corpus, and the rev is interpolated into the header.
///
/// ⚠️ **The engine's own search is an input too, and it is the one no
/// argument here names.** The balance filter judges with it, so a patch that
/// makes it cheaper moves how far [`PipelineConfig::balance_node_cap`]
/// reaches and with it the depth a candidate's verdict comes from. That is
/// what makes a set reproducible only from the rev its header records.
#[derive(Debug, Clone)]
pub struct PipelineConfig {
    /// Both players' floodgate rates must reach this.
    pub min_rate: f64,
    /// Whole-game length floor, in plies, before any replay.
    pub min_game_plies: usize,
    /// Plies whose positions are opening candidates.
    pub window: RangeInclusive<usize>,
    /// Openings taken from one source game, at most.
    pub max_per_game: usize,
    /// Lines in the emitted file — a shortfall is an error, never a shorter
    /// file.
    pub target: usize,
    /// Depth of the balance search.
    pub balance_depth: u32,
    /// Node cap on the balance search. The engine's quiescence is unpruned, so
    /// an open position can cost orders more than the depth suggests.
    ///
    /// ⚠️ **A cap tight enough to stop the first iteration decides *whether* a
    /// candidate is judged, not only how deeply**, because a search that
    /// finished no iteration is an error here rather than a rejection. Above
    /// that, a search it interrupts is scored on the deepest iteration
    /// that finished, so raising the cap does not admit or reject candidates
    /// directly — it moves the depth their verdict comes from, which can
    /// change the verdict. That is enough to make a set generated at another
    /// cap a different file.
    pub balance_node_cap: u64,
    /// A candidate survives iff rinsai's own score sits within ±this, in
    /// centipawns from the side to move, and is not a mate score.
    pub balance_cp_max: i32,
    /// Table size for the balance search — fixed for the same reason `bench`
    /// fixes its own: an operator's option must not move a frozen output.
    pub balance_hash_mb: usize,
    /// Seeds the Fisher–Yates pick over the surviving candidates.
    pub seed: u64,
    /// What the header calls this set, e.g. `"v2"`.
    pub set_name: &'static str,
    /// The file the header sends a later set to, e.g. `"openings-v3.sfen"`.
    pub next_set_file: &'static str,
}

impl PipelineConfig {
    /// The constants `positions/openings-v2.sfen` was generated with, at the
    /// rev its header names. The seed spells `RINSAI-1`.
    #[must_use]
    pub fn frozen_v2() -> Self {
        Self {
            min_rate: 3000.0,
            min_game_plies: 60,
            window: 12..=24,
            max_per_game: 2,
            target: 256,
            balance_depth: 6,
            balance_node_cap: 2_000_000,
            balance_cp_max: 100,
            balance_hash_mb: 16,
            seed: 0x5249_4E53_4149_2D31,
            set_name: "v2",
            next_set_file: "openings-v3.sfen",
        }
    }

    /// The constants `positions/openings-v3.sfen` was generated with, at the
    /// rev its header names: v2's filters unchanged, drawn deeper. The seed
    /// spells `RINSAI-3`.
    ///
    /// ⚠️ **The target bounds every fixed-node SPRT opened from the set**, and
    /// that is what it is for: `runner::check_budget_fits_openings` will not
    /// play more pairs than there are lines. How many a given gate needs is a
    /// function of the true Elo gap, so no single number belongs here.
    #[must_use]
    pub fn frozen_v3() -> Self {
        Self {
            target: 3000,
            seed: 0x5249_4E53_4149_2D33,
            set_name: "v3",
            next_set_file: "openings-v4.sfen",
            ..Self::frozen_v2()
        }
    }
}

/// One day of records. `files` must be `(basename, contents)` in ascending
/// basename order — file order is part of the deterministic scan the header
/// promises.
#[derive(Debug)]
pub struct DayInput {
    /// `YYYY-MM-DD`, used in provenance lines.
    pub label: String,
    pub files: Vec<(String, String)>,
}

/// What happened to the corpus on the way to the file. Reported to the
/// operator by `run`, and deliberately not written into the set: a frozen
/// artifact records what it *is*, and the run that produced it is recoverable
/// from the recorded rev.
#[derive(Debug, Default)]
pub struct Counters {
    files: usize,
    rejected_rate: usize,
    rejected_termination: usize,
    rejected_short: usize,
    rejected_replay: usize,
    snapshots: usize,
    unique: usize,
    evaluated: usize,
    rejected_balance: usize,
    capped: usize,
}

impl std::fmt::Display for Counters {
    /// Every field is named here, and that is what keeps them alive: this
    /// is their only reader.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let qualifying =
            self.files - self.rejected_rate - self.rejected_termination - self.rejected_short;
        write!(
            f,
            "{} records, {qualifying} qualifying games, {} replayed clean\n  \
             rejected: {} on rate, {} on termination, {} on length, {} on replay\n  \
             {} snapshots, {} unique positions, {} skipped over the per-game cap\n  \
             {} searched, {} outside the balance window",
            self.files,
            qualifying - self.rejected_replay,
            self.rejected_rate,
            self.rejected_termination,
            self.rejected_short,
            self.rejected_replay,
            self.snapshots,
            self.unique,
            self.capped,
            self.evaluated,
            self.rejected_balance,
        )
    }
}

#[derive(Debug)]
struct Candidate {
    day: usize,
    game: usize,
    file: String,
    ply: usize,
    moves_usi: String,
}

/// The whole pipeline, cache to file contents. Pure given its arguments —
/// the balance search is deterministic at fixed depth and fixed table size.
pub fn generate(
    days: &[DayInput],
    cfg: &PipelineConfig,
    rev: &str,
) -> Result<(String, Counters), String> {
    let mut counters = Counters::default();
    let mut candidates: Vec<Candidate> = Vec::new();
    let mut seen = HashSet::new();
    let mut game_id = 0usize;

    for (day_index, day) in days.iter().enumerate() {
        for (name, text) in &day.files {
            counters.files += 1;
            let record = csa::parse(name, text);

            let rates_ok = record.black_rate.is_some_and(|r| r >= cfg.min_rate)
                && record.white_rate.is_some_and(|r| r >= cfg.min_rate);
            if !rates_ok {
                counters.rejected_rate += 1;
                continue;
            }
            if !matches!(record.termination.as_deref(), Some("%TORYO" | "%KACHI")) {
                counters.rejected_termination += 1;
                continue;
            }
            if record.moves.len() < cfg.min_game_plies {
                counters.rejected_short += 1;
                continue;
            }

            game_id += 1;
            match replay(&record.moves, cfg) {
                Ok(snapshots) => {
                    for (ply, moves_usi, key) in snapshots {
                        counters.snapshots += 1;
                        if seen.insert(key) {
                            counters.unique += 1;
                            candidates.push(Candidate {
                                day: day_index,
                                game: game_id,
                                file: name.clone(),
                                ply,
                                moves_usi,
                            });
                        }
                    }
                }
                Err(e) => {
                    counters.rejected_replay += 1;
                    eprintln!("gen-openings: skipping {}/{name}: {e}", day.label);
                }
            }
        }
    }

    // Pick and balance in one lazily-evaluated pass: candidates are visited
    // in the seeded shuffle's order, the per-game cap is checked before any
    // search is spent, and the walk stops at the target — so the number of
    // balance searches scales with the target, not with the corpus.
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    let mut state = cfg.seed;
    rng::shuffle(&mut order, &mut state);

    let mut balance = BalanceSearch::new(cfg);
    let mut per_game: HashMap<usize, usize> = HashMap::new();
    let mut picked: Vec<(usize, i32, i32)> = Vec::new();
    for i in order {
        if picked.len() == cfg.target {
            break;
        }
        let candidate = &candidates[i];
        let taken = per_game.entry(candidate.game).or_insert(0);
        if *taken >= cfg.max_per_game {
            counters.capped += 1;
            continue;
        }
        counters.evaluated += 1;
        let args = format!("startpos moves {}", candidate.moves_usi);
        let reading = balance
            .score(cfg, &args)
            .map_err(|e| format!("{} ply {}: {e}", candidate.file, candidate.ply))?;
        let Some((is_mate, cp)) = reading.completed else {
            return Err(format!(
                "{} ply {}: the balance search completed no iteration",
                candidate.file, candidate.ply
            ));
        };
        if !is_mate && cp.abs() <= cfg.balance_cp_max {
            *taken += 1;
            picked.push((i, cp, reading.completed_depth));
        } else {
            counters.rejected_balance += 1;
        }
    }
    if picked.len() < cfg.target {
        return Err(format!(
            "only {} of the {} unique candidates passed the balance filter under <= {} per \
             game — a target of {} needs more source days",
            picked.len(),
            counters.unique,
            cfg.max_per_game,
            cfg.target
        ));
    }
    // A canonical re-sort: the shuffle chose the subset, the source order
    // fixes the layout, so the file diffs readably.
    picked.sort_by(|&(a, _, _), &(b, _, _)| {
        let (a, b) = (&candidates[a], &candidates[b]);
        (a.day, &a.file, a.ply).cmp(&(b.day, &b.file, b.ply))
    });

    Ok((emit(days, cfg, rev, &candidates, &picked), counters))
}

/// Replay one game's moves up to the window's end, snapshotting every
/// in-window position where the side to move is not in check.
/// Returns `(ply, moves so far in USI, position key)` per snapshot.
#[allow(clippy::type_complexity)]
fn replay(
    moves: &[CsaMove],
    cfg: &PipelineConfig,
) -> Result<Vec<(usize, String, rinsai_game::PositionKey)>, String> {
    let mut game = Game::startpos();
    let mut tokens: Vec<String> = Vec::new();
    let mut snapshots = Vec::new();
    for (i, mv) in moves.iter().enumerate() {
        let ply = i + 1;
        if ply > *cfg.window.end() {
            break;
        }
        let mv = build_move(&game, mv).map_err(|e| format!("move {ply}: {e}"))?;
        game.play(mv).map_err(|e| format!("move {ply}: {e}"))?;
        tokens.push(mv.to_usi_owned());
        if cfg.window.contains(&ply) && !game.in_check() {
            snapshots.push((ply, tokens.join(" "), position_key(game.position())));
        }
    }
    Ok(snapshots)
}

/// A CSA move statement against the position it was played in. CSA writes
/// the piece kind *after* the move, so promotion is recovered by comparing
/// with the piece found on the from-square.
fn build_move(game: &Game, mv: &CsaMove) -> Result<Move, String> {
    let side = game.side_to_move();
    if (side == Color::Black) != mv.black {
        return Err("the record and the board disagree on the side to move".to_owned());
    }
    let square =
        |(f, r): (u8, u8)| Square::new(f, r).ok_or_else(|| format!("({f}, {r}) is not a square"));
    match mv.from {
        None => Ok(Move::Drop {
            piece: Piece::new(mv.kind, side),
            to: square(mv.to)?,
        }),
        Some(from) => {
            let from = square(from)?;
            let to = square(mv.to)?;
            let piece = game
                .position()
                .piece_at(from)
                .ok_or("nothing on the from-square")?;
            let (kind_before, colour) = piece.to_parts();
            if colour != side {
                return Err("the from-square holds the opponent's piece".to_owned());
            }
            let promote = if kind_before == mv.kind {
                false
            } else if kind_before.promote() == Some(mv.kind) {
                true
            } else {
                return Err(format!(
                    "the from-square holds a {kind_before:?}, the record wrote a {:?}",
                    mv.kind
                ));
            };
            Ok(Move::Normal { from, to, promote })
        }
    }
}

/// Every `info` line's `(depth, is_mate, value)`, so the balance filter can
/// ask for the score of a named iteration.
///
/// ⚠️ **The last line published is the wrong one to judge a position on.** A
/// search that spends its node cap mid-iteration publishes that iteration
/// anyway, and its score is the best over a *prefix* of the root move list —
/// a lower bound, not the iteration's value.
///
/// ⚠️ **A lower bound against `|cp| <= max` errs in both directions**, which
/// is the half that is easy to get wrong: it can sit inside the band while
/// the finished value is above `+max`, and it can sit below `−max` while the
/// finished value is inside, and both happen.
#[derive(Debug, Default)]
struct ScoreSink(Mutex<Vec<(i32, bool, i32)>>);

impl ScoreSink {
    /// `(is_mate, value)` from the iteration at `depth` — centipawns from the
    /// side to move, or mate distance when `is_mate`. `None` when no line
    /// reported that depth, which for `depth` 0 means no iteration finished.
    fn at(&self, depth: i32) -> Option<(bool, i32)> {
        self.0
            .lock()
            .expect("no panics hold this lock")
            .iter()
            .rev()
            .find(|&&(d, _, _)| d == depth)
            .map(|&(_, is_mate, value)| (is_mate, value))
    }

    /// `(depth, is_mate, value)` from the last line published, whichever
    /// iteration it belongs to. `None` when no line carried both fields.
    fn last(&self) -> Option<(i32, bool, i32)> {
        self.0
            .lock()
            .expect("no panics hold this lock")
            .last()
            .copied()
    }
}

/// What one candidate's balance search reported, under both of the readings
/// its caller could take it on.
///
/// ⚠️ **`completed` is the one the filter judges on and `last` is the one it
/// must not** — the two differ exactly when the node cap interrupts an
/// iteration deeper than the last one that finished. `last` has one caller,
/// `a_score_from_an_interrupted_iteration_does_not_decide_a_candidate`, which
/// reads it to check that the two still disagree on the fixture chosen to make
/// them.
#[derive(Debug)]
pub struct Balance {
    /// The deepest iteration that searched every root move; `0` when none did.
    pub completed_depth: i32,
    /// `(is_mate, value)` at `completed_depth`. `None` when no iteration
    /// finished, which the caller turns into an error rather than a guess.
    pub completed: Option<(bool, i32)>,
    /// `(depth, is_mate, value)` of the last line published. `None` when the
    /// search published none.
    pub last: Option<(i32, bool, i32)>,
}

/// The searcher the balance filter judges candidates with.
///
/// One searcher serves a whole run and is cleared per candidate, so no
/// candidate is scored against what an earlier one left in the table.
#[derive(Debug)]
pub struct BalanceSearch {
    searcher: NegamaxSearcher,
    searched: u64,
}

impl BalanceSearch {
    /// ⚠️ The table size is the config's rather than an operator's, for the
    /// reason [`PipelineConfig::balance_hash_mb`] carries.
    #[must_use]
    pub fn new(cfg: &PipelineConfig) -> Self {
        Self {
            searcher: NegamaxSearcher::with_hash_mb(cfg.balance_hash_mb),
            searched: 0,
        }
    }

    /// Searches one candidate and reports both readings. `position_args` is a
    /// USI `position` argument, the shape the emitted file's own lines carry.
    ///
    /// The error names what went wrong and not where: the caller knows which
    /// record and ply the position came from, and prefixes it.
    pub fn score(&mut self, cfg: &PipelineConfig, position_args: &str) -> Result<Balance, String> {
        self.searcher.new_game();
        self.searched += 1;
        // The moves were legal for the referee's rules library; the engine's
        // movegen refusing one would be a legality disagreement worth a loud
        // stop, not a skipped line.
        let game = rinsai_search::Game::from_usi_position(position_args)
            .map_err(|e| format!("shunsai refused a replay legality_lite accepted: {e:?}"))?;
        let job = SearchJob {
            id: self.searched,
            game,
            limits: Limits {
                depth: Some(cfg.balance_depth),
                nodes: Some(cfg.balance_node_cap),
                ..Limits::default()
            },
            signals: Arc::new(SearchSignals::new()),
        };
        let sink = ScoreSink::default();
        let completed_depth = match self.searcher.search(&job, &sink) {
            BestMove::Play {
                completed_depth, ..
            } => completed_depth,
            // No legal move at all: a candidate the window should never have
            // produced, since it snapshots positions the game continued from.
            BestMove::Resign => 0,
        };
        Ok(Balance {
            completed_depth,
            completed: sink.at(completed_depth),
            last: sink.last(),
        })
    }
}

impl InfoSink for ScoreSink {
    fn info(&self, line: &str) {
        let mut depth = None;
        let mut score = None;
        let mut tokens = line.split_whitespace();
        while let Some(token) = tokens.next() {
            match token {
                // `seldepth` is a whole token of its own, so it cannot be
                // mistaken for this one.
                "depth" => depth = tokens.next().and_then(|v| v.parse::<i32>().ok()),
                "score" => {
                    let kind = tokens.next();
                    let value = tokens.next().and_then(|v| v.parse::<i32>().ok());
                    score = match (kind, value) {
                        (Some("cp"), Some(v)) => Some((false, v)),
                        (Some("mate"), Some(v)) => Some((true, v)),
                        _ => None,
                    };
                    // `depth` precedes `score` in the line, so nothing
                    // after this point is wanted.
                    break;
                }
                _ => {}
            }
        }
        if let (Some(depth), Some((is_mate, value))) = (depth, score) {
            self.0
                .lock()
                .expect("no panics hold this lock")
                .push((depth, is_mate, value));
        }
    }
}

fn emit(
    days: &[DayInput],
    cfg: &PipelineConfig,
    rev: &str,
    candidates: &[Candidate],
    picked: &[(usize, i32, i32)],
) -> String {
    let mut out = String::new();
    let _ = write!(
        out,
        "\
# rinsai opening set, {set} — frozen.
#
# One USI `position` argument per line (`startpos moves …`); `#` starts a
# comment and blank lines are ignored.
#
# ⚠️ FROZEN. Paired-game results are comparable only within one opening set,
# so a later set is a new file, {next}, never an edit to this one.
#
# Generated by `cargo run --release -p xtask -- gen-openings` at
# rev {rev},
# splitmix64 seed {seed:#018x}.
# Every other input is fixed in the generator, so `--rev` reproduces this file
# byte for byte from the same cached records.
#
# The floodgate game behind each line is named on the comment above it, with
# `eval=` rinsai's own score from the side to move and `d=` the deepest
# iteration that searched every root move.

",
        set = cfg.set_name,
        next = cfg.next_set_file,
        rev = rev,
        seed = cfg.seed,
    );
    for &(i, eval_cp, eval_depth) in picked {
        let c = &candidates[i];
        let _ = write!(
            out,
            "# {}/{} ply={} eval={:+} d={}\nstartpos moves {}\n",
            days[c.day].label, c.file, c.ply, eval_cp, eval_depth, c.moves_usi
        );
    }
    out
}

pub fn run(args: &[String]) -> ExitCode {
    let mut dates: Option<String> = None;
    let mut root = PathBuf::from("data/floodgate");
    let mut out_path = PathBuf::from("positions/openings-v3.sfen");
    let mut rev: Option<String> = None;
    // The current set. An earlier one is frozen and is not regenerated.
    let mut cfg = PipelineConfig::frozen_v3();
    let mut iter = args.iter();
    // ⚠️ A flag whose value is missing must be an error, never a silent
    // fallback: `--out` with the path eaten by the shell would otherwise
    // rewrite the frozen set in place, and `--rev` would stamp today's HEAD
    // as the provenance of a run it does not describe.
    let mut parsed = || -> Result<_, String> {
        while let Some(arg) = iter.next() {
            let mut value = || {
                iter.next()
                    .cloned()
                    .ok_or_else(|| format!("`{arg}` wants a value"))
            };
            match arg.as_str() {
                "--dates" => dates = Some(value()?),
                "--root" => root = PathBuf::from(value()?),
                "--out" => out_path = PathBuf::from(value()?),
                // The header's rev is the provenance of the *original*
                // generation. To reproduce a committed file byte for byte,
                // pass the rev its header records — HEAD has necessarily
                // moved past it, since the commit that added the file came
                // after the run that stamped it.
                "--rev" => rev = Some(value()?),
                "--seed" => cfg.seed = parse_seed(&value()?)?,
                other => return Err(format!("unknown argument `{other}`")),
            }
        }
        Ok(())
    };
    if let Err(e) = parsed() {
        eprintln!("gen-openings: {e}");
        return usage();
    }
    let Some(dates) = dates else {
        eprintln!("gen-openings: --dates is required");
        return usage();
    };
    let days = match fetch::parse_date_range(&dates) {
        Ok(days) => days,
        Err(e) => {
            eprintln!("gen-openings: {e}");
            return usage();
        }
    };

    let mut inputs = Vec::new();
    for day in &days {
        let dir = day.cache_dir(&root);
        let mut files = Vec::new();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) => {
                eprintln!(
                    "gen-openings: {}: {e} — fetch-floodgate --dates {day} fills the cache",
                    dir.display()
                );
                return ExitCode::FAILURE;
            }
        };
        for entry in entries {
            let Ok(entry) = entry else { continue };
            let path = entry.path();
            if path.extension().is_none_or(|e| e != "csa") {
                continue;
            }
            let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let Ok(bytes) = std::fs::read(&path) else {
                eprintln!("gen-openings: unreadable {}", path.display());
                return ExitCode::FAILURE;
            };
            files.push((
                name.to_owned(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
        files.sort_by(|a, b| a.0.cmp(&b.0));
        inputs.push(DayInput {
            label: day.to_string(),
            files,
        });
    }

    let rev = match rev {
        Some(rev) => rev,
        None => match git_head() {
            Ok(rev) => rev,
            Err(e) => {
                eprintln!("gen-openings: {e}");
                return ExitCode::FAILURE;
            }
        },
    };
    match generate(&inputs, &cfg, &rev) {
        Ok((contents, counters)) => {
            eprintln!("gen-openings: {counters}");
            let lines = contents
                .lines()
                .filter(|l| l.starts_with("startpos"))
                .count();
            // Written beside the target and renamed, the same shape
            // `fetch-floodgate` uses: an interrupted write must not leave a
            // truncated opening set that the harness would then load without
            // complaint.
            let part = out_path.with_extension("part");
            if let Err(e) =
                std::fs::write(&part, contents).and_then(|()| std::fs::rename(&part, &out_path))
            {
                let _ = std::fs::remove_file(&part);
                eprintln!("gen-openings: write {}: {e}", out_path.display());
                return ExitCode::FAILURE;
            }
            println!(
                "gen-openings: wrote {lines} openings to {}",
                out_path.display()
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("gen-openings: {e}");
            ExitCode::FAILURE
        }
    }
}

fn parse_seed(text: &str) -> Result<u64, String> {
    let parsed = match text.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16),
        None => text.parse(),
    };
    parsed.map_err(|_| format!("`{text}` is not a decimal or 0x-hex u64"))
}

fn git_head() -> Result<String, String> {
    let out = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .map_err(|e| format!("running git rev-parse: {e}"))?;
    if !out.status.success() {
        return Err("git rev-parse HEAD failed — run from inside the repository".to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo run --release -p xtask -- gen-openings --dates 2026-06-01..2026-06-21 \
         [--root data/floodgate] [--out positions/openings-v3.sfen] [--seed N] [--rev SHA]"
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A search's published lines, in the order the deepening loop emits them.
    fn feed(sink: &ScoreSink, lines: &[&str]) {
        for line in lines {
            sink.info(line);
        }
    }

    /// The rule the balance filter rests on: a named depth's score, not the
    /// newest one.
    ///
    /// Sabotage: have `at` ignore its argument and return the last entry —
    /// the depth-2 score below — and this fires.
    #[test]
    fn a_named_depth_is_read_and_not_the_last_line() {
        let sink = ScoreSink::default();
        feed(
            &sink,
            &[
                "info depth 1 seldepth 1 time 0 nodes 31 nps 0 hashfull 0 score cp 0 pv 7g7f",
                "info depth 2 seldepth 5 time 1 nodes 900 nps 0 hashfull 0 score cp 415 pv 7g7f",
            ],
        );
        assert_eq!(sink.at(1), Some((false, 0)));
        assert_eq!(sink.at(2), Some((false, 415)));
    }

    /// A depth no line reported has no score, which is what the caller turns
    /// into an error rather than a guess. `0` is that case in practice: it is
    /// what a search reports when no iteration finished.
    #[test]
    fn an_unreported_depth_has_no_score() {
        let sink = ScoreSink::default();
        feed(
            &sink,
            &["info depth 1 seldepth 1 time 0 nodes 31 nps 0 hashfull 0 score cp 0 pv 7g7f"],
        );
        assert_eq!(sink.at(0), None);
        assert_eq!(sink.at(2), None);
    }

    /// A mate score is kept apart from a centipawn one, because the filter
    /// rejects on it rather than comparing it against ±`balance_cp_max` — a
    /// `mate 1` compared as centipawns would read as balanced.
    ///
    /// Sabotage: parse `mate` as `cp` and the filter admits mates.
    #[test]
    fn a_mate_score_is_not_a_centipawn_score() {
        let sink = ScoreSink::default();
        feed(
            &sink,
            &["info depth 3 seldepth 9 time 0 nodes 130 nps 0 hashfull 0 score mate 2 pv 5e5d"],
        );
        assert_eq!(sink.at(3), Some((true, 2)));
    }
}
