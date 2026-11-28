//! The `instructions` subcommand: what a candidate does per move against its
//! baseline, counted in instructions, and the node budget that evens it out.
//!
//! A patch that skips work no node counts — a pruned move is generated and
//! evaluated, then never searched — changes what one counted node costs, so
//! two equal node budgets stop being equal work. This counts both binaries'
//! instructions under callgrind on the same positions at the gate's budget
//! and names the candidate budget that matches them.
//!
//! ⚠️ **Instructions, not time.** A count repeats to about one part in a
//! million across runs of one binary, so it needs no quiet machine; how well
//! it stands in for time is not measured here.
//!
//! ⚠️ **The positions come from the baseline playing itself**, both seats,
//! from the opening set at the gate's budget — not from a gate's own games.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use crate::config::{self, EngineConfig};
use crate::jsonl::JsonObject;
use crate::referee::{self, EndReason, MatchControl};
use crate::rng;
use crate::runner::{load_openings, parse_u64};
use crate::schedule;
use crate::usi::{GoSpec, UsiEngine};

const DEFAULT_POSITIONS: usize = 120;
const DEFAULT_NODES: u64 = 300_000;
/// Generous because a move under callgrind is far slower than a native one;
/// the same bound serves the native games that pick the positions.
const DEFAULT_MOVE_TIMEOUT: Duration = Duration::from_secs(600);
/// How far past its opening a pair's position sits is this times
/// `pair mod POSITION_STEPS`.
const PLIES_PER_STEP: usize = 20;
const POSITION_STEPS: u64 = 5;
/// A game that ends within this many plies of the position it was played for
/// gives the opening instead.
const END_MARGIN: usize = 4;
const RESAMPLES: usize = 10_000;

struct Args {
    candidate: String,
    baseline: String,
    config_path: PathBuf,
    openings_path: PathBuf,
    positions: usize,
    seed: u64,
    nodes: u64,
    concurrency: usize,
    move_timeout: Duration,
}

/// The candidate measured again at the budget [`matched_budget`] named.
struct Matched {
    budget: u64,
    /// Net instructions per position, in [`Report::samples`]' order.
    counts: Vec<u64>,
    ratio: f64,
    interval: (f64, f64),
}

/// Everything one run measured.
struct Report {
    samples: Vec<Sample>,
    /// `(candidate, baseline)` net instructions per position.
    pairs: Vec<(u64, u64)>,
    ratio: f64,
    interval: (f64, f64),
    /// `None` when the interval includes 1 and both play at the gate budget.
    matched: Option<Matched>,
}

/// One measured position.
struct Sample {
    pair: u64,
    opening: usize,
    /// Plies from the opening's own root.
    plies: usize,
    /// A USI `position` argument.
    position: String,
}

pub fn run(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("instructions: {e}");
            return usage();
        }
    };
    match measure(&args) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("instructions: {e}");
            ExitCode::FAILURE
        }
    }
}

