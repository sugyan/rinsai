//! rinsai's development tooling, behind `cargo run -p xtask -- <subcommand>`.
//!
//! Not part of the engine and never published; the library target exists so
//! the tests can reach the modules.

// stdout is this tool's user interface; no protocol runs on it. The engine
// keeps the allow to one protocol-writer module — here every subcommand
// module reports to the terminal.
#![allow(clippy::print_stdout)]

pub mod csa;
pub mod fetch;
pub mod openings;
pub mod rng;
