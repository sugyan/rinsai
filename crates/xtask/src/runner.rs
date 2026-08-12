//! The `sprt` subcommand: colour-swapped pairs from the opening set, fresh
//! engine processes per game, pentanomial GSPRT over the pair outcomes.
//!
//! Fixed-node games between deterministic engines are load-immune, which is
//! what licenses `--concurrency` and running beside other work (CLAUDE.md
//! §3); nothing here reads a wall clock except the hang-detection timeouts.
//! Determinism per game comes from fresh processes: no table state, no
//! option drift, nothing carried between games.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::Duration;

use crate::config::{self, EngineConfig};
use crate::jsonl::JsonObject;
use crate::referee::{self, GameRecord, MAX_GAME_PLIES, Seat, Winner};
use crate::schedule::{self, GamePlan};
use crate::sprt::{self, PairCounts, Status};
use crate::usi::{BestmoveAnswer, EngineError, UsiEngine};

const DEFAULT_MOVE_TIMEOUT: Duration = Duration::from_secs(30);
const DEFAULT_MAX_PAIRS: u64 = 2_000;

/// One engine behind a fixed node budget — the [`Seat`] the referee plays.
struct EngineSeat {
    engine: UsiEngine,
    nodes: u64,
    timeout: Duration,
}

impl Seat for EngineSeat {
    fn bestmove(&mut self, position_args: &str) -> Result<BestmoveAnswer, EngineError> {
        self.engine
            .bestmove(position_args, self.nodes, self.timeout)
    }
}

struct Args {
    candidate: String,
    baseline: String,
    config_path: PathBuf,
    openings_path: PathBuf,
    candidate_nodes: u64,
    baseline_nodes: u64,
    elo: (f64, f64),
    alpha: f64,
    beta: f64,
    /// `Some(n)`: play exactly n pairs, no early stop. `None`: sequential,
    /// stopping on a bound or at `max_pairs`.
    pairs: Option<u64>,
    max_pairs: u64,
    seed: u64,
    concurrency: usize,
    move_timeout: Duration,
}

struct PairOutcome {
    plan: GamePlan,
    games: [GameRecord; 2],
    half_points: usize,
}