fn measure(args: &Args) -> Result<(), String> {
    let engines = config::load(&args.config_path)?;
    let find = |id: &str| {
        engines
            .iter()
            .find(|e| e.id == id)
            .ok_or_else(|| format!("no engine `{id}` in {}", args.config_path.display()))
    };
    let (candidate, baseline) = (find(&args.candidate)?, find(&args.baseline)?);
    let toolchain = same_toolchain(&candidate.path, &baseline.path)?;
    let valgrind = Command::new("valgrind")
        .arg("--version")
        .output()
        .map_err(|e| format!("valgrind: {e}"))?;
    if !valgrind.status.success() {
        return Err("`valgrind --version` failed".to_owned());
    }
    let valgrind = String::from_utf8_lossy(&valgrind.stdout).trim().to_owned();
    let openings = load_openings(&args.openings_path)?;
    if args.positions > openings.len() {
        return Err(format!(
            "{} positions from {} openings would repeat one",
            args.positions,
            openings.len()
        ));
    }

    println!(
        "instructions: {} positions at {} nodes; up to {} workers, each running two {} \
         processes for a game or one engine under {valgrind}",
        args.positions, args.nodes, args.concurrency, baseline.id
    );
    let plans: Vec<_> = schedule::pairings(openings.len(), args.seed)
        .take(args.positions)
        .collect();
    let samples = parallel(args.concurrency, &plans, |[plan, _]| {
        sample(args, baseline, plan.pair, &openings[plan.opening]).map(|(plies, position)| Sample {
            pair: plan.pair,
            opening: plan.opening,
            plies,
            position,
        })
    })?;

    let spec = |nodes| GoSpec::Nodes {
        nodes,
        hang_timeout: args.move_timeout,
    };
    let setup = |engine: &EngineConfig| {
        let depth_1 = GoSpec::Depth {
            depth: 1,
            hang_timeout: args.move_timeout,
        };
        count(engine, "startpos", depth_1, args.move_timeout)
    };
    let setups = [setup(baseline)?, setup(candidate)?];
    let runs: Vec<(&EngineConfig, u64, &Sample)> = samples
        .iter()
        .flat_map(|s| [(baseline, setups[0], s), (candidate, setups[1], s)])
        .collect();
    let counts = parallel(args.concurrency, &runs, |&(engine, setup, s)| {
        let total = count(engine, &s.position, spec(args.nodes), args.move_timeout)?;
        net(total, setup, &engine.id)
    })?;
    let pairs: Vec<(u64, u64)> = counts.chunks(2).map(|c| (c[1], c[0])).collect();
    let interval = bootstrap(&pairs, RESAMPLES, args.seed);
    let budget = matched_budget(args.nodes, interval);
    let matched = if budget == args.nodes {
        None
    } else {
        let counts = parallel(args.concurrency, &samples, |s| {
            let total = count(candidate, &s.position, spec(budget), args.move_timeout)?;
            net(total, setups[1], &candidate.id)
        })?;
        let back: Vec<(u64, u64)> = counts
            .iter()
            .zip(&pairs)
            .map(|(&c, &(_, b))| (c, b))
            .collect();
        Some(Matched {
            budget,
            counts,
            ratio: ratio(&back),
            interval: bootstrap(&back, RESAMPLES, args.seed),
        })
    };
    let report = Report {
        ratio: ratio(&pairs),
        samples,
        pairs,
        interval,
        matched,
    };

    let log_path = write_log(args, candidate, baseline, &toolchain, &report)?;
    println!(
        "candidate/baseline at {} nodes: {:.5} [{:.5}, {:.5}] (95%, {RESAMPLES} resamples over \
         {} positions)",
        args.nodes,
        report.ratio,
        report.interval.0,
        report.interval.1,
        report.pairs.len()
    );
    match &report.matched {
        None => println!(
            "the interval includes 1: both play at the gate budget\nsprt control: --nodes {}",
            args.nodes
        ),
        Some(m) => println!(
            "candidate at {} against baseline at {}: {:.5} [{:.5}, {:.5}]\n\
             sprt control: --candidate-nodes {} --baseline-nodes {}",
            m.budget, args.nodes, m.ratio, m.interval.0, m.interval.1, m.budget, args.nodes
        ),
    }
    println!("toolchain: {toolchain}");
    println!("log: {}", log_path.display());
    Ok(())
}

