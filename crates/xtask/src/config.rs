//! The engine roster, read from `tools/opponents.toml`.
//!
//! A hand-rolled reader for a small TOML subset — the workspace's licence
//! allow-list rules out the usual parsing dependencies, and a roster needs
//! four keys. The accepted grammar, documented in the committed `.example`:
//! full-line `#` comments, blank lines, `[engines.<id>]` and
//! `[engines.<id>.options]` headers, and `key = "value"` with double-quoted
//! values (escapes: `\"` and `\\`). ⚠️ Anything else is a hard error, never
//! skipped: an ignored option key would silently change match conditions,
//! which is the one failure mode a roster file must not have.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// The section key — what `--candidate`/`--baseline` name.
    pub id: String,
    /// Display name for logs.
    pub name: String,
    pub path: PathBuf,
    /// Extra command-line arguments, whitespace-split from the `args` value.
    pub args: Vec<String>,
    /// `setoption` pairs, in file order — the order they are sent.
    pub options: Vec<(String, String)>,
}

pub fn load(path: &Path) -> Result<Vec<EngineConfig>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        format!(
            "{}: {e} — copy tools/opponents.toml.example there and edit the paths",
            path.display()
        )
    })?;
    let engines = parse(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    validate_paths(&engines)?;
    Ok(engines)
}

pub fn parse(text: &str) -> Result<Vec<EngineConfig>, String> {
    struct Draft {
        id: String,
        name: Option<String>,
        path: Option<PathBuf>,
        args: Vec<String>,
        options: Vec<(String, String)>,
    }
    enum Where {
        Engine,
        Options,
    }
    let mut drafts: Vec<Draft> = Vec::new();
    let mut section: Option<Where> = None;

    for (index, raw) in text.lines().enumerate() {
        let n = index + 1;
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            let mut parts = header.split('.');
            let (engines, id, tail) = (parts.next(), parts.next(), parts.next());
            if engines != Some("engines") || id.is_none_or(str::is_empty) || parts.next().is_some()
            {
                return Err(format!("line {n}: unrecognised section `[{header}]`"));
            }
            let id = id.expect("checked above");
            match tail {
                None => {
                    if drafts.iter().any(|d| d.id == id) {
                        return Err(format!("line {n}: engine `{id}` is declared twice"));
                    }
                    drafts.push(Draft {
                        id: id.to_owned(),
                        name: None,
                        path: None,
                        args: Vec::new(),
                        options: Vec::new(),
                    });
                    section = Some(Where::Engine);
                }
                Some("options") => {
                    if drafts.last().is_none_or(|d| d.id != id) {
                        return Err(format!(
                            "line {n}: `[engines.{id}.options]` before `[engines.{id}]`"
                        ));
                    }
                    section = Some(Where::Options);
                }
                Some(other) => {
                    return Err(format!("line {n}: unrecognised subsection `{other}`"));
                }
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(format!(
                "line {n}: expected `key = \"value\"`, got `{line}`"
            ));
        };
        let key = key.trim();
        let value = unquote(value.trim()).ok_or_else(|| {
            format!("line {n}: the value for `{key}` must be a double-quoted string")
        })?;
        let draft = drafts.last_mut();
        match section {
            None => return Err(format!("line {n}: `{key}` outside any [engines.*] section")),
            Some(Where::Engine) => {
                let draft = draft.expect("a section implies a draft");
                match key {
                    "name" => draft.name = Some(value),
                    "path" => draft.path = Some(PathBuf::from(value)),
                    "args" => draft.args = value.split_whitespace().map(String::from).collect(),
                    other => {
                        return Err(format!(
                            "line {n}: unknown engine key `{other}` (name, path, args)"
                        ));
                    }
                }
            }
            Some(Where::Options) => {
                let draft = draft.expect("a section implies a draft");
                if draft.options.iter().any(|(k, _)| k == key) {
                    return Err(format!("line {n}: option `{key}` is set twice"));
                }
                draft.options.push((key.to_owned(), value));
            }
        }
    }

    drafts
        .into_iter()
        .map(|draft| {
            let id = draft.id;
            Ok(EngineConfig {
                name: draft.name.ok_or(format!("engine `{id}` has no name"))?,
                path: draft.path.ok_or(format!("engine `{id}` has no path"))?,
                args: draft.args,
                options: draft.options,
                id,
            })
        })
        .collect()
}