pub fn run(args: &[String]) -> ExitCode {
    let args = match parse_args(args) {
        Ok(args) => args,
        Err(e) => {
            eprintln!("sprt: {e}");
            return usage();
        }
    };

    let engines = match config::load(&args.config_path) {
        Ok(engines) => engines,
        Err(e) => {
            eprintln!("sprt: {e}");
            return ExitCode::FAILURE;
        }
    };
    let Some(candidate) = engines.iter().find(|e| e.id == args.candidate) else {
        eprintln!(
            "sprt: no engine `{}` in {}",
            args.candidate,
            args.config_path.display()
        );
        return ExitCode::FAILURE;
    };
    let Some(baseline) = engines.iter().find(|e| e.id == args.baseline) else {
        eprintln!(
            "sprt: no engine `{}` in {}",
            args.baseline,
            args.config_path.display()
        );
        return ExitCode::FAILURE;
    };

    let openings = match load_openings(&args.openings_path) {
        Ok(openings) => openings,
        Err(e) => {
            eprintln!("sprt: {e}");
            return ExitCode::FAILURE;
        }
    };

    let budget = args.pairs.unwrap_or(args.max_pairs);
    let plans: Vec<[GamePlan; 2]> = schedule::pairings(openings.len(), args.seed)
        .take(budget as usize)
        .collect();

    let log_dir = PathBuf::from("logs").join(format!(
        "sprt-{}-{}-vs-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
        args.candidate,
        args.baseline
    ));
    if let Err(e) = std::fs::create_dir_all(&log_dir) {
        eprintln!("sprt: create {}: {e}", log_dir.display());
        return ExitCode::FAILURE;
    }
    let log_path = log_dir.join("run.jsonl");
    let mut log = match std::fs::File::create(&log_path) {
        Ok(file) => file,
        Err(e) => {
            eprintln!("sprt: create {}: {e}", log_path.display());
            return ExitCode::FAILURE;
        }
    };
    {
        use std::io::Write as _;
        let params = JsonObject::new()
            .str("type", "params")
            .str("candidate", &args.candidate)
            .str("candidate_path", &candidate.path.display().to_string())
            .u64("candidate_nodes", args.candidate_nodes)
            .str("baseline", &args.baseline)
            .str("baseline_path", &baseline.path.display().to_string())
            .u64("baseline_nodes", args.baseline_nodes)
            .str("openings", &args.openings_path.display().to_string())
            .u64("opening_count", openings.len() as u64)
            .f64("elo0", args.elo.0)
            .f64("elo1", args.elo.1)
            .f64("alpha", args.alpha)
            .f64("beta", args.beta)
            .u64("seed", args.seed)
            .u64("budget_pairs", budget)
            .bool("fixed_length", args.pairs.is_some())
            .u64("concurrency", args.concurrency as u64)
            .finish();
        let _ = writeln!(log, "{params}");
    }

    let next = AtomicUsize::new(0);
    let stop = AtomicBool::new(false);
    let (tx, rx) = std::sync::mpsc::channel::<Result<PairOutcome, String>>();

    let mut counts = PairCounts::default();
    let mut game_tally = [0u64; 3]; // candidate wins, draws, losses
    let mut fatal: Option<String> = None;

    std::thread::scope(|scope| {
        for _ in 0..args.concurrency {
            let tx = tx.clone();
            let (next, stop, plans, openings) = (&next, &stop, &plans, &openings);
            let (candidate, baseline, args) = (candidate, baseline, &args);
            scope.spawn(move || {
                loop {
                    if stop.load(Ordering::Relaxed) {
                        break;
                    }
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(pair) = plans.get(index) else { break };
                    let outcome = play_pair(pair, openings, candidate, baseline, args);
                    if tx.send(outcome).is_err() {
                        break;
                    }
                }
            });
        }
        drop(tx);

        let (lower, upper) = sprt::bounds(args.alpha, args.beta);
        let mut decided: Option<Status> = None;
        for outcome in rx {
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(e) => {
                    // A refereeing failure is a harness bug, not a game
                    // result; stop issuing pairs and surface it.
                    stop.store(true, Ordering::Relaxed);
                    fatal.get_or_insert(e);
                    continue;
                }
            };
            counts.record(outcome.half_points);
            for (game_index, record) in outcome.games.iter().enumerate() {
                let candidate_black = game_index == 0;
                match candidate_points(record, candidate_black) {
                    2 => game_tally[0] += 1,
                    1 => game_tally[1] += 1,
                    _ => game_tally[2] += 1,
                }
                log_game(&mut log, &outcome.plan, game_index, record, candidate_black);
            }
            let llr = sprt::llr(&counts, args.elo.0, args.elo.1);
            println!(
                "pair {:>4} opening {:>3} [{}{}] | pent {} | score {:>5.1}% | llr {:+.3} in [{:+.3}, {:+.3}]",
                outcome.plan.pair,
                outcome.plan.opening,
                letter(&outcome.games[0], true),
                letter(&outcome.games[1], false),
                counts
                    .counts
                    .iter()
                    .map(|c| c.to_string())
                    .collect::<Vec<_>>()
                    .join("/"),
                counts.score() * 100.0,
                llr,
                lower,
                upper,
            );
            if args.pairs.is_none() && decided.is_none() {
                let status = sprt::status(llr, args.alpha, args.beta);
                if status != Status::Continue {
                    decided = Some(status);
                    // In-flight pairs still land and count; no new ones start.
                    stop.store(true, Ordering::Relaxed);
                }
            }
        }
    });

    if let Some(e) = fatal {
        eprintln!("sprt: {e}");
        return ExitCode::FAILURE;
    }

    let llr = sprt::llr(&counts, args.elo.0, args.elo.1);
    let verdict = if args.pairs.is_some() {
        "fixed-length match complete".to_owned()
    } else {
        match sprt::status(llr, args.alpha, args.beta) {
            Status::AcceptH1 => format!("H1 accepted (elo >= {})", args.elo.1),
            Status::AcceptH0 => format!("H0 accepted (elo <= {})", args.elo.0),
            Status::Continue => format!("inconclusive at the {}-pair cap", args.max_pairs),
        }
    };
    println!("---");
    println!("{verdict}");
    println!(
        "pairs {} | games {} | candidate W-D-L {}-{}-{} | pent {:?} | llr {:+.3} | score {:.2}% (elo {:+.1} est)",
        counts.pairs(),
        counts.pairs() * 2,
        game_tally[0],
        game_tally[1],
        game_tally[2],
        counts.counts,
        llr,
        counts.score() * 100.0,
        sprt::elo_estimate(counts.score()),
    );
    println!("log: {}", log_path.display());
    ExitCode::SUCCESS
}

