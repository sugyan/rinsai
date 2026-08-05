//! Search and evaluation for [`rinsai`](https://github.com/sugyan/rinsai).
//!
//! This is the only crate that depends on [`shunsai`], which owns legal move
//! generation, the position representation, do/undo and the Zobrist key. What
//! lives here is everything shunsai deliberately does not carry: the game and
//! its history, evaluation, search, and time management.
//!
//! # Conventions
//!
//! * A bare `Position` always means [`shunsai::Position`]. `shogi_core::Position`
//!   is never imported — we replay moves ourselves, so we never need it.
//! * Evaluation is **negamax, from the side to move**: a positive [`Score`] is
//!   good for whoever is to move at that node, and a parent takes `-child`.
//! * Scores are **centipawns**, pawn = 100.

// Re-exported so a consumer reaches the shared vocabulary through one path, and
// so a future major bump of either is a single-line change in one manifest.
pub use shogi_core;
pub use shunsai;

mod score;

pub use score::{Depth, MAX_PLY, Score};
