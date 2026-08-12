//! The extractor's reproducibility gate, at fixture scale: the committed
//! records in `tests/fixtures/floodgate/` run through the whole pipeline and
//! must reproduce `tests/fixtures/expected-openings.sfen` byte for byte.
//!
//! The full-scale form of the same claim — regenerating
//! `positions/openings-v1.sfen` from the local cache at the frozen rev and
//! seed leaves `git diff` empty — needs that cache and is run by hand;
//! PROGRESS.md records each run. This test is the half CI can hold.

use xtask::openings::{DayInput, PipelineConfig, generate};

fn fixture_days() -> Vec<DayInput> {
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/floodgate");
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
        ..PipelineConfig::frozen_v1()
    }
}

/// Regenerate the golden file with `BLESS=1 cargo test -p xtask`; the diff is
/// then reviewed like code.
#[test]
fn the_pipeline_reproduces_the_committed_golden_output_byte_for_byte() {
    let generated =
        generate(&fixture_days(), &fixture_config(), "fixture").expect("the fixtures suffice");
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
    let first = generate(&days, &cfg, "fixture").expect("the fixtures suffice");
    let second = generate(&days, &cfg, "fixture").expect("the fixtures suffice");
    assert_eq!(first, second);
}

/// The seed genuinely drives the pick — a pipeline that ignored it would
/// pass the byte-identity tests above trivially.
#[test]
fn a_different_seed_selects_a_different_subset() {
    let days = fixture_days();
    let cfg = fixture_config();
    let baseline = generate(&days, &cfg, "fixture").expect("the fixtures suffice");
    let other_cfg = PipelineConfig {
        seed: cfg.seed + 1,
        ..cfg
    };
    let other = generate(&days, &other_cfg, "fixture").expect("the fixtures suffice");
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