/// The position pair `pair` is measured at, as `(plies, position)`: the
/// opening itself, or the position a game of the baseline against itself
/// reached [`PLIES_PER_STEP`] × (`pair` mod [`POSITION_STEPS`]) plies past it.
fn sample(
    args: &Args,
    baseline: &EngineConfig,
    pair: u64,
    opening: &str,
) -> Result<(usize, String), String> {
    let root =
        rinsai_game::Game::from_usi_position(opening).map_err(|e| format!("{opening}: {e}"))?;
    let target = root.ply() + target_offset(pair);
    if target == root.ply() {
        return Ok((root.ply(), opening.to_owned()));
    }
    let launch = |seat: &str| {
        UsiEngine::launch(
            &format!("{} as {seat}", baseline.id),
            &baseline.path,
            &baseline.args,
            &baseline.options,
        )
        .map_err(|e| format!("launching {}: {e}", baseline.id))
    };
    let (mut black, mut white) = (launch("black")?, launch("white")?);
    let control = MatchControl::Nodes {
        black: args.nodes,
        white: args.nodes,
        hang_timeout: args.move_timeout,
    };
    let record = referee::play_game(
        &mut black,
        &mut white,
        opening,
        target + END_MARGIN + 1,
        control,
    )?;
    if record.reason != EndReason::MaxMoves && !is_result(record.reason) {
        return Err(format!(
            "the baseline's game from {opening} ended abnormally: {:?} {}",
            record.reason,
            record.detail.unwrap_or_default()
        ));
    }
    position_at(opening, &record.moves_usi, record.plies, target)
}

/// How many plies past its opening pair `pair` is measured at.
fn target_offset(pair: u64) -> usize {
    PLIES_PER_STEP * usize::try_from(pair % POSITION_STEPS).expect("a small step")
}

/// Whether `reason` is an ending the game produced rather than a failure.
fn is_result(reason: EndReason) -> bool {
    matches!(
        reason,
        EndReason::Checkmate
            | EndReason::Resign
            | EndReason::Declaration
            | EndReason::Sennichite
            | EndReason::PerpetualCheck
    )
}

/// The position `target` plies from `opening`'s root along a game of
/// `plies` plies whose moves, the opening's included, are `moves_usi`, as
/// `(plies, position)` — or the opening itself when the game ended within
/// [`END_MARGIN`] plies of `target`.
fn position_at(
    opening: &str,
    moves_usi: &str,
    plies: usize,
    target: usize,
) -> Result<(usize, String), String> {
    let mut game =
        rinsai_game::Game::from_usi_position(opening).map_err(|e| format!("{opening}: {e}"))?;
    let from = game.ply();
    if plies <= target + END_MARGIN {
        return Ok((from, opening.to_owned()));
    }
    let moves: Vec<&str> = moves_usi.split_whitespace().collect();
    for &token in moves
        .get(from..target)
        .ok_or("the game is shorter than it says")?
    {
        game.play_usi(token)
            .map_err(|e| format!("`{token}` after {opening}: {e}"))?;
    }
    Ok((target, game.to_usi_position()))
}

/// Instructions one process spends from launch to exit, answering one `go`
/// from `position`.
fn count(
    engine: &EngineConfig,
    position: &str,
    spec: GoSpec,
    move_timeout: Duration,
) -> Result<u64, String> {
    let mut args = vec![
        "--tool=callgrind".to_owned(),
        "--callgrind-out-file=/dev/null".to_owned(),
        engine.path.display().to_string(),
    ];
    args.extend(engine.args.iter().cloned());
    let failed = |e: String| format!("{} under callgrind at {position}: {e}", engine.id);
    let mut usi = UsiEngine::launch(
        &format!("{} under callgrind", engine.id),
        Path::new("valgrind"),
        &args,
        &engine.options,
    )
    .map_err(|e| failed(e.to_string()))?;
    usi.go(position, spec)
        .answer
        .map_err(|e| failed(e.to_string()))?;
    let stderr = usi
        .quit_and_wait(move_timeout)
        .map_err(|e| failed(e.to_string()))?;
    collected(&stderr).ok_or_else(|| failed("no `Collected :` line on stderr".to_owned()))
}

/// What a run cost above its binary's setup run.
fn net(total: u64, setup: u64, id: &str) -> Result<u64, String> {
    total
        .checked_sub(setup)
        .ok_or_else(|| format!("{id}: a run counted {total}, below its setup run's {setup}"))
}

/// The count in callgrind's exit summary, `==pid== Collected : N`.
fn collected(stderr: &[String]) -> Option<u64> {
    stderr.iter().rev().find_map(|line| {
        let (_, count) = line.split_once("Collected :")?;
        count.trim().parse().ok()
    })
}

