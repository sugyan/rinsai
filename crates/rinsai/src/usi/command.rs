//! Parsing what a GUI sends.
//!
//! This is our own code on purpose. The `usi` crate is a GUI-side engine
//! driver: its `GuiCommand` can be *written* but not read, so it cannot parse
//! anything an engine receives. Owning the loop is also what lets it carry
//! its own conformance tests.

use std::time::Duration;

use rinsai_search::Limits;

/// A command from the GUI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum GuiCommand {
    Usi,
    IsReady,
    SetOption {
        name: String,
        value: Option<String>,
    },
    UsiNewGame,
    /// Everything after `position`, unparsed — `Game` owns that grammar,
    /// because `bench` needs the same rules.
    Position(String),
    Go(Limits),
    /// `go mate …`. A different command, not a `go` with a flag: it answers
    /// `checkmate`, never `bestmove`.
    GoMate,
    Stop,
    PonderHit,
    GameOver,
    Quit,
    /// Kept rather than discarded so the loop can log what it ignored.
    Unknown(String),
}

/// Parses one line. `None` for a blank one.
///
/// Tokens are split on any run of whitespace, so the irregular spacing real
/// GUIs emit is not a special case. The line still carries its terminator: the
/// loop in [`usi::run`](crate::usi::run) reads with `read_until(b'\n')` rather
/// than `BufRead::lines`, so that a non-UTF-8 byte can be absorbed instead of
/// ending the session. The `trim` below is therefore what removes the `\n` and
/// a Windows GUI's `\r`, not a leftover safeguard.
pub(crate) fn parse_line(line: &str) -> Option<GuiCommand> {
    let line = line.trim();
    let (word, rest) = match line.split_once(char::is_whitespace) {
        Some((word, rest)) => (word, rest.trim()),
        None if line.is_empty() => return None,
        None => (line, ""),
    };

    Some(match word {
        // Trailing tokens are ignored on the bare commands: the spec does not
        // define any, and refusing to answer `usi` over a stray word would be a
        // worse failure than accepting it.
        "usi" => GuiCommand::Usi,
        "isready" => GuiCommand::IsReady,
        "usinewgame" => GuiCommand::UsiNewGame,
        "stop" => GuiCommand::Stop,
        "ponderhit" => GuiCommand::PonderHit,
        "gameover" => GuiCommand::GameOver,
        "quit" => GuiCommand::Quit,
        "setoption" => parse_setoption(rest),
        "position" => GuiCommand::Position(rest.to_owned()),
        "go" if rest.split_whitespace().next() == Some("mate") => GuiCommand::GoMate,
        "go" => GuiCommand::Go(parse_go(rest)),
        _ => GuiCommand::Unknown(line.to_owned()),
    })
}

/// `setoption name <id> [value <v>]`.
///
/// The name is everything between `name` and a ` value ` token; the value is
/// everything after it, **with its spaces intact** — a `string` option may
/// legitimately contain them, and a path certainly may.
fn parse_setoption(rest: &str) -> GuiCommand {
    // Strip the `name` keyword as a whole word — `strip_prefix("name")` would
    // turn an option called `nameless` into `less`.
    let after_name = match rest.split_once(char::is_whitespace) {
        Some(("name", tail)) => tail.trim_start(),
        _ => rest,
    };
    match split_once_token(after_name, "value") {
        Some((name, value)) => GuiCommand::SetOption {
            name: name.trim().to_owned(),
            value: Some(value.trim().to_owned()),
        },
        None => GuiCommand::SetOption {
            name: after_name.trim().to_owned(),
            value: None,
        },
    }
}

/// Splits at the first occurrence of `token` as a whole word.
fn split_once_token<'a>(haystack: &'a str, token: &str) -> Option<(&'a str, &'a str)> {
    let mut offset = 0;
    for word in haystack.split_whitespace() {
        let start = offset + haystack[offset..].find(word)?;
        if word == token {
            return Some((&haystack[..start], &haystack[start + word.len()..]));
        }
        offset = start + word.len();
    }
    None
}

