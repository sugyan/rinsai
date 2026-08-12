//! A shogi game, its history, and the single gate every move passes through.
//!
//! [`Game`] holds a position with how it was reached, refuses illegal moves,
//! and adjudicates the endings decided by rule rather than by a player: 詰み
//! (in shogi a player with no legal move loses, stalemate included), 千日手
//! (fourfold repetition) and 連続王手の千日手 (perpetual check — the checking
//! side loses). Legality comes from [`shogi_legality_lite`], deliberately not
//! from any engine's own movegen: a game refereed here is a differential test
//! of its players, not an echo of one of them.
//!
//! Consumers: the match harness in this repository (`crates/xtask`), and the
//! `tuishogi` board app once this crate is published.
//!
//! Provenance: moved from <https://github.com/sugyan/tuishogi> at
//! `73d0d9c592d4c16fa6adb97b7c924bdf9bd52a05` (`src/game.rs` and
//! `src/game/{moves,repetition,types}.rs`), MIT, same author; relicensed to
//! this workspace's `MIT OR Apache-2.0` by the author.
//!
//! ⚠️ A [`Game`] rooted at a mid-game position cannot see a repetition whose
//! earlier occurrences predate its root: the rule reads the history this
//! `Game` recorded, and nothing else.

mod game;
mod moves;
mod repetition;
mod types;

pub use game::Game;
pub use moves::{in_check, move_from_usi};
pub use repetition::{PositionKey, position_key};
pub use types::{MoveError, Outcome, Ply, PromotionChoice, illegal, side};
