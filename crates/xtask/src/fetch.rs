//! `fetch-floodgate` — mirror per-day floodgate game records into the local
//! cache the openings pipeline reads.
//!
//! One `curl` subprocess per request (no HTTP dependency), 200 ms between
//! network fetches, and a cache hit costs nothing — re-running is free and is
//! how a partial download resumes. Files land as `<name>.part` and are
//! renamed only when curl succeeds, so an interrupted run cannot leave a
//! truncated record that a later run would trust.

use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use std::time::Duration;

const HOST: &str = "https://wdoor.c.u-tokyo.ac.jp/shogi/x";
const PACE: Duration = Duration::from_millis(200);

pub fn run(args: &[String]) -> ExitCode {
    let mut dates: Option<String> = None;
    let mut root = PathBuf::from("data/floodgate");
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        match arg.as_str() {
            "--dates" => dates = iter.next().cloned(),
            "--root" => {
                if let Some(v) = iter.next() {
                    root = PathBuf::from(v);
                }
            }
            other => {
                eprintln!("fetch-floodgate: unknown argument `{other}`");
                return usage();
            }
        }
    }
    let Some(dates) = dates else {
        eprintln!("fetch-floodgate: --dates is required");
        return usage();
    };
    let days = match parse_date_range(&dates) {
        Ok(days) => days,
        Err(e) => {
            eprintln!("fetch-floodgate: {e}");
            return usage();
        }
    };

    let mut failures = 0usize;
    for day in &days {
        match fetch_day(&root, day) {
            Ok((fetched, cached, failed)) => {
                println!(
                    "{day}: {fetched} fetched, {cached} already cached{}",
                    if failed > 0 {
                        failures += failed;
                        format!(", {failed} FAILED")
                    } else {
                        String::new()
                    }
                );
            }
            Err(e) => {
                eprintln!("fetch-floodgate: {day}: {e}");
                failures += 1;
            }
        }
    }
    if failures > 0 {
        eprintln!("fetch-floodgate: {failures} failures — re-run to retry; cache hits are free");
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

fn usage() -> ExitCode {
    eprintln!(
        "usage: cargo run -p xtask -- fetch-floodgate --dates 2026-06-01..2026-06-07 \
         [--root data/floodgate]"
    );
    ExitCode::FAILURE
}

/// One day: fetch the index, then every record not already cached.
/// Returns `(fetched, cached, failed)`.
fn fetch_day(root: &Path, day: &Date) -> Result<(usize, usize, usize), String> {
    let dir = day.cache_dir(root);
    std::fs::create_dir_all(&dir).map_err(|e| format!("create {}: {e}", dir.display()))?;

    let index_url = format!("{HOST}/{:04}/{:02}/{:02}/", day.y, day.m, day.d);
    let index = curl_capture(&index_url)?;
    std::thread::sleep(PACE);

    let (mut fetched, mut cached, mut failed) = (0, 0, 0);
    for name in hrefs(&index) {
        let target = dir.join(&name);
        if target.exists() {
            cached += 1;
            continue;
        }
        let part = dir.join(format!("{name}.part"));
        let url = format!("{index_url}{name}");
        match curl_to_file(&url, &part) {
            Ok(()) => {
                std::fs::rename(&part, &target).map_err(|e| format!("rename {name}: {e}"))?;
                fetched += 1;
            }
            Err(e) => {
                let _ = std::fs::remove_file(&part);
                eprintln!("fetch-floodgate: {url}: {e}");
                failed += 1;
            }
        }
        std::thread::sleep(PACE);
    }
    Ok((fetched, cached, failed))
}

fn curl(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("curl")
        .args(["-sS", "--fail", "-L"])
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                "curl is not installed or not on PATH".to_owned()
            } else {
                format!("spawning curl: {e}")
            }
        })
}