/// One colour-swapped pair: two games, fresh processes for each.
fn play_pair(
    pair: &[GamePlan; 2],
    openings: &[String],
    candidate: &EngineConfig,
    baseline: &EngineConfig,
    args: &Args,
) -> Result<PairOutcome, String> {
    let opening = &openings[pair[0].opening];
    let mut games: Vec<GameRecord> = Vec::with_capacity(2);
    for plan in pair {
        let (black_cfg, black_nodes, white_cfg, white_nodes) = if plan.candidate_is_black {
            (
                candidate,
                args.candidate_nodes,
                baseline,
                args.baseline_nodes,
            )
        } else {
            (
                baseline,
                args.baseline_nodes,
                candidate,
                args.candidate_nodes,
            )
        };
        let launch = |cfg: &EngineConfig, seat_name: &str, nodes: u64| {
            UsiEngine::launch(
                &format!("{} as {seat_name}", cfg.id),
                &cfg.path,
                &cfg.args,
                &cfg.options,
            )
            .map(|engine| EngineSeat {
                engine,
                nodes,
                timeout: args.move_timeout,
            })
            .map_err(|e| format!("launching {}: {e}", cfg.id))
        };
        let mut black = launch(black_cfg, "black", black_nodes)?;
        let mut white = launch(white_cfg, "white", white_nodes)?;
        let mut record = referee::play_game(&mut black, &mut white, opening, MAX_GAME_PLIES)?;
        if record.detail.is_some() {
            // The offender's stderr is the post-mortem; attach the tail of
            // the side that lost abnormally.
            let offender = match (record.winner, record.reason) {
                (Winner::White, _) => Some(&black.engine),
                (Winner::Black, _) => Some(&white.engine),
                (Winner::Neither, _) => None,
            };
            if let Some(engine) = offender {
                let tail = engine.stderr_tail();
                if !tail.is_empty() {
                    let joined = tail.join(" | ");
                    record.detail = record.detail.map(|d| format!("{d}; stderr: {joined}"));
                }
            }
        }
        let result_black = match record.winner {
            Winner::Black => ("win", "lose"),
            Winner::White => ("lose", "win"),
            Winner::Neither => ("draw", "draw"),
        };
        black.engine.gameover(result_black.0);
        white.engine.gameover(result_black.1);
        games.push(record);
    }
    let games: [GameRecord; 2] = games.try_into().expect("two games were pushed");
    let half_points = candidate_points(&games[0], pair[0].candidate_is_black)
        + candidate_points(&games[1], pair[1].candidate_is_black);
    Ok(PairOutcome {
        plan: pair[0],
        games,
        half_points,
    })
}

/// The candidate's points in one game, in halves: 2 a win, 1 a draw, 0 a
/// loss.
fn candidate_points(record: &GameRecord, candidate_is_black: bool) -> usize {
    match (record.winner, candidate_is_black) {
        (Winner::Neither, _) => 1,
        (Winner::Black, true) | (Winner::White, false) => 2,
        _ => 0,
    }
}

/// The candidate's result letter for the compact per-pair line.
fn letter(record: &GameRecord, candidate_is_black: bool) -> char {
    match candidate_points(record, candidate_is_black) {
        2 => 'W',
        1 => 'D',
        _ => 'L',
    }
}

