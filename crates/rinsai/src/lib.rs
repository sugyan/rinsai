//! The rinsai engine.
//!
//! The binary is a thin wrapper over [`usi::run`]. The library target exists so
//! the conformance tests can drive the protocol loop over in-memory pipes: a
//! dialogue test that spawns a process per case is slower, and — because it has
//! to guess when the engine has finished writing — far less precise.

pub mod output;
pub mod usi;
