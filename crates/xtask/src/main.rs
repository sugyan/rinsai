//! `cargo run -p xtask -- <subcommand>` — dispatch and usage.

// stdout is this tool's user interface; no protocol runs on it.
#![allow(clippy::print_stdout)]

use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some(other) => {
            eprintln!("xtask: unknown subcommand `{other}`");
            usage()
        }
        None => usage(),
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: cargo run --release -p xtask -- <subcommand>");
    ExitCode::FAILURE
}
