//! rinsai's development tooling, behind `cargo run -p xtask -- <subcommand>`.
//!
//! Not part of the engine and never published; the library target exists so
//! the tests can reach the modules.

// stdout is this tool's user interface; no protocol runs on it, so the
// allow is crate-wide rather than per-module.
#![allow(clippy::print_stdout)]

pub mod config;
pub mod csa;
pub mod fetch;
pub mod jsonl;
pub mod openings;
pub mod referee;
pub mod rng;
pub mod runner;
pub mod schedule;
pub mod sprt;
pub mod usi;