/// The `rustc version …` string a Rust binary carries in its `.comment`
/// section.
fn rustc_version(bytes: &[u8]) -> Option<String> {
    const MARK: &[u8] = b"rustc version ";
    let start = bytes.windows(MARK.len()).position(|w| w == MARK)?;
    let tail = &bytes[start..];
    let end = tail.iter().position(|&b| b == 0 || b == b'\n')?;
    String::from_utf8(tail[..end].to_vec()).ok()
}

/// The one toolchain both binaries were built by, or why there is not one.
fn same_toolchain(candidate: &Path, baseline: &Path) -> Result<String, String> {
    let version = |path: &Path| {
        let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
        rustc_version(&bytes).ok_or_else(|| format!("{}: no rustc version in it", path.display()))
    };
    let (a, b) = (version(candidate)?, version(baseline)?);
    if a != b {
        return Err(format!(
            "the two binaries come from different toolchains, which count different \
             instructions for the same code: {} has `{a}`, {} has `{b}`",
            candidate.display(),
            baseline.display()
        ));
    }
    Ok(a)
}

/// Σ candidate / Σ baseline over `(candidate, baseline)` pairs.
fn ratio(pairs: &[(u64, u64)]) -> f64 {
    let sum = |f: fn(&(u64, u64)) -> u64| pairs.iter().map(f).map(u128::from).sum::<u128>();
    sum(|p| p.0) as f64 / sum(|p| p.1) as f64
}

/// The 95% interval of [`ratio`], by percentile over `resamples` resamples of
/// the pairs drawn with replacement.
fn bootstrap(pairs: &[(u64, u64)], resamples: usize, seed: u64) -> (f64, f64) {
    let mut state = seed;
    let n = pairs.len() as u64;
    let mut ratios: Vec<f64> = (0..resamples)
        .map(|_| {
            let (mut c, mut b) = (0u128, 0u128);
            for _ in 0..n {
                let (cand, base) = pairs[(rng::splitmix64(&mut state) % n) as usize];
                c += u128::from(cand);
                b += u128::from(base);
            }
            c as f64 / b as f64
        })
        .collect();
    ratios.sort_by(f64::total_cmp);
    let at = |q: f64| ratios[((ratios.len() - 1) as f64 * q).round() as usize];
    (at(0.025), at(0.975))
}

/// The candidate's node budget: `nodes` divided by the interval's upper end and
/// rounded down when the interval excludes 1, and `nodes` when it does not.
///
/// The upper end in both directions, because it is the end that leaves the
/// candidate the fewer nodes.
fn matched_budget(nodes: u64, (lo, hi): (f64, f64)) -> u64 {
    if lo <= 1.0 && 1.0 <= hi {
        return nodes;
    }
    (nodes as f64 / hi).floor() as u64
}

/// Runs `f` over `items` on up to `workers` threads, in order, stopping at the
/// first error.
fn parallel<T: Sync, R: Send>(
    workers: usize,
    items: &[T],
    f: impl Fn(&T) -> Result<R, String> + Sync,
) -> Result<Vec<R>, String> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<Result<R, String>>>> =
        Mutex::new((0..items.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(items.len()).max(1) {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else { break };
                    let result = f(item);
                    let failed = result.is_err();
                    results.lock().expect("no panics hold this lock")[index] = Some(result);
                    if failed {
                        next.store(items.len(), Ordering::Relaxed);
                    }
                }
            });
        }
    });
    let results = results.into_inner().expect("no panics hold this lock");
    let mut out = Vec::with_capacity(items.len());
    for result in results {
        match result {
            Some(Ok(r)) => out.push(r),
            Some(Err(e)) => return Err(e),
            None => return Err("a run stopped before every item was measured".to_owned()),
        }
    }
    Ok(out)
}

