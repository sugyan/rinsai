//! One end-to-end run of the real binary.
//!
//! The dialogue suite drives `usi::run` in-process, which is faster and far
//! more precise. This exists so that the parts it cannot reach stay covered:
//! `main`'s argument handling, the real stdin/stdout wiring, and — the one that
//! actually bites — **whether output is flushed when stdout is a pipe rather
//! than a terminal**. An unflushed `bestmove` is the classic USI engine hang,
//! and it is invisible to every in-process test.

use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

fn engine() -> Command {
    Command::new(env!("CARGO_BIN_EXE_rinsai"))
}

#[test]
fn version_is_reported_and_exits_cleanly() {
    let output = engine()
        .arg("--version")
        .output()
        .expect("the engine binary runs");
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        format!("rinsai {}", env!("CARGO_PKG_VERSION"))
    );
}

/// Reads the answers *as they arrive*, so an engine that only flushed at exit
/// would hang here rather than passing.
#[test]
fn a_scripted_game_over_real_pipes() {
    let mut child = engine()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("the engine binary runs");

    let mut stdin = child.stdin.take().expect("stdin was piped");
    let mut stdout = BufReader::new(child.stdout.take().expect("stdout was piped"));

    let read_until = |marker: &str, stdout: &mut BufReader<_>| -> Vec<String> {
        let mut lines = Vec::new();
        loop {
            let mut line = String::new();
            let read = stdout
                .read_line(&mut line)
                .expect("the engine writes UTF-8");
            assert_ne!(read, 0, "the engine closed stdout before `{marker}`");
            let line = line.trim_end().to_owned();
            let done = line.starts_with(marker);
            lines.push(line);
            if done {
                return lines;
            }
        }
    };

    writeln!(stdin, "usi").expect("the engine is listening");
    let handshake = read_until("usiok", &mut stdout);
    assert_eq!(
        handshake[0],
        format!("id name rinsai {}", env!("CARGO_PKG_VERSION"))
    );

    writeln!(stdin, "isready").expect("the engine is listening");
    read_until("readyok", &mut stdout);

    writeln!(stdin, "position startpos moves 7g7f 3c3d").expect("the engine is listening");
    writeln!(stdin, "go btime 1000 wtime 1000 byoyomi 0").expect("the engine is listening");
    let answer = read_until("bestmove", &mut stdout);
    let bestmove = answer.last().expect("read_until returns the marker line");

    // Legality is checked against a position built here, not against a move
    // name written down — generation order is not a stability guarantee.
    let mut game = rinsai_search::Game::from_usi_position("startpos moves 7g7f 3c3d")
        .expect("the fixture parses");
    let token = bestmove
        .strip_prefix("bestmove ")
        .expect("a bestmove line names a move");
    game.push_usi_move(token)
        .unwrap_or_else(|e| panic!("the engine answered `{token}`, which is not legal: {e}"));

    writeln!(stdin, "quit").expect("the engine is listening");
    drop(stdin);
    assert!(
        child.wait().expect("the engine exits").success(),
        "the engine did not exit cleanly on `quit`"
    );
}
