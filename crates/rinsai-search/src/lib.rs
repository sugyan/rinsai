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

// Re-exported so a consumer *can* reach the shared vocabulary through one path
// — `crates/rinsai` in fact takes `shogi_core` directly, which is fine, since
// the rule is that rinsai-search is the only crate depending on **shunsai**.
// What this does buy: the version is stated once, in `[workspace.dependencies]`,
// so a future major bump is a single-line change.
pub use shogi_core;
pub use shunsai;

mod eval;
mod game;
mod info;
mod moves;
mod negamax;
mod score;
mod search;

pub use game::{Game, HistoryEntry, IllegalMove, PositionError};
pub use moves::{MAX_LEGAL_MOVES, is_legal};
pub use negamax::NegamaxSearcher;
pub use score::{Depth, MAX_PLY, Score};
pub use search::{
    BestMove, InfoSink, Limits, SearchDriver, SearchJob, SearchSignals, Searcher, SilentSink,
};