/// Every path must be absolute — `../benchmarks` does not resolve from a
/// `.claude/worktrees/*` checkout, which sits two levels deeper than the
/// main one — and must exist.
fn validate_paths(engines: &[EngineConfig]) -> Result<(), String> {
    for engine in engines {
        if !engine.path.is_absolute() {
            return Err(format!(
                "engine `{}`: path `{}` is relative — use an absolute path; a git worktree \
                 under .claude/worktrees/ is two levels deeper than the main checkout, so \
                 relative paths resolve differently per checkout",
                engine.id,
                engine.path.display()
            ));
        }
        if !engine.path.exists() {
            return Err(format!(
                "engine `{}`: `{}` does not exist",
                engine.id,
                engine.path.display()
            ));
        }
    }
    Ok(())
}

/// `"…"` with `\"` and `\\` escapes; anything else is not a value.
fn unquote(text: &str) -> Option<String> {
    let inner = text.strip_prefix('"')?.strip_suffix('"')?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '"' {
            return None;
        }
        if c == '\\' {
            match chars.next() {
                Some('"') => out.push('"'),
                Some('\\') => out.push('\\'),
                _ => return None,
            }
        } else {
            out.push(c);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROSTER: &str = r#"
# a comment
[engines.rinsai-dev]
name = "rinsai (current build)"
path = "/tmp/rinsai"

[engines.rinsai-dev.options]
USI_Hash = "256"
Threads = "1"

[engines.yaneuraou]
name = "YaneuraOu \"Material\""
path = "/tmp/yane"
args = "--flag value"
"#;

    #[test]
    fn a_roster_parses_with_option_order_preserved() {
        let engines = parse(ROSTER).expect("valid roster");
        assert_eq!(engines.len(), 2);
        assert_eq!(engines[0].id, "rinsai-dev");
        assert_eq!(engines[0].name, "rinsai (current build)");
        assert_eq!(
            engines[0].options,
            vec![
                ("USI_Hash".to_owned(), "256".to_owned()),
                ("Threads".to_owned(), "1".to_owned()),
            ]
        );
        assert_eq!(engines[1].name, "YaneuraOu \"Material\"");
        assert_eq!(engines[1].args, vec!["--flag", "value"]);
        assert!(engines[1].options.is_empty());
    }

    /// The rule the module doc states: nothing is skipped, everything
    /// unrecognised stops the load.
    #[test]
    fn every_unrecognised_shape_is_a_hard_error() {
        for (text, fragment) in [
            ("[engine.x]\n", "unrecognised section"),
            ("[engines.x.settings]\n", "unrecognised subsection"),
            ("[engines.x]\nnodes = \"1\"\n", "unknown engine key"),
            ("[engines.x]\nname = bare\n", "double-quoted"),
            ("name = \"orphan\"\n", "outside any"),
            ("[engines.x]\n[engines.x]\n", "declared twice"),
            (
                "[engines.x]\n[engines.x.options]\nA = \"1\"\nA = \"2\"\n",
                "set twice",
            ),
            ("[engines.y]\n[engines.x.options]\n", "before `[engines.x]`"),
            ("[engines.x]\nname = \"n\"\n", "has no path"),
            ("[engines.x]\npath = \"/tmp/x\"\n", "has no name"),
            ("just words\n", "expected `key = \"value\"`"),
        ] {
            let err = parse(text).expect_err(text);
            assert!(err.contains(fragment), "{text:?} gave {err:?}");
        }
    }

    #[test]
    fn a_relative_engine_path_names_the_worktree_trap() {
        let engines = vec![EngineConfig {
            id: "x".to_owned(),
            name: "x".to_owned(),
            path: PathBuf::from("../benchmarks/x"),
            args: Vec::new(),
            options: Vec::new(),
        }];
        let err = validate_paths(&engines).expect_err("relative path");
        assert!(err.contains("worktrees"), "{err}");
    }

    #[test]
    fn escapes_cover_quotes_and_backslashes_and_nothing_else() {
        assert_eq!(unquote(r#""a\"b\\c""#).as_deref(), Some(r#"a"b\c"#));
        assert_eq!(unquote(r#""\n""#), None, "no other escapes");
        assert_eq!(unquote("bare"), None);
        assert_eq!(unquote(r#""unclosed"#), None);
    }
}