/// Every `go` field is parsed and acted on. A field
/// whose value is missing or unparseable is simply left unset — a `go` is never
/// refused, because refusing one means no `bestmove` and a game lost on time.
/// (The asymmetry is deliberate: `position` *is* refused, because there the
/// safe answer is to keep the last good board and say so.)
fn parse_go(rest: &str) -> Limits {
    fn ms<'a>(tokens: &mut impl Iterator<Item = &'a str>) -> Option<Duration> {
        tokens
            .next()
            .and_then(|v| v.parse().ok())
            .map(Duration::from_millis)
    }
    fn number<'a, T: std::str::FromStr>(tokens: &mut impl Iterator<Item = &'a str>) -> Option<T> {
        tokens.next().and_then(|v| v.parse().ok())
    }

    let mut limits = Limits::default();
    let mut tokens = rest.split_whitespace();

    while let Some(token) = tokens.next() {
        match token {
            "btime" => limits.btime = ms(&mut tokens),
            "wtime" => limits.wtime = ms(&mut tokens),
            "binc" => limits.binc = ms(&mut tokens),
            "winc" => limits.winc = ms(&mut tokens),
            "byoyomi" => limits.byoyomi = ms(&mut tokens),
            "movetime" => limits.movetime = ms(&mut tokens),
            "depth" => limits.depth = number(&mut tokens),
            "nodes" => limits.nodes = number(&mut tokens),
            "infinite" => limits.infinite = true,
            "ponder" => limits.ponder = true,
            _ => {}
        }
    }
    limits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn go(line: &str) -> Limits {
        match parse_line(line) {
            Some(GuiCommand::Go(limits)) => limits,
            other => panic!("expected a `go`, got {other:?}"),
        }
    }

    #[test]
    fn blank_lines_are_not_commands() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("   \t "), None);
    }

    #[test]
    fn the_bare_commands_parse() {
        assert_eq!(parse_line("usi"), Some(GuiCommand::Usi));
        assert_eq!(parse_line("isready"), Some(GuiCommand::IsReady));
        assert_eq!(parse_line("usinewgame"), Some(GuiCommand::UsiNewGame));
        assert_eq!(parse_line("stop"), Some(GuiCommand::Stop));
        assert_eq!(parse_line("ponderhit"), Some(GuiCommand::PonderHit));
        assert_eq!(parse_line("quit"), Some(GuiCommand::Quit));
        assert_eq!(parse_line("gameover win"), Some(GuiCommand::GameOver));
    }

    /// A GUI that sends `usi ` or `  quit` must still be understood.
    #[test]
    fn surrounding_whitespace_and_stray_tokens_are_tolerated() {
        assert_eq!(parse_line("  usi  "), Some(GuiCommand::Usi));
        assert_eq!(parse_line("quit now"), Some(GuiCommand::Quit));
        assert_eq!(
            parse_line("  position   startpos   moves  7g7f "),
            Some(GuiCommand::Position("startpos   moves  7g7f".to_owned()))
        );
    }

    #[test]
    fn unknown_commands_keep_their_text_for_the_log() {
        assert_eq!(
            parse_line("frobnicate 1 2 3"),
            Some(GuiCommand::Unknown("frobnicate 1 2 3".to_owned()))
        );
    }

    /// The value keeps its spaces: a path value contains them.
    #[test]
    fn setoption_keeps_the_value_verbatim() {
        assert_eq!(
            parse_line("setoption name USI_Hash value 512"),
            Some(GuiCommand::SetOption {
                name: "USI_Hash".to_owned(),
                value: Some("512".to_owned())
            })
        );
        assert_eq!(
            parse_line("setoption name Eval Dir value /some path/with spaces"),
            Some(GuiCommand::SetOption {
                name: "Eval Dir".to_owned(),
                value: Some("/some path/with spaces".to_owned())
            })
        );
        assert_eq!(
            parse_line("setoption name ClearHash"),
            Some(GuiCommand::SetOption {
                name: "ClearHash".to_owned(),
                value: None
            })
        );
    }

    /// `value` must only split on the whole word, or an option named
    /// `NetworkDelay` with a value of `values` would come apart in odd ways.
    #[test]
    fn setoption_splits_on_the_value_keyword_only_as_a_word() {
        assert_eq!(
            parse_line("setoption name Revalue value 3"),
            Some(GuiCommand::SetOption {
                name: "Revalue".to_owned(),
                value: Some("3".to_owned())
            })
        );
    }

    /// `split_once_token` mixes `split_whitespace` with `haystack[offset..]
    /// .find(word)`, which *looks* as though it could match a substring before
    /// the real word boundary, and slices by byte offset, which looks as though
    /// it could split a multibyte character. It does neither — `offset` is
    /// always just past the previous word, so everything between it and the
    /// next word is whitespace — but "looks wrong and is fine" is worth pinning
    /// rather than re-deriving. Compared against an obviously-correct reference
    /// over every string of length 0..=4 in a whitespace-heavy alphabet.
    #[test]
    fn split_once_token_agrees_with_a_reference_implementation() {
        fn reference<'a>(haystack: &'a str, token: &str) -> Option<(&'a str, &'a str)> {
            let mut cursor = 0;
            for word in haystack.split_whitespace() {
                let start = cursor + haystack[cursor..].find(word).expect("the word is present");
                if word == token {
                    return Some((&haystack[..start], &haystack[start + word.len()..]));
                }
                cursor = start + word.len();
            }
            None
        }

        let alphabet = ['v', 'a', 'l', 'u', 'e', ' ', '\t', 'あ'];
        let mut checked = 0usize;
        let mut stack = vec![String::new()];
        while let Some(s) = stack.pop() {
            for token in ["value", "a", "val", "あ"] {
                assert_eq!(
                    split_once_token(&s, token),
                    reference(&s, token),
                    "disagreed on {s:?} / {token:?}"
                );
                checked += 1;
            }
            if s.chars().count() < 4 {
                for c in alphabet {
                    let mut next = s.clone();
                    next.push(c);
                    stack.push(next);
                }
            }
        }
        assert!(checked > 10_000, "only {checked} cases");
    }

    #[test]
    fn go_mate_is_its_own_command() {
        assert_eq!(parse_line("go mate 1000"), Some(GuiCommand::GoMate));
        assert_eq!(parse_line("go mate infinite"), Some(GuiCommand::GoMate));
    }

    #[test]
    fn every_go_field_lands_in_limits() {
        assert_eq!(
            go("go btime 300000 wtime 299000 byoyomi 5000"),
            Limits {
                btime: Some(Duration::from_millis(300_000)),
                wtime: Some(Duration::from_millis(299_000)),
                byoyomi: Some(Duration::from_millis(5_000)),
                ..Limits::default()
            }
        );
        assert_eq!(
            go("go btime 60000 wtime 60000 binc 5000 winc 5000"),
            Limits {
                btime: Some(Duration::from_millis(60_000)),
                wtime: Some(Duration::from_millis(60_000)),
                binc: Some(Duration::from_millis(5_000)),
                winc: Some(Duration::from_millis(5_000)),
                ..Limits::default()
            }
        );
        assert_eq!(
            go("go movetime 2000"),
            Limits {
                movetime: Some(Duration::from_millis(2_000)),
                ..Limits::default()
            }
        );
        assert_eq!(
            go("go depth 8"),
            Limits {
                depth: Some(8),
                ..Limits::default()
            }
        );
        assert_eq!(
            go("go nodes 100000"),
            Limits {
                nodes: Some(100_000),
                ..Limits::default()
            }
        );
        assert_eq!(
            go("go infinite"),
            Limits {
                infinite: true,
                ..Limits::default()
            }
        );
        assert_eq!(go("go"), Limits::default());
    }

    /// A `go` is never refused. An unknown token is skipped and the fields
    /// around it still arrive; a keyword with no value simply stays unset.
    #[test]
    fn a_malformed_go_still_yields_the_fields_it_can() {
        assert_eq!(
            go("go btime 1000 flurb 7 wtime 2000"),
            Limits {
                btime: Some(Duration::from_millis(1_000)),
                wtime: Some(Duration::from_millis(2_000)),
                ..Limits::default()
            }
        );
        assert_eq!(go("go btime"), Limits::default());
        assert_eq!(
            go("go btime notanumber wtime 5"),
            Limits {
                wtime: Some(Duration::from_millis(5)),
                ..Limits::default()
            }
        );
    }
}
