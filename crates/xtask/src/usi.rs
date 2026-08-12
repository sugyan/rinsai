//! One USI engine process, driven from the GUI side of the protocol.
//!
//! The harness's clocks are hang detectors, not budgets: a timeout converts
//! a wedged engine into a recorded loss, and can never change a move. Fixed
//! nodes (`go nodes N`) are the only search limit ever sent.
//!
//! ⚠️ No orphans: [`UsiEngine`]'s `Drop` quits, waits briefly, then kills and
//! reaps — and it runs on unwinding panics, which the workspace keeps
//! enabled. If the harness itself dies uncleanly, the child's stdin closes
//! and a USI engine exits on EOF. Two engines exist per game and are
//! dropped before the next one starts, so what bounds the total is the
//! caller's worker count.

use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write as _};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// `usi` → `usiok` — instant for every engine in the roster; a hang detector.
pub const USIOK_TIMEOUT: Duration = Duration::from_secs(10);
/// `isready` → `readyok` — generous because engines that load evaluation
/// files are slow exactly here.
pub const READYOK_TIMEOUT: Duration = Duration::from_secs(60);
/// Lines of stderr kept per engine for the post-mortem of an abnormal end.
const STDERR_TAIL_LINES: usize = 200;
/// How long [`UsiEngine::stderr_tail`] waits for a dying engine to finish
/// writing before it reads. Only ever paid on a game that ended abnormally.
const STDERR_SETTLE_STEP: Duration = Duration::from_millis(20);
const STDERR_SETTLE_POLLS: usize = 25;

/// What a `bestmove` line said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BestmoveAnswer {
    /// A move token, exactly as the engine spelled it.
    Move(String),
    Resign,
    /// 入玉宣言 (`bestmove win`).
    Win,
}

#[derive(Debug)]
pub enum EngineError {
    Spawn(std::io::Error),
    /// The engine said nothing matching before the deadline.
    Timeout {
        waiting_for: &'static str,
    },
    /// The engine's stdout closed while waiting.
    Died {
        waiting_for: &'static str,
    },
    /// The engine said something the protocol has no reading for.
    Protocol(String),
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Spawn(e) => write!(f, "spawn: {e}"),
            Self::Timeout { waiting_for } => write!(f, "timed out waiting for {waiting_for}"),
            Self::Died { waiting_for } => {
                write!(f, "engine exited while waiting for {waiting_for}")
            }
            Self::Protocol(line) => write!(f, "unreadable protocol line: {line}"),
        }
    }
}

pub struct UsiEngine {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// For log lines, e.g. `rinsai-dev as black`.
    pub label: String,
}

impl std::fmt::Debug for UsiEngine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsiEngine")
            .field("label", &self.label)
            .finish_non_exhaustive()
    }
}