fn log_game(
    log: &mut std::fs::File,
    plan: &GamePlan,
    game_index: usize,
    record: &GameRecord,
    candidate_black: bool,
) {
    use std::io::Write as _;
    let mut object = JsonObject::new()
        .str("type", "game")
        .u64("pair", plan.pair)
        .u64("game", game_index as u64)
        .u64("opening", plan.opening as u64)
        .bool("candidate_black", candidate_black)
        .str(
            "result",
            match record.winner {
                Winner::Black => "black",
                Winner::White => "white",
                Winner::Neither => "draw",
            },
        )
        .str("reason", &format!("{:?}", record.reason))
        .u64("plies", record.plies as u64)
        .str("moves", &record.moves_usi);
    if let Some(detail) = &record.detail {
        object = object.str("detail", detail);
    }
    let _ = writeln!(log, "{}", object.finish());
}

fn load_openings(path: &PathBuf) -> Result<Vec<String>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let openings: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect();
    if openings.is_empty() {
        return Err(format!("{}: no openings", path.display()));
    }
    // Fail before any engine spawns: every line must be a position the
    // referee can replay.
    for (index, opening) in openings.iter().enumerate() {
        rinsai_game::Game::from_usi_position(opening)
            .map_err(|e| format!("{} line {}: {e}", path.display(), index + 1))?;
    }
    Ok(openings)
}

fn parse_args(args: &[String]) -> Result<Args, String> {
    let mut candidate = None;
    let mut baseline = None;
    let mut config_path = PathBuf::from("tools/opponents.toml");
    let mut openings_path = PathBuf::from("positions/openings-v1.sfen");
    let mut nodes = None;
    let mut candidate_nodes = None;
    let mut baseline_nodes = None;
    let mut elo: Option<(f64, f64)> = None;
    let mut elo0 = None;
    let mut elo1 = None;
    let (mut alpha, mut beta) = (0.05, 0.05);
    let mut pairs = None;
    let mut max_pairs = DEFAULT_MAX_PAIRS;
    let mut seed = 1u64;
    let mut concurrency = 1usize;
    let mut move_timeout = DEFAULT_MOVE_TIMEOUT;

    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let mut value = || iter.next().ok_or_else(|| format!("`{arg}` wants a value"));
        match arg.as_str() {
            "--candidate" => candidate = Some(value()?.clone()),
            "--baseline" => baseline = Some(value()?.clone()),
            "--config" => config_path = PathBuf::from(value()?),
            "--openings" => openings_path = PathBuf::from(value()?),
            "--nodes" => nodes = Some(parse_u64(value()?)?),
            "--candidate-nodes" => candidate_nodes = Some(parse_u64(value()?)?),
            "--baseline-nodes" => baseline_nodes = Some(parse_u64(value()?)?),
            "--gain" => elo = Some((0.0, 5.0)),
            "--non-regression" => elo = Some((-5.0, 0.0)),
            "--elo0" => elo0 = Some(parse_f64(value()?)?),
            "--elo1" => elo1 = Some(parse_f64(value()?)?),
            "--alpha" => alpha = parse_f64(value()?)?,
            "--beta" => beta = parse_f64(value()?)?,
            "--pairs" => pairs = Some(parse_u64(value()?)?),
            "--max-pairs" => max_pairs = parse_u64(value()?)?,
            "--seed" => seed = parse_u64(value()?)?,
            "--concurrency" => {
                concurrency = parse_u64(value()?)? as usize;
                if concurrency == 0 {
                    return Err("--concurrency wants at least 1".to_owned());
                }
            }
            "--move-timeout-ms" => move_timeout = Duration::from_millis(parse_u64(value()?)?),
            other => return Err(format!("unknown argument `{other}`")),
        }
    }

    let elo = match (elo, elo0, elo1) {
        (Some(pair), None, None) => pair,
        (None, Some(e0), Some(e1)) if e0 < e1 => (e0, e1),
        (None, Some(_), Some(_)) => return Err("--elo0 must be below --elo1".to_owned()),
        _ => {
            return Err(
                "pick exactly one of --gain, --non-regression, or both --elo0 and --elo1"
                    .to_owned(),
            );
        }
    };
    let (candidate_nodes, baseline_nodes) = match (nodes, candidate_nodes, baseline_nodes) {
        (Some(n), None, None) => (n, n),
        (None, Some(c), Some(b)) => (c, b),
        _ => {
            return Err(
                "pick --nodes N, or both --candidate-nodes and --baseline-nodes".to_owned(),
            );
        }
    };
    Ok(Args {
        candidate: candidate.ok_or("--candidate is required")?,
        baseline: baseline.ok_or("--baseline is required")?,
        config_path,
        openings_path,
        candidate_nodes,
        baseline_nodes,
        elo,
        alpha,
        beta,
        pairs,
        max_pairs,
        seed,
        concurrency,
        move_timeout,
    })
}