fn write_log(
    args: &Args,
    candidate: &EngineConfig,
    baseline: &EngineConfig,
    toolchain: &str,
    report: &Report,
) -> Result<PathBuf, String> {
    use std::io::Write as _;
    let dir = PathBuf::from("logs").join(format!(
        "instructions-{}-{}-{}-vs-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        std::process::id(),
        args.candidate,
        args.baseline
    ));
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    let path = dir.join("counts.jsonl");
    let mut log =
        std::fs::File::create(&path).map_err(|e| format!("create {}: {e}", path.display()))?;
    let mut lines = vec![
        JsonObject::new()
            .str("type", "params")
            .str("candidate", &args.candidate)
            .str("candidate_path", &candidate.path.display().to_string())
            .str("baseline", &args.baseline)
            .str("baseline_path", &baseline.path.display().to_string())
            .str("toolchain", toolchain)
            .str("openings", &args.openings_path.display().to_string())
            .u64("positions", args.positions as u64)
            .u64("seed", args.seed)
            .u64("nodes", args.nodes)
            .finish(),
    ];
    for (i, (s, &(cand, base))) in report.samples.iter().zip(&report.pairs).enumerate() {
        let mut line = JsonObject::new()
            .str("type", "position")
            .u64("pair", s.pair)
            .u64("opening", s.opening as u64)
            .u64("plies", s.plies as u64)
            .str("position", &s.position)
            .u64("baseline", base)
            .u64("candidate", cand);
        if let Some(m) = &report.matched {
            line = line.u64("candidate_matched", m.counts[i]);
        }
        lines.push(line.finish());
    }
    let mut summary = JsonObject::new()
        .str("type", "summary")
        .f64("ratio", report.ratio)
        .f64("ratio_lo", report.interval.0)
        .f64("ratio_hi", report.interval.1)
        .u64("baseline_nodes", args.nodes);
    summary = match &report.matched {
        None => summary.u64("candidate_nodes", args.nodes),
        Some(m) => summary
            .u64("candidate_nodes", m.budget)
            .f64("matched_ratio", m.ratio)
            .f64("matched_lo", m.interval.0)
            .f64("matched_hi", m.interval.1),
    };
    lines.push(summary.finish());
    for line in lines {
        writeln!(log, "{line}").map_err(|e| format!("write {}: {e}", path.display()))?;
    }
    Ok(path)
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut candidate = None;
    let mut baseline = None;
    let mut parsed = Args {
        candidate: String::new(),
        baseline: String::new(),
        config_path: PathBuf::from("tools/opponents.toml"),
        openings_path: PathBuf::from("positions/openings-v3.sfen"),
        positions: DEFAULT_POSITIONS,
        seed: 1,
        nodes: DEFAULT_NODES,
        concurrency: 1,
        move_timeout: DEFAULT_MOVE_TIMEOUT,
    };
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().ok_or_else(|| format!("`{arg}` wants a value"));
        let mut whole = || -> Result<usize, String> {
            usize::try_from(parse_u64(value()?)?).map_err(|e| e.to_string())
        };
        match arg.as_str() {
            "--candidate" => candidate = Some(value()?.clone()),
            "--baseline" => baseline = Some(value()?.clone()),
            "--config" => parsed.config_path = PathBuf::from(value()?),
            "--openings" => parsed.openings_path = PathBuf::from(value()?),
            "--positions" => parsed.positions = whole()?,
            "--seed" => parsed.seed = parse_u64(value()?)?,
            "--nodes" => parsed.nodes = parse_u64(value()?)?,
            "--concurrency" => parsed.concurrency = whole()?,
            "--move-timeout-ms" => {
                parsed.move_timeout = Duration::from_millis(parse_u64(value()?)?);
            }
            other => return Err(format!("unknown argument `{other}`")),
        }
    }
    if parsed.positions == 0 || parsed.concurrency == 0 || parsed.nodes == 0 {
        return Err("--positions, --concurrency and --nodes want at least 1".to_owned());
    }
    parsed.candidate = candidate.ok_or("--candidate is required")?;
    parsed.baseline = baseline.ok_or("--baseline is required")?;
    Ok(parsed)
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo run --release -p xtask -- instructions --candidate <id> --baseline <id> \\\n\
         \x20   [--config tools/opponents.toml] [--openings positions/openings-v3.sfen] \\\n\
         \x20   [--positions {DEFAULT_POSITIONS}] [--seed 1] [--nodes {DEFAULT_NODES}] \\\n\
         \x20   [--concurrency 1] [--move-timeout-ms {}]",
        DEFAULT_MOVE_TIMEOUT.as_millis()
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 8a's gate: an upper end of 1.47492 gave 203 400 nodes.
    #[test]
    fn an_interval_above_one_divides_the_budget_by_its_upper_end() {
        assert_eq!(matched_budget(300_000, (1.400, 1.47492)), 203_400);
    }

    #[test]
    fn an_interval_below_one_raises_the_budget_by_its_upper_end() {
        assert_eq!(matched_budget(300_000, (0.8, 0.96)), 312_500);
    }

    #[test]
    fn an_interval_that_includes_one_leaves_the_budget_alone() {
        for interval in [(0.99, 1.01), (1.0, 1.2), (0.8, 1.0)] {
            assert_eq!(matched_budget(300_000, interval), 300_000, "{interval:?}");
        }
    }

    #[test]
    fn the_count_is_read_off_callgrinds_exit_summary() {
        let stderr = [
            "==42== Callgrind, a call-graph generating cache profiler".to_owned(),
            "==42== Events    : Ir".to_owned(),
            "==42== Collected : 1610380229".to_owned(),
            "==42== I   refs:      1,610,380,229".to_owned(),
        ];
        assert_eq!(collected(&stderr), Some(1_610_380_229));
        assert_eq!(collected(&stderr[..2]), None);
    }

    #[test]
    fn the_toolchain_is_read_out_of_the_comment_section() {
        let bytes = b"\x7fELF\0junk\0rustc version 1.97.0 (2d8144b78 2026-07-07)\0GCC: (x) 13\0";
        assert_eq!(
            rustc_version(bytes).as_deref(),
            Some("rustc version 1.97.0 (2d8144b78 2026-07-07)")
        );
        assert_eq!(rustc_version(b"no version here\0"), None);
    }

    #[test]
    fn the_interval_is_a_function_of_its_seed() {
        let pairs: Vec<(u64, u64)> = (1..=40)
            .map(|i| (100 + i * 7 % 13, 100 + i * 5 % 11))
            .collect();
        assert_eq!(bootstrap(&pairs, 500, 3), bootstrap(&pairs, 500, 3));
        let (lo, hi) = bootstrap(&pairs, 500, 3);
        let point = ratio(&pairs);
        assert!(lo <= point && point <= hi, "{lo} {point} {hi}");
    }

    #[test]
    fn identical_counts_give_a_ratio_of_exactly_one() {
        let pairs = vec![(5_000, 5_000), (7_000, 7_000), (9_000, 9_000)];
        assert_eq!(ratio(&pairs), 1.0);
        assert_eq!(bootstrap(&pairs, 100, 1), (1.0, 1.0));
    }

    #[test]
    fn every_fifth_pair_is_measured_at_its_opening() {
        let offsets: Vec<usize> = (0..10).map(target_offset).collect();
        assert_eq!(offsets, [0, 20, 40, 60, 80, 0, 20, 40, 60, 80]);
    }

    /// A game that ended within [`END_MARGIN`] plies of the target gives the
    /// opening; one that ran past it gives the position at the target.
    #[test]
    fn a_game_that_ends_near_its_target_gives_the_opening() {
        let opening = "startpos moves 7g7f 3c3d";
        let moves = "7g7f 3c3d 2g2f 8c8d 2f2e 8d8e 6i7h 4a3b";
        assert_eq!(
            position_at(opening, moves, 8, 4).expect("replays"),
            (2, opening.to_owned())
        );
        let (plies, position) = position_at(opening, moves, 9, 4).expect("replays");
        assert_eq!(plies, 4);
        assert_eq!(position, "startpos moves 7g7f 3c3d 2g2f 8c8d");
    }
}
