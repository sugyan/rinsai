//! Vocabulary for the rules layer.

use std::fmt;

use shogi_core::{Color, IllegalMoveKind, Move};

/// One played move, with everything derived from it that is expensive or
/// impossible to recompute later.
#[derive(Debug, Clone)]
pub struct Ply {
    pub mv: Move,
    /// Whether this move gave check. Cached because the perpetual-check rule
    /// needs it for every ply inside a repetition window.
    pub gave_check: bool,
    /// Official kifu text, e.g. `▲７六歩`. Computed at push time because
    /// [`shogi_official_kifu::display_single_move_kansuji`] needs the position
    /// *before* the move.
    pub kifu: String,
}

/// How a game ended.
///
/// ⚠️ 詰み, 千日手 and 連続王手の千日手 are the endings a board can reach.
/// [`play`](crate::Game::play) produces those and no others; every other variant
/// is decided off the board and reaches a game through one of
/// [`Game`](crate::Game)'s adjudication methods. A caller that sets a *derived*
/// ending is asserting something the referee could have checked and did not,
/// which is why no method takes an `Outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// 詰み. `status_partial` folds stalemate into this, which is right: in
    /// shogi a player with no legal move loses either way.
    Checkmate { winner: Color },
    /// 千日手 — fourfold repetition with neither side checking throughout.
    Repetition,
    /// 連続王手の千日手 — `loser` checked on every one of its moves in the cycle.
    PerpetualCheck { loser: Color },
    /// 投了.
    Resignation { loser: Color },
    /// 時間切れ.
    FlagFall { loser: Color },
    /// 反則負け, carrying the rule that was broken so the result can say which.
    IllegalMove { loser: Color, kind: IllegalMoveKind },
    /// 入玉宣言.
    ///
    /// ⚠️ A claim, not a verdict: this crate has no 27-point rule, so nothing
    /// here checked it. [`declare`](crate::Game::declare) carries what that
    /// costs a caller.
    Declaration { winner: Color },
    /// The game reached the ply cap its players agreed on. A draw, and the one
    /// ending that names no side.
    MaxMoves,
    /// `loser` stopped playing — crashed, wedged, or answered something no move
    /// could be read out of.
    Abandoned { loser: Color },
}

impl fmt::Display for Outcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::Checkmate { winner } => write!(f, "{}の勝ち（詰み）", side(winner)),
            Self::Repetition => f.write_str("千日手"),
            Self::PerpetualCheck { loser } => {
                write!(f, "{}の負け（連続王手の千日手）", side(loser))
            }
            Self::Resignation { loser } => write!(f, "{}の投了", side(loser)),
            Self::FlagFall { loser } => write!(f, "{}の時間切れ", side(loser)),
            // The kind's own sentence rather than a noun beside it: a second
            // table over `IllegalMoveKind` is a second thing to keep in step
            // with the first, for one word's worth of grammar.
            Self::IllegalMove { loser, kind } => {
                write!(f, "{}の反則負け（{}）", side(loser), illegal(kind))
            }
            Self::Declaration { winner } => write!(f, "{}の勝ち（入玉宣言）", side(winner)),
            Self::MaxMoves => f.write_str("引き分け（最大手数）"),
            Self::Abandoned { loser } => write!(f, "{}の負け（対局放棄）", side(loser)),
        }
    }
}

/// Why a move was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MoveError {
    /// The game already has an outcome.
    GameOver,
    Illegal(IllegalMoveKind),
}

impl fmt::Display for MoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::GameOver => f.write_str("対局は終了しています"),
            Self::Illegal(kind) => f.write_str(illegal(kind)),
        }
    }
}

impl std::error::Error for MoveError {}

/// Which promotion choices exist for one `(from, to)` pair.
///
/// The rules library decides this, rather than hand-written rank predicates:
/// [`Forced`](Self::Forced) is exactly the case where not promoting leaves the
/// piece with nowhere to go (歩 and 香 on the last rank, 桂 on the last two).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionChoice {
    /// Only the non-promoting move is legal.
    None,
    /// Only the promoting move is legal — do not prompt.
    Forced,
    /// Both are legal; the UI must ask.
    Optional,
}

/// The Japanese name of a side, e.g. for building result sentences.
#[must_use]
pub const fn side(color: Color) -> &'static str {
    match color {
        Color::Black => "先手",
        Color::White => "後手",
    }
}

/// Why a USI move token was refused by [`Game::play_usi`](crate::Game::play_usi).
#[derive(Debug, Clone)]
pub enum UsiMoveError {
    /// The token is not a move.
    Syntax(shogi_usi_parser::Error),
    /// The token is a move, and the rules refused it.
    Refused(MoveError),
}

impl fmt::Display for UsiMoveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax(e) => write!(f, "not a USI move: {e}"),
            Self::Refused(e) => write!(f, "refused: {e}"),
        }
    }
}

impl std::error::Error for UsiMoveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Syntax(e) => Some(e),
            Self::Refused(e) => Some(e),
        }
    }
}