fn curl_capture(url: &str) -> Result<String, String> {
    let out = curl(&[url])?;
    if !out.status.success() {
        return Err(format!(
            "curl exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn curl_to_file(url: &str, target: &Path) -> Result<(), String> {
    let target = target.to_str().ok_or("non-UTF-8 cache path")?;
    let out = curl(&["-o", target, url])?;
    if !out.status.success() {
        return Err(format!(
            "curl exited with {}: {}",
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(())
}

/// The `wdoor…​.csa` names an index page links to, deduplicated and sorted —
/// filename order is the deterministic spine the pipeline scans in.
fn hrefs(index_html: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = index_html;
    while let Some(at) = rest.find("href=\"") {
        rest = &rest[at + 6..];
        let Some(end) = rest.find('"') else { break };
        let name = &rest[..end];
        if name.starts_with("wdoor") && name.ends_with(".csa") && !name.contains('/') {
            names.push(name.to_owned());
        }
        rest = &rest[end..];
    }
    names.sort();
    names.dedup();
    names
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Date {
    y: u16,
    m: u8,
    d: u8,
}

impl Date {
    /// Where this day's records live under the cache root. The layout is the
    /// server's own (`YYYY/MM/DD/`), so a cache path can be read back into
    /// the URL it came from.
    pub(crate) fn cache_dir(&self, root: &Path) -> PathBuf {
        root.join(format!("{:04}", self.y))
            .join(format!("{:02}", self.m))
            .join(format!("{:02}", self.d))
    }
}

impl std::fmt::Display for Date {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:04}-{:02}-{:02}", self.y, self.m, self.d)
    }
}

/// `YYYY-MM-DD` or `YYYY-MM-DD..YYYY-MM-DD`, both ends inclusive.
/// Callers: this module's `run`, and `gen-openings`, which reads the days the
/// fetch wrote.
pub(crate) fn parse_date_range(text: &str) -> Result<Vec<Date>, String> {
    let (from, to) = match text.split_once("..") {
        Some((a, b)) => (parse_date(a)?, parse_date(b)?),
        None => {
            let one = parse_date(text)?;
            (one, one)
        }
    };
    let mut days = Vec::new();
    let mut day = from;
    loop {
        days.push(day);
        if day == to {
            return Ok(days);
        }
        if days.len() > 366 {
            return Err(format!(
                "{text}: more than a year of days — backwards range?"
            ));
        }
        day = next_day(day);
    }
}

fn parse_date(text: &str) -> Result<Date, String> {
    let bad = || format!("`{text}` is not a YYYY-MM-DD date");
    let mut parts = text.split('-');
    let y: u16 = parts.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let m: u8 = parts.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    let d: u8 = parts.next().and_then(|v| v.parse().ok()).ok_or_else(bad)?;
    if parts.next().is_some() || !(1..=12).contains(&m) || d < 1 || d > days_in_month(y, m) {
        return Err(bad());
    }
    Ok(Date { y, m, d })
}

fn days_in_month(y: u16, m: u8) -> u8 {
    match m {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        _ => {
            if y.is_multiple_of(4) && (!y.is_multiple_of(100) || y.is_multiple_of(400)) {
                29
            } else {
                28
            }
        }
    }
}

fn next_day(day: Date) -> Date {
    if day.d < days_in_month(day.y, day.m) {
        Date {
            d: day.d + 1,
            ..day
        }
    } else if day.m < 12 {
        Date {
            m: day.m + 1,
            d: 1,
            ..day
        }
    } else {
        Date {
            y: day.y + 1,
            m: 1,
            d: 1,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_range_is_inclusive_and_crosses_month_and_year_ends() {
        let days = parse_date_range("2026-06-01..2026-06-03").expect("valid");
        assert_eq!(days.len(), 3);
        assert_eq!(days[0].to_string(), "2026-06-01");
        assert_eq!(days[2].to_string(), "2026-06-03");

        let days = parse_date_range("2026-06-30..2026-07-01").expect("valid");
        assert_eq!(days[1].to_string(), "2026-07-01");

        let days = parse_date_range("2025-12-31..2026-01-01").expect("valid");
        assert_eq!(days[1].to_string(), "2026-01-01");

        let one = parse_date_range("2026-06-05").expect("valid");
        assert_eq!(one.len(), 1);
    }

    #[test]
    fn february_knows_about_leap_years() {
        assert!(parse_date_range("2024-02-29").is_ok());
        assert!(parse_date_range("2026-02-29").is_err());
        assert!(parse_date_range("2000-02-29").is_ok());
        assert!(parse_date_range("1900-02-29").is_err(), "century rule");
    }

    #[test]
    fn a_backwards_range_errors_instead_of_spinning() {
        assert!(parse_date_range("2026-06-07..2026-06-01").is_err());
    }

    #[test]
    fn nonsense_dates_are_refused() {
        for text in [
            "",
            "2026",
            "2026-13-01",
            "2026-06-32",
            "2026-06-00",
            "a-b-c",
        ] {
            assert!(parse_date_range(text).is_err(), "{text:?} accepted");
        }
    }

    /// The scanner reads a real wdoor index shape: relative hrefs, `.html`
    /// twins to skip, sorted output.
    #[test]
    fn the_index_scanner_keeps_only_csa_records() {
        let html = r#"<html><body><a href="?C=N;O=D">Name</a>
<a href="wdoor+floodgate-300-10F+b+a+20260601000001.csa">x</a>
<a href="wdoor+floodgate-300-10F+b+a+20260601000001.html">x</a>
<a href="wdoor+floodgate-300-10F+a+b+20260601000000.csa">x</a>
<a href="/shogi/x/2026/06/wdoor+elsewhere.csa">absolute path</a>
<a href="wdoor+floodgate-300-10F+a+b+20260601000000.csa">duplicate</a></body></html>"#;
        assert_eq!(
            hrefs(html),
            vec![
                "wdoor+floodgate-300-10F+a+b+20260601000000.csa".to_owned(),
                "wdoor+floodgate-300-10F+b+a+20260601000001.csa".to_owned(),
            ]
        );
    }
}
