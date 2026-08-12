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
use rinsai_search::{InfoSink, Limits, NegamaxSearcher, SearchJob, SearchSignals, Searcher};
use shogi_core::{Color, Move, Piece, Square, ToUsi};

use crate::csa::{self, CsaMove};
use crate::fetch;
use crate::rng;

/// Everything that moves the output. ⚠️ Changing any field after
/// `openings-v1.sfen` is frozen means a new file, not a regeneration.
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
    /// Node cap on the balance search. The engine's quiescence is unordered
    /// and unpruned, so an open position can cost orders more than the depth
    /// suggests; the cap bounds it, and the score is then the last completed
    /// iteration's — the first iteration always completes, so a score always
    /// exists. A capped search is as deterministic as an uncapped one.
    pub balance_node_cap: u64,
    /// A candidate survives iff rinsai's own score sits within ±this, in
    /// centipawns from the side to move, and is not a mate score.
    pub balance_cp_max: i32,
    /// Table size for the balance search — fixed for the same reason `bench`
    /// fixes its own: an operator's option must not move a frozen output.
    pub balance_hash_mb: usize,
    /// Seeds the Fisher–Yates pick over the surviving candidates.
    pub seed: u64,
}

impl PipelineConfig {
    /// The constants `positions/openings-v1.sfen` is generated with. The seed
    /// spells `RINSAI-1`.
    #[must_use]
    pub fn frozen_v1() -> Self {
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

/// What happened to the corpus on the way to the file — interpolated into the
/// header so the counts and the output cannot disagree.
#[derive(Debug, Default)]
struct Counters {
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
pub fn generate(days: &[DayInput], cfg: &PipelineConfig, rev: &str) -> Result<String, String> {
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
    // balance searches scales with the target, not with the corpus. One
    // searcher, cleared per candidate so no candidate is scored against what
    // an earlier one left in the table.
    let mut order: Vec<usize> = (0..candidates.len()).collect();
    let mut state = cfg.seed;
    rng::shuffle(&mut order, &mut state);

    let mut searcher = NegamaxSearcher::with_hash_mb(cfg.balance_hash_mb);
    let mut per_game: HashMap<usize, usize> = HashMap::new();
    let mut picked: Vec<(usize, i32)> = Vec::new();
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
        searcher.new_game();
        let args = format!("startpos moves {}", candidate.moves_usi);
        // The moves were legal for the referee's rules library; the engine's
        // movegen refusing one would be a legality disagreement worth a loud
        // stop, not a skipped line.
        let game = rinsai_search::Game::from_usi_position(&args).map_err(|e| {
            format!(
                "{} ply {}: shunsai refused a replay legality_lite accepted: {e:?}",
                candidate.file, candidate.ply
            )
        })?;
        let job = SearchJob {
            id: counters.evaluated as u64,
            game,
            limits: Limits {
                depth: Some(cfg.balance_depth),
                nodes: Some(cfg.balance_node_cap),
                ..Limits::default()
            },
            signals: Arc::new(SearchSignals::new()),
        };
        let sink = ScoreSink::default();
        searcher.search(&job, &sink);
        let Some((is_mate, cp)) = sink.last() else {
            return Err(format!(
                "{} ply {}: the balance search published no score",
                candidate.file, candidate.ply
            ));
        };
        if !is_mate && cp.abs() <= cfg.balance_cp_max {
            *taken += 1;
            picked.push((i, cp));
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
    picked.sort_by(|&(a, _), &(b, _)| {
        let (a, b) = (&candidates[a], &candidates[b]);
        (a.day, &a.file, a.ply).cmp(&(b.day, &b.file, b.ply))
    });

    Ok(emit(days, cfg, rev, &counters, &candidates, &picked))
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

/// Captures the score of the last `info` line a search published.
#[derive(Debug, Default)]
struct ScoreSink(Mutex<Option<(bool, i32)>>);

impl ScoreSink {
    /// `(is_mate, value)` — centipawns from the side to move, or mate
    /// distance when `is_mate`.
    fn last(&self) -> Option<(bool, i32)> {
        *self.0.lock().expect("no panics hold this lock")
    }
}

impl InfoSink for ScoreSink {
    fn info(&self, line: &str) {
        let mut tokens = line.split_whitespace();
        while let Some(token) = tokens.next() {
            if token != "score" {
                continue;
            }
            let kind = tokens.next();
            let value = tokens.next().and_then(|v| v.parse::<i32>().ok());
            if let (Some(kind), Some(value)) = (kind, value) {
                let is_mate = match kind {
                    "cp" => false,
                    "mate" => true,
                    _ => return,
                };
                *self.0.lock().expect("no panics hold this lock") = Some((is_mate, value));
            }
            return;
        }
    }
}

fn emit(
    days: &[DayInput],
    cfg: &PipelineConfig,
    rev: &str,
    counters: &Counters,
    candidates: &[Candidate],
    picked: &[(usize, i32)],
) -> String {
    let labels: Vec<&str> = days.iter().map(|d| d.label.as_str()).collect();
    let qualifying = counters.files
        - counters.rejected_rate
        - counters.rejected_termination
        - counters.rejected_short;
    let mut out = String::new();
    let _ = write!(
        out,
        "\
# rinsai opening set, v1 — frozen.
#
# One USI `position` argument per line (`startpos moves …`); `#` starts a
# comment and blank lines are ignored — the same conventions as
# bench-v1.sfen. The SPRT harness plays every line twice per pair, colours
# swapped (CLAUDE.md §3).
#
# ⚠️ FROZEN. Paired-game results are comparable only within one opening set,
# so a later set is a new file, openings-v2.sfen, never an edit to this one.
#
# PROVENANCE (CLAUDE.md §2). floodgate game records are factual data
# (DESIGN.md §7); the game behind each line is named on the comment above
# it. Generated by `cargo run --release -p xtask -- gen-openings` at rev
# {rev}, splitmix64 seed {seed:#018x}.
#   source days: {labels} — {files} records, {qualifying} qualifying games,
#     {replayed} replayed clean, {rej_rate} rejected on rate,
#     {rej_term} on termination, {rej_short} on length, {rej_replay} on replay
#   game filter: both floodgate rates >= {min_rate}, ends %TORYO or %KACHI,
#     >= {min_plies} plies, every replayed move legal
#   window: plies {win_a}..={win_b}, side to move not in check —
#     {snapshots} snapshots, {unique} unique positions
#   pick and balance, one lazily-evaluated pass: candidates visited in
#     seeded Fisher-Yates order, the per-game cap (<= {max_per_game} of one
#     source game; {capped} skipped over it) checked before any search;
#     {evaluated} searched at depth {depth} capped at {node_cap} nodes,
#     {hash_mb} MiB table; {rej_bal} fell outside ±{cp_max} cp or carried a
#     mate score; the first {target} survivors are the set, emitted in
#     (day, file, ply) order. eval= on each comment line is rinsai's own
#     score, from the side to move.
# Regeneration from the same cached inputs, rev and seed is byte-identical
# (`--rev` replays the rev recorded above, since HEAD moves when the file
# is committed); crates/xtask/tests/gen_openings_fixture.rs is the
# executable form of that claim, at fixture scale.

",
        rev = rev,
        seed = cfg.seed,
        labels = labels.join(" "),
        files = counters.files,
        qualifying = qualifying,
        replayed = qualifying - counters.rejected_replay,
        rej_rate = counters.rejected_rate,
        rej_term = counters.rejected_termination,
        rej_short = counters.rejected_short,
        rej_replay = counters.rejected_replay,
        min_rate = cfg.min_rate,
        min_plies = cfg.min_game_plies,
        win_a = cfg.window.start(),
        win_b = cfg.window.end(),
        snapshots = counters.snapshots,
        unique = counters.unique,
        capped = counters.capped,
        evaluated = counters.evaluated,
        rej_bal = counters.rejected_balance,
        cp_max = cfg.balance_cp_max,
        depth = cfg.balance_depth,
        node_cap = cfg.balance_node_cap,
        hash_mb = cfg.balance_hash_mb,
        max_per_game = cfg.max_per_game,
        target = cfg.target,
    );
    for &(i, eval_cp) in picked {
        let c = &candidates[i];
        let _ = write!(
            out,
            "# {}/{} ply={} eval={:+}\nstartpos moves {}\n",
            days[c.day].label, c.file, c.ply, eval_cp, c.moves_usi
        );
    }
    out
}

pub fn run(args: &[String]) -> ExitCode {
    let mut dates: Option<String> = None;
    let mut root = PathBuf::from("data/floodgate");
    let mut out_path = PathBuf::from("positions/openings-v1.sfen");
    let mut rev: Option<String> = None;
    let mut cfg = PipelineConfig::frozen_v1();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dates" => dates = iter.next().cloned(),
            "--root" => {
                if let Some(v) = iter.next() {
                    root = PathBuf::from(v);
                }
            }
            "--out" => {
                if let Some(v) = iter.next() {
                    out_path = PathBuf::from(v);
                }
            }
            // The header's rev is the provenance of the *original* generation.
            // To reproduce a committed file byte for byte, pass the rev its
            // header records — HEAD has necessarily moved past it, since the
            // commit that added the file came after the run that stamped it.
            "--rev" => rev = iter.next().cloned(),
            "--seed" => match iter.next().map(|v| parse_seed(v)) {
                Some(Ok(seed)) => cfg.seed = seed,
                Some(Err(e)) => {
                    eprintln!("gen-openings: {e}");
                    return usage();
                }
                None => {
                    eprintln!("gen-openings: --seed wants a value");
                    return usage();
                }
            },
            other => {
                eprintln!("gen-openings: unknown argument `{other}`");
                return usage();
            }
        }
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
        Ok(contents) => {
            let lines = contents
                .lines()
                .filter(|l| l.starts_with("startpos"))
                .count();
            if let Err(e) = std::fs::write(&out_path, contents) {
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
        "usage: cargo run --release -p xtask -- gen-openings --dates 2026-06-01..2026-06-07 \
         [--root data/floodgate] [--out positions/openings-v1.sfen] [--seed N] [--rev SHA]"
    );
    ExitCode::FAILURE
}