/// Why a USI `position` argument was refused by
/// [`Game::from_usi_position`](crate::Game::from_usi_position).
#[derive(Debug, Clone)]
pub enum UsiPositionError {
    /// The argument was empty.
    Empty,
    /// The root — `startpos` or `sfen …` — did not parse.
    Root(shogi_usi_parser::Error),
    /// A token in the `moves` list was refused; `index` counts from the first
    /// move token.
    Move {
        index: usize,
        token: String,
        source: UsiMoveError,
    },
}

impl fmt::Display for UsiPositionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("empty position argument"),
            Self::Root(e) => write!(f, "bad root: {e}"),
            Self::Move {
                index,
                token,
                source,
            } => write!(f, "move {index} `{token}`: {source}"),
        }
    }
}

impl std::error::Error for UsiPositionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Empty | Self::Root(_) => None,
            Self::Move { source, .. } => Some(source),
        }
    }
}

/// The user-facing reason a move was illegal.
///
/// `IncorrectMove` is a catch-all that lumps together far too much to show
/// anyone; a caller wanting finer messages classifies the common cases before
/// reaching the rules library and falls through to here as a last resort.
#[must_use]
pub const fn illegal(kind: IllegalMoveKind) -> &'static str {
    match kind {
        IllegalMoveKind::TwoPawns => "二歩です",
        IllegalMoveKind::IgnoredCheck => "王手を放置しています",
        IllegalMoveKind::DropPawnMate => "打ち歩詰めです",
        IllegalMoveKind::DropStuck => "そこに打つと行き所がありません",
        IllegalMoveKind::NormalStuck => "成らなければ行き所がありません",
        IllegalMoveKind::GameFinished => "対局は終了しています",
        IllegalMoveKind::IncorrectMove => "その手は指せません",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_perpetual_check_loss_names_the_checking_side_as_the_loser() {
        let outcome = Outcome::PerpetualCheck {
            loser: Color::Black,
        };
        assert_eq!(outcome.to_string(), "先手の負け（連続王手の千日手）");
    }

    /// Every variant, written out, because nothing enumerates an enum for us.
    /// What the distinctness check would catch is a `Display` arm copied from
    /// the one above it and left naming the wrong ending — nine arms sharing
    /// one shape is where that goes unnoticed.
    #[test]
    fn every_outcome_reads_as_a_sentence_of_its_own() {
        let all = [
            Outcome::Checkmate {
                winner: Color::Black,
            },
            Outcome::Repetition,
            Outcome::PerpetualCheck {
                loser: Color::Black,
            },
            Outcome::Resignation {
                loser: Color::Black,
            },
            Outcome::FlagFall {
                loser: Color::Black,
            },
            Outcome::IllegalMove {
                loser: Color::Black,
                kind: IllegalMoveKind::TwoPawns,
            },
            Outcome::Declaration {
                winner: Color::Black,
            },
            Outcome::MaxMoves,
            Outcome::Abandoned {
                loser: Color::Black,
            },
        ];
        let mut seen: Vec<String> = Vec::new();
        for outcome in all {
            let sentence = outcome.to_string();
            assert!(!sentence.is_empty(), "{outcome:?} says nothing");
            assert!(
                !seen.contains(&sentence),
                "{outcome:?} repeats {sentence:?}"
            );
            seen.push(sentence);
        }
        assert_eq!(seen[0], "先手の勝ち（詰み）");
        assert_eq!(seen[1], "千日手");
        assert_eq!(seen[7], "引き分け（最大手数）");
    }

    /// The rule broken has to survive into the sentence: an adjudication that
    /// says only "反則負け" leaves the loser unable to tell 二歩 from 王手放置.
    #[test]
    fn an_illegal_move_names_both_the_loser_and_the_rule() {
        let outcome = Outcome::IllegalMove {
            loser: Color::White,
            kind: IllegalMoveKind::DropPawnMate,
        };
        let sentence = outcome.to_string();
        assert!(sentence.contains("後手"), "{sentence}");
        assert!(
            sentence.contains(illegal(IllegalMoveKind::DropPawnMate)),
            "{sentence}"
        );
    }

    /// Exhaustiveness is the `match` in `illegal`'s job. What this adds is that
    /// no two kinds answer with the same sentence, so a player told why a move
    /// was refused learns which refusal it was.
    #[test]
    fn every_illegal_move_kind_has_a_message_of_its_own() {
        // Enumerated from upstream rather than written out here. An eighth
        // variant stops `illegal` above compiling before this ever runs; the
        // count at the end is the backstop if that ever stops being true.
        let mut seen: Vec<&str> = Vec::new();
        for kind in (1..=u8::MAX).filter_map(IllegalMoveKind::from_u8) {
            let message = illegal(kind);
            assert!(!message.is_empty(), "{kind:?} says nothing");
            assert!(!seen.contains(&message), "{kind:?} repeats {message:?}");
            seen.push(message);
        }
        assert_eq!(seen.len(), 7);
    }

    /// One sentence with two producers in this file: `MoveError::GameOver`'s
    /// Display, and `illegal`'s arm for the kind a referee would use for the
    /// same fact. Nothing else ties them together.
    #[test]
    fn a_finished_game_reads_the_same_whether_the_game_or_the_kind_says_so() {
        assert_eq!(
            MoveError::GameOver.to_string(),
            illegal(IllegalMoveKind::GameFinished)
        );
    }
}