fn parse_u64(text: &str) -> Result<u64, String> {
    text.parse()
        .map_err(|_| format!("`{text}` is not a whole number"))
}

fn parse_f64(text: &str) -> Result<f64, String> {
    text.parse()
        .map_err(|_| format!("`{text}` is not a number"))
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo run --release -p xtask -- sprt --candidate <id> --baseline <id> \\\n\
         \x20   (--nodes N | --candidate-nodes N --baseline-nodes N) \\\n\
         \x20   (--gain | --non-regression | --elo0 E --elo1 E) \\\n\
         \x20   [--config tools/opponents.toml] [--openings positions/openings-v1.sfen] \\\n\
         \x20   [--pairs N] [--max-pairs {DEFAULT_MAX_PAIRS}] [--alpha 0.05] [--beta 0.05] \\\n\
         \x20   [--seed 1] [--concurrency 1] [--move-timeout-ms 30000]"
    );
    ExitCode::FAILURE
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::referee::EndReason;

    fn strings(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| (*s).to_owned()).collect()
    }

    #[test]
    fn the_presets_and_the_explicit_hypotheses_are_mutually_exclusive() {
        let base = ["--candidate", "a", "--baseline", "b", "--nodes", "1000"];
        let with = |extra: &[&str]| {
            let mut v = strings(&base);
            v.extend(strings(extra));
            parse_args(&v)
        };
        assert_eq!(with(&["--gain"]).expect("valid").elo, (0.0, 5.0));
        assert_eq!(with(&["--non-regression"]).expect("valid").elo, (-5.0, 0.0));
        assert_eq!(
            with(&["--elo0", "-3", "--elo1", "3"]).expect("valid").elo,
            (-3.0, 3.0)
        );
        assert!(with(&[]).is_err(), "no hypotheses at all");
        assert!(with(&["--elo0", "3", "--elo1", "-3"]).is_err(), "reversed");
        assert!(with(&["--elo0", "0"]).is_err(), "half a pair");
    }

    #[test]
    fn node_budgets_are_one_flag_or_both_sides_never_a_mixture() {
        let with = |extra: &[&str]| {
            let mut v = strings(&["--candidate", "a", "--baseline", "b", "--gain"]);
            v.extend(strings(extra));
            parse_args(&v)
        };
        let both = with(&["--nodes", "500"]).expect("valid");
        assert_eq!((both.candidate_nodes, both.baseline_nodes), (500, 500));
        let split = with(&["--candidate-nodes", "100", "--baseline-nodes", "9"]).expect("valid");
        assert_eq!((split.candidate_nodes, split.baseline_nodes), (100, 9));
        assert!(with(&[]).is_err());
        assert!(with(&["--nodes", "1", "--candidate-nodes", "2"]).is_err());
    }

    #[test]
    fn the_candidates_points_read_off_the_seat_and_the_winner() {
        let record = |winner| GameRecord {
            winner,
            reason: EndReason::Resign,
            plies: 10,
            moves_usi: String::new(),
            detail: None,
        };
        assert_eq!(candidate_points(&record(Winner::Black), true), 2);
        assert_eq!(candidate_points(&record(Winner::Black), false), 0);
        assert_eq!(candidate_points(&record(Winner::White), false), 2);
        assert_eq!(candidate_points(&record(Winner::Neither), true), 1);
    }
}
