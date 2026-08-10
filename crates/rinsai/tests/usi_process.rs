//! One end-to-end run of the real binary.
//!
//! The dialogue suite drives `usi::run` in-process. This covers what that
//! cannot reach: `main`'s argument handling, the real stdin/stdout wiring, and
//! ⚠️ **whether output is flushed when stdout is a pipe rather than a
//! terminal** — an unflushed `bestmove` is the classic USI engine hang, and it
//! is invisible to every in-process test.

use std::io::{BufRead, BufReader, Read, Write};
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
///
/// It also plays several plies the way a GUI does — waiting for each
/// `bestmove` before sending the next `go` — and then requires stderr to be
/// **empty**. ⚠️ That is the regression test for a bug this suite originally
/// missed and a real game against YaneuraOu found: an engine that clears its
/// "searching" state only on `stop` believes every completed search is still
/// running and warns about a protocol violation on every move. An in-process
/// dialogue cannot catch it, because there the script is consumed faster than
/// the worker answers and a real overlap is indistinguishable from the bug.
#[test]
fn a_scripted_game_over_real_pipes() {
    let mut child = engine()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
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

    // Play several plies as a GUI does: one `go` at a time, each answered
    // before the next is sent. `Game` replays the answers, so every move is
    // checked for legality against a position the test builds itself —
    // generation order is not a stability guarantee, so no move may be named.
    writeln!(stdin, "usinewgame").expect("the engine is listening");
    let mut game = rinsai_search::Game::from_startpos();
    let mut moves: Vec<String> = Vec::new();

    for ply in 0..6 {
        let position = if moves.is_empty() {
            "position startpos".to_owned()
        } else {
            format!("position startpos moves {}", moves.join(" "))
        };
        writeln!(stdin, "{position}").expect("the engine is listening");
        writeln!(stdin, "go btime 1000 wtime 1000 byoyomi 0").expect("the engine is listening");

        let answer = read_until("bestmove", &mut stdout);
        let line = answer.last().expect("read_until returns the marker line");
        let token = line
            .strip_prefix("bestmove ")
            .unwrap_or_else(|| panic!("ply {ply}: `{line}` is not a bestmove line"));
        assert_ne!(token, "resign", "ply {ply}: the opening has legal moves");

        game.push_usi_move(token)
            .unwrap_or_else(|e| panic!("ply {ply}: the engine answered `{token}`: {e}"));
        moves.push(token.to_owned());
    }

    writeln!(stdin, "quit").expect("the engine is listening");
    drop(stdin);
    let status = child.wait().expect("the engine exits");
    assert!(
        status.success(),
        "the engine did not exit cleanly on `quit`"
    );

    let mut stderr = String::new();
    child
        .stderr
        .take()
        .expect("stderr was piped")
        .read_to_string(&mut stderr)
        .expect("stderr is UTF-8");
    assert!(
        stderr.is_empty(),
        "a correctly sequenced game must produce no diagnostics, got:\n{stderr}"
    );
}

/// A non-UTF-8 byte on stdin must not end the engine.
///
/// See `usi::run` for why. ⚠️ This has to be a process test — the bytes never
/// survive a `&str` fixture.
/// Sabotage: restore `for line in input.lines()` and the second `readyok`
/// never arrives.
#[test]
fn a_non_utf8_line_does_not_end_the_session() {
    let mut child = engine()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the engine binary runs");

    let mut stdin = child.stdin.take().expect("stdin was piped");
    // CP932 for 将棋, inside an option value, between two things that must work.
    let mut script: Vec<u8> = b"usi\nisready\nsetoption name EvalFile value C:\\".to_vec();
    script.extend_from_slice(&[0x8f, 0xab, 0x8a, 0xfb]);
    script.extend_from_slice(b"\\eval\nisready\nposition startpos\ngo byoyomi 10\nquit\n");
    stdin.write_all(&script).expect("the engine is listening");
    drop(stdin);

    let output = child.wait_with_output().expect("the engine exits");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "the engine did not exit cleanly");
    assert_eq!(
        stdout.lines().filter(|l| *l == "readyok").count(),
        2,
        "the engine stopped answering after the mis-encoded line:\n{stdout}"
    );
    assert_eq!(
        stdout.lines().filter(|l| l.starts_with("bestmove")).count(),
        1,
        "the engine never moved:\n{stdout}"
    );
    // And it says so, rather than mangling the line in silence.
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("not valid UTF-8"),
        "no diagnostic: {stderr}"
    );
}