impl UsiEngine {
    /// Spawn and handshake: `usi`/`usiok`, every configured option in the
    /// given order, `isready`/`readyok`, `usinewgame`. An option name the
    /// engine did not declare is a stderr warning, not an error — the
    /// declared list is how a casing typo surfaces before it silently
    /// changes match conditions.
    pub fn launch(
        label: &str,
        path: &Path,
        args: &[String],
        options: &[(String, String)],
    ) -> Result<Self, EngineError> {
        let mut child = Command::new(path)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(EngineError::Spawn)?;

        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let (line_tx, lines) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else { break };
                if line_tx.send(line).is_err() {
                    break;
                }
            }
        });

        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let tail = Arc::clone(&stderr_tail);
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else { break };
                let mut tail = tail.lock().expect("no panics hold this lock");
                if tail.len() == STDERR_TAIL_LINES {
                    tail.pop_front();
                }
                tail.push_back(line);
            }
        });

        let mut engine = Self {
            child,
            stdin,
            lines,
            stderr_tail,
            label: label.to_owned(),
        };

        engine.send("usi")?;
        let mut declared = Vec::new();
        let deadline = Instant::now() + USIOK_TIMEOUT;
        loop {
            let line = wait_line(&engine.lines, deadline, "usiok")?;
            if let Some(name) = option_name(&line) {
                declared.push(name);
            }
            if line.split_whitespace().next() == Some("usiok") {
                break;
            }
        }
        for (key, value) in options {
            if !declared.iter().any(|d| d == key) {
                eprintln!(
                    "{label}: option `{key}` is not among the engine's declared options \
                     ({declared:?}) — sent anyway"
                );
            }
            engine.send(&format!("setoption name {key} value {value}"))?;
        }
        engine.send("isready")?;
        wait_for(&engine.lines, Instant::now() + READYOK_TIMEOUT, "readyok")?;
        engine.send("usinewgame")?;
        Ok(engine)
    }

    /// One move: `position …`, `go nodes N`, and the `bestmove` line, with
    /// everything else (`info` chatter included) ignored.
    ///
    /// ⚠️ Anything the engine left unread is discarded before the `go` is
    /// sent. Without that, an engine emitting a second `bestmove` for one
    /// `go` would have it returned as the answer to a position never asked
    /// about, and every later move would be one behind — which reads as an
    /// illegal move from a healthy engine.
    pub fn bestmove(
        &mut self,
        position_args: &str,
        nodes: u64,
        timeout: Duration,
    ) -> Result<BestmoveAnswer, EngineError> {
        while self.lines.try_recv().is_ok() {}
        self.send(&format!("position {position_args}"))?;
        self.send(&format!("go nodes {nodes}"))?;
        let line = wait_for(&self.lines, Instant::now() + timeout, "bestmove")?;
        parse_bestmove(&line)
    }

    /// Best-effort `gameover`; a dead engine is already the recorded story.
    pub fn gameover(&mut self, result: &str) {
        let _ = writeln!(self.stdin, "gameover {result}");
        let _ = self.stdin.flush();
    }

    /// The last lines the engine wrote to stderr.
    ///
    /// ⚠️ Takes `&mut self` because it first gives a dying engine a moment to
    /// finish writing: `Died` is raised the instant *stdout* closes, and the
    /// stderr reader is a separate thread on a separate pipe, so reading the
    /// tail straight away most often returns nothing — in exactly the case
    /// the tail exists for, an engine that panicked and printed why.
    pub fn stderr_tail(&mut self) -> Vec<String> {
        for _ in 0..STDERR_SETTLE_POLLS {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) => std::thread::sleep(STDERR_SETTLE_STEP),
                Err(_) => break,
            }
        }
        // The child is gone, so its stderr pipe is closed; give the reader
        // thread the last scheduling slot it needs to drain what is buffered.
        std::thread::sleep(STDERR_SETTLE_STEP);
        self.stderr_tail
            .lock()
            .expect("no panics hold this lock")
            .iter()
            .cloned()
            .collect()
    }

    fn send(&mut self, line: &str) -> Result<(), EngineError> {
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|_| EngineError::Died {
                waiting_for: "a writable stdin",
            })
    }
}

