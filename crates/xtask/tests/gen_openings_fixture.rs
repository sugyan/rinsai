//! The extractor's reproducibility gate, at fixture scale: the committed
//! records in `tests/fixtures/floodgate/` run through the whole pipeline and
//! must reproduce `tests/fixtures/expected-openings.sfen` byte for byte.
//!
//! The full-scale form of the same claim — regenerating a committed set
//! leaves `git diff` empty — is run by hand, from the rev its header
//! records. This test is the half CI holds.

use xtask::openings::{DayInput, PipelineConfig, generate};

fn fixture_days() -> Vec<DayInput> {
    days_under(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/floodgate"
    ))
}

fn days_under(root: &str) -> Vec<DayInput> {
    let mut days: Vec<DayInput> = Vec::new();
    let mut labels: Vec<String> = std::fs::read_dir(root)
        .expect("fixture root exists")
        .map(|e| {
            e.expect("readable")
                .file_name()
                .into_string()
                .expect("UTF-8")
        })
        .collect();
    labels.sort();
    for label in labels {
        let dir = format!("{root}/{label}");
        let mut files: Vec<(String, String)> = std::fs::read_dir(&dir)
            .expect("fixture day exists")
            .filter_map(|e| {
                let path = e.expect("readable").path();
                if path.extension().is_some_and(|x| x == "csa") {
                    let name = path.file_name()?.to_str()?.to_owned();
                    let text = std::fs::read_to_string(&path).expect("fixtures are UTF-8");
                    Some((name, text))
                } else {
                    None
                }
            })
            .collect();
        files.sort_by(|a, b| a.0.cmp(&b.0));
        days.push(DayInput { label, files });
    }
    days
}

/// Small enough for CI, exercising every pass: the fixture set holds seven
/// qualifying games and one reject per filter arm, so a target of four picks
/// from a real surplus. Depth 3 because the suite runs in the dev profile,
/// where the search is an order slower than release.
fn fixture_config() -> PipelineConfig {
    PipelineConfig {
        target: 4,
        balance_depth: 3,
        ..PipelineConfig::frozen_v2()
    }
}

/// Regenerate the golden file with `BLESS=1 cargo test -p xtask`; the diff is
/// then reviewed like code.
#[test]
fn the_pipeline_reproduces_the_committed_golden_output_byte_for_byte() {
    let generated = generate(&fixture_days(), &fixture_config(), "fixture")
        .expect("the fixtures suffice")
        .0;
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/expected-openings.sfen"
    );
    if std::env::var_os("BLESS").is_some() {
        std::fs::write(path, &generated).expect("golden file is writable");
    }
    let expected = std::fs::read_to_string(path).expect("golden file is committed");
    assert_eq!(generated, expected);
}

#[test]
fn two_runs_in_one_process_emit_identical_bytes() {
    let days = fixture_days();
    let cfg = fixture_config();
    let first = generate(&days, &cfg, "fixture")
        .expect("the fixtures suffice")
        .0;
    let second = generate(&days, &cfg, "fixture")
        .expect("the fixtures suffice")
        .0;
    assert_eq!(first, second);
}

/// The seed genuinely drives the pick — a pipeline that ignored it would
/// pass the byte-identity tests above trivially.
#[test]
fn a_different_seed_selects_a_different_subset() {
    let days = fixture_days();
    let cfg = fixture_config();
    let baseline = generate(&days, &cfg, "fixture")
        .expect("the fixtures suffice")
        .0;
    let other_cfg = PipelineConfig {
        seed: cfg.seed + 1,
        ..cfg
    };
    let other = generate(&days, &other_cfg, "fixture")
        .expect("the fixtures suffice")
        .0;
    let openings = |text: &str| {
        text.lines()
            .filter(|l| l.starts_with("startpos"))
            .map(String::from)
            .collect::<Vec<_>>()
    };
    assert_ne!(
        openings(&baseline),
        openings(&other),
        "a neighbouring seed picked the same four openings — the pool is too small \
         or the seed is unused"
    );
}

/// ⚠️ **The corpus above exercises none of the balance filter's own rule.**
/// Its searches reach the node cap often enough, but on every one of its
/// candidates the interrupted iteration's score and the last completed one
/// agree — so reading either gives the same file, and the golden test is
/// blind to which is read. Divergence is rare (six of `openings-v1`'s 256
/// lines at the frozen settings), so it has to be chosen rather than hoped
/// for; this corpus is one floodgate game picked because its ply-12 position
/// diverges, and the window is pinned to that ply so nothing else competes.
///
/// At depth 3 under a 5 000-node cap the position scores **0 on the depth-2
/// iteration it completes** and **−1380 on the depth-3 iteration the cap
/// interrupts** — one inside ±100 cp and one far outside, so the two rules
/// disagree about whether to keep it at all.
///
/// Sabotage: score the candidate from the last line published instead of from
/// `completed_depth`, and `generate` fails with "only 0 of the 1 unique
/// candidates passed the balance filter".
#[test]
fn a_score_from_an_interrupted_iteration_does_not_decide_a_candidate() {
    let cfg = PipelineConfig {
        window: 12..=12,
        target: 1,
        balance_depth: 3,
        balance_node_cap: 5_000,
        ..PipelineConfig::frozen_v2()
    };
    let days = days_under(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/floodgate-capped"
    ));
    let generated = generate(&days, &cfg, "fixture")
        .expect("the one candidate is kept")
        .0;

    let picked: Vec<&str> = generated
        .lines()
        .filter(|l| l.starts_with("# 2026-"))
        .collect();
    assert_eq!(picked.len(), 1, "{generated}");
    assert!(
        picked[0].ends_with("ply=12 eval=+0 d=2"),
        "the line was kept, but not on the score this test is about: {}",
        picked[0]
    );
}
