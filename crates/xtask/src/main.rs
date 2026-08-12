//! `cargo run -p xtask -- <subcommand>` — dispatch and usage.

// stdout is this tool's user interface; no protocol runs on it.
#![allow(clippy::print_stdout)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(2).collect();
    match std::env::args().nth(1).as_deref() {
        Some("fetch-floodgate") => xtask::fetch::run(&args),
        Some(other) => {
            eprintln!("xtask: unknown subcommand `{other}`");
            usage()
        }
        None => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: cargo run --release -p xtask -- <subcommand>");
    eprintln!("subcommands: fetch-floodgate");
    ExitCode::FAILURE
}