impl Drop for UsiEngine {
    fn drop(&mut self) {
        let _ = writeln!(self.stdin, "quit");
        let _ = self.stdin.flush();
        for _ in 0..20 {
            match self.child.try_wait() {
                Ok(Some(_)) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(50)),
                Err(_) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The next line whose first token is `prefix`, discarding everything else.
fn wait_for(
    lines: &Receiver<String>,
    deadline: Instant,
    prefix: &'static str,
) -> Result<String, EngineError> {
    loop {
        let line = wait_line(lines, deadline, prefix)?;
        if line.split_whitespace().next() == Some(prefix) {
            return Ok(line);
        }
    }
}

/// The next line, whatever it is.
fn wait_line(
    lines: &Receiver<String>,
    deadline: Instant,
    waiting_for: &'static str,
) -> Result<String, EngineError> {
    let now = Instant::now();
    let budget = deadline.saturating_duration_since(now);
    if budget.is_zero() {
        return Err(EngineError::Timeout { waiting_for });
    }
    match lines.recv_timeout(budget) {
        Ok(line) => Ok(line),
        Err(RecvTimeoutError::Timeout) => Err(EngineError::Timeout { waiting_for }),
        Err(RecvTimeoutError::Disconnected) => Err(EngineError::Died { waiting_for }),
    }
}

/// The declared name in an `option name <N> type …` line.
///
/// ⚠️ The name runs to the `type` token, not to the first space: USI option
/// names may contain spaces (`Move Overhead`, `Skill Level`). Taking one
/// token instead would make the declared-options check warn on every launch
/// for such an engine, which retires the check as a signal.
fn option_name(line: &str) -> Option<String> {
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some("option") || tokens.next() != Some("name") {
        return None;
    }
    let name: Vec<&str> = tokens.take_while(|t| *t != "type").collect();
    (!name.is_empty()).then(|| name.join(" "))
}

fn parse_bestmove(line: &str) -> Result<BestmoveAnswer, EngineError> {
    let mut tokens = line.split_whitespace();
    if tokens.next() != Some("bestmove") {
        return Err(EngineError::Protocol(line.to_owned()));
    }
    match tokens.next() {
        Some("resign") => Ok(BestmoveAnswer::Resign),
        Some("win") => Ok(BestmoveAnswer::Win),
        Some(token) => Ok(BestmoveAnswer::Move(token.to_owned())),
        None => Err(EngineError::Protocol(line.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc::channel;

    fn deadline_in(ms: u64) -> Instant {
        Instant::now() + Duration::from_millis(ms)
    }

    #[test]
    fn a_bestmove_line_with_a_ponder_tail_yields_only_the_move() {
        assert_eq!(
            parse_bestmove("bestmove 7g7f ponder 3c3d").expect("readable"),
            BestmoveAnswer::Move("7g7f".to_owned())
        );
        assert_eq!(
            parse_bestmove("bestmove resign").expect("readable"),
            BestmoveAnswer::Resign
        );
        assert_eq!(
            parse_bestmove("bestmove win").expect("readable"),
            BestmoveAnswer::Win
        );
        assert!(matches!(
            parse_bestmove("bestmove"),
            Err(EngineError::Protocol(_))
        ));
        assert!(matches!(
            parse_bestmove("info string chatty"),
            Err(EngineError::Protocol(_))
        ));
    }

    #[test]
    fn waiting_skips_chatter_and_returns_the_matching_line() {
        let (tx, rx) = channel();
        tx.send("info depth 1 score cp 0".to_owned()).expect("open");
        tx.send("bestmove 7g7f".to_owned()).expect("open");
        let line = wait_for(&rx, deadline_in(1_000), "bestmove").expect("delivered");
        assert_eq!(line, "bestmove 7g7f");
    }

    /// `bestmoveX` must not satisfy a wait for `bestmove` — the match is on
    /// the whole first token, not a prefix of the line.
    #[test]
    fn a_token_prefix_is_not_a_match() {
        let (tx, rx) = channel();
        tx.send("bestmoveish nonsense".to_owned()).expect("open");
        tx.send("bestmove resign".to_owned()).expect("open");
        let line = wait_for(&rx, deadline_in(1_000), "bestmove").expect("delivered");
        assert_eq!(line, "bestmove resign");
    }

    #[test]
    fn a_silent_stream_times_out_and_a_closed_one_reports_death() {
        let (tx, rx) = channel::<String>();
        assert!(matches!(
            wait_for(&rx, deadline_in(30), "usiok"),
            Err(EngineError::Timeout {
                waiting_for: "usiok"
            })
        ));
        drop(tx);
        assert!(matches!(
            wait_for(&rx, deadline_in(30), "usiok"),
            Err(EngineError::Died {
                waiting_for: "usiok"
            })
        ));
    }

    #[test]
    fn declared_option_names_are_read_off_their_lines() {
        let name = |line: &str| option_name(line);
        assert_eq!(
            name("option name USI_Hash type spin default 256 min 1 max 65536").as_deref(),
            Some("USI_Hash")
        );
        assert_eq!(
            name("option name NodesLimit type spin default 0").as_deref(),
            Some("NodesLimit")
        );
        // A name with spaces in it runs to `type`, not to the first space.
        assert_eq!(
            name("option name Move Overhead type spin default 10").as_deref(),
            Some("Move Overhead")
        );
        assert_eq!(
            name("option name Skill Level type spin default 20 min 0 max 20").as_deref(),
            Some("Skill Level")
        );
        assert_eq!(name("id name rinsai 0.1.0"), None);
        assert_eq!(name("usiok"), None);
        assert_eq!(name("option name type spin"), None, "an empty name is none");
    }
}
