//! Search and evaluation for [`rinsai`](https://github.com/sugyan/rinsai).
//!
//! This is the only crate that depends on [`shunsai`], which owns legal move
//! generation, the position representation, do/undo and the Zobrist key. What
//! lives here is everything shunsai deliberately does not carry: the game and
//! its history, evaluation, search, and time management.
//!
//! # Conventions
//!
//! * A bare `Position` always means [`shunsai::Position`] — the board a search
//!   walks, and the only one of the two with unmake and an incremental Zobrist
//!   key. `shogi_core::Position` is a *record* (root, current position, moves
//!   played); [`Game`] delegates that half to it under the alias `Record`, and
//!   it is never imported unqualified. Its `from_usi` is not used at all — see
//!   [`Game::from_usi_position`].
//! * Evaluation is **negamax, from the side to move**: a positive [`Score`] is
//!   good for whoever is to move at that node, and a parent takes `-child`.
//! * Scores are **centipawns**, pawn = 100.

// Re-exported so the shared vocabulary is reachable through one path, and so
// the version is stated once in `[workspace.dependencies]`.
pub use shogi_core;
pub use shunsai;

mod eval;
mod game;
mod info;
mod moves;
mod negamax;
mod score;
mod search;
mod tt;

pub use game::{Game, HistoryEntry, IllegalMove, PositionError};
pub use info::nps;
pub use moves::{MAX_LEGAL_MOVES, is_legal};
pub use negamax::NegamaxSearcher;
pub use score::{Depth, MAX_PLY, Score};
pub use search::{
    BestMove, InfoSink, Limits, SearchDriver, SearchJob, SearchSignals, Searcher, SilentSink,
};
pub use tt::DEFAULT_HASH_MB;
