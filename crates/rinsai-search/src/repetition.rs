//! 千日手 and 連続王手の千日手 — when a repeated position ends the game, and how.
//!
//! The rule is about a *game* rather than a position, so this module reads
//! [`HistoryEntry`]s and never touches a board. Nothing here calls shunsai,
//! which is what lets the rule be tested without one.

use crate::game::HistoryEntry;
use crate::score::Score;

/// How many times one position has to occur for the game to be 千日手.
const OCCURRENCES: usize = 4;

/// How a game that has just repeated for the [`OCCURRENCES`]th time ends, from
/// the point of view of the side to move.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Repetition {
    /// 千日手. Neither side checked throughout, so the game is drawn.
    Draw,
    /// 連続王手の千日手. The side to move gave the checks, so it loses.
    PerpetualCheckLoss,
    /// 連続王手の千日手. The opponent gave the checks, so it loses.
    PerpetualCheckWin,
}

impl Repetition {
    /// The verdict as a negamax score, from the side to move.
    pub(crate) fn score(self) -> Score {
        match self {
            Self::Draw => Score::ZERO,
            Self::PerpetualCheckLoss => -Score::REPETITION,
            Self::PerpetualCheckWin => Score::REPETITION,
        }
    }
}

/// Whether the **last** position in `path` ends the game by repetition.
///
/// `path` carries one entry per position reached, oldest first — the shape
/// [`crate::Game::history`] hands out, and the shape the search extends as it
/// descends.
///
/// ⚠️ **The window is the four occurrences themselves, not every occurrence in
/// `path`.** A game following the rule ends at the fourth, so a fifth can only
/// come from a search line running past a finished game or from a test; taking
/// the fourth-from-last keeps the perpetual-check question about the repetition
/// that actually ended it.
///
/// ⚠️ **`key` filters and `hands` confirm, and dropping the second compiles and
/// stays silent.** shunsai's key already covers the hands, so the comparison
/// adds no information about shogi — it is a guard against a 64-bit collision
/// handing back a position that merely hashes alike. CLAUDE.md prescribes the
/// pair.
///
/// ⚠️ **A game rooted at a mid-game `position sfen …` cannot see a repetition
/// that began before its root**, because those plies are not in `path`. The
/// same limitation `Game::initial_sfen` carries.
pub(crate) fn verdict(path: &[HistoryEntry]) -> Option<Repetition> {
    let (current, earlier) = path.split_last()?;
    // The index of `current`, so the two runs below can be written as ranges
    // over `path` rather than over `earlier`.
    let now = earlier.len();

    // Back by two, because `path` alternates side to move — every entry is
    // one legal move on from the last, and a key carries the side to move, so
    // the other parity cannot match.
    //
    // ⚠️ **A null move breaks that and nothing here would notice.** Pushing an
    // entry for a pass shifts the parity of everything below it, and this walk
    // then compares `current` only against the opponent's positions and returns
    // `None` for every real repetition in the subtree. DESIGN.md's E1 item 5
    // owns it.
    let mut seen = 0;
    let mut first = None;
    let mut i = now;
    while i >= 2 {
        i -= 2;
        if path[i].key == current.key && path[i].hands == current.hands {
            seen += 1;
            if seen == OCCURRENCES - 1 {
                first = Some(i);
                break;
            }
        }
    }
    let first = first?;

    // At most one side is in check in any legal position, but *alternating*
    // checks are legal, so both runs can hold at once — an evasion may give
    // check back. The rule names one loser and has no answer for two, so the
    // pair falls to a draw.
    let we_checked = (first + 1..now).step_by(2).all(|i| path[i].in_check);
    let they_checked = (first + 2..=now).step_by(2).all(|i| path[i].in_check);
    Some(match (we_checked, they_checked) {
        (true, false) => Repetition::PerpetualCheckLoss,
        (false, true) => Repetition::PerpetualCheckWin,
        _ => Repetition::Draw,
    })
}

#[cfg(test)]
mod tests {
    use shogi_core::{Hand, PieceKind};

    use super::*;

    /// An entry with a distinguishable key and empty hands.
    fn entry(key: u64) -> HistoryEntry {
        HistoryEntry {
            key,
            hands: [Hand::default(); 2],
            in_check: false,
        }
    }

    /// `keys` as a path, oldest first. Each element also says whether the side
    /// to move at that position was in check.
    fn entries(keys: &[(u64, bool)]) -> Vec<HistoryEntry> {
        keys.iter()
            .map(|&(key, in_check)| HistoryEntry {
                in_check,
                ..entry(key)
            })
            .collect()
    }

    /// One cycle of a four-fold repetition: two positions alternating, `n`
    /// entries long, none of them in check.
    fn shuffle(n: usize) -> Vec<HistoryEntry> {
        (0..n)
            .map(|i| entry(if i.is_multiple_of(2) { 1 } else { 2 }))
            .collect()
    }

    #[test]
    fn nothing_to_report_on_a_short_or_empty_path() {
        assert_eq!(verdict(&[]), None);
        assert_eq!(verdict(&shuffle(1)), None);
        assert_eq!(verdict(&shuffle(2)), None);
    }

    /// The rule is four occurrences, so the third is not yet one.
    ///
    /// Sabotage: compare `seen` against `OCCURRENCES - 2`, or make the loop
    /// stop one match early, and the third occurrence reports a draw.
    #[test]
    fn the_fourth_occurrence_is_the_repetition_and_the_third_is_not() {
        // Indices 0, 2, 4 hold the same position; index 4 is its third.
        assert_eq!(verdict(&shuffle(5)), None);
        // 0, 2, 4, 6 — the fourth.
        assert_eq!(verdict(&shuffle(7)), Some(Repetition::Draw));
    }

    /// A key that matches with different hands is a collision, not a
    /// repetition.
    ///
    /// Sabotage: drop the `hands` comparison and this reports a draw. It is the
    /// only test that reaches that half of the filter, because every other
    /// fixture here holds empty hands throughout.
    #[test]
    fn a_key_match_with_different_hands_does_not_count() {
        let mut path = shuffle(7);
        path[2].hands[0] = Hand::default()
            .added(PieceKind::Pawn)
            .expect("an empty hand takes a pawn");
        assert_eq!(verdict(&path), None);
    }

    /// Black shuffles while checking White every time: Black is to move at the
    /// repeated position, and Black loses.
    ///
    /// Sabotage: swap the two runs' start offsets and the verdict inverts —
    /// the engine then seeks the perpetual check that loses it the game.
    #[test]
    fn the_side_to_move_loses_when_it_gave_every_check() {
        // Even indices: the repeated position, side to move not in check.
        // Odd indices: the opponent, in check every time.
        let checked = entries(&[
            (1, false),
            (2, true),
            (1, false),
            (2, true),
            (1, false),
            (2, true),
            (1, false),
        ]);
        assert_eq!(verdict(&checked), Some(Repetition::PerpetualCheckLoss));
    }

    /// The same shape from the other side: the opponent gave every check, so
    /// the side to move wins.
    #[test]
    fn the_opponent_loses_when_it_gave_every_check() {
        let checked = entries(&[
            (1, true),
            (2, false),
            (1, true),
            (2, false),
            (1, true),
            (2, false),
            (1, true),
        ]);
        assert_eq!(verdict(&checked), Some(Repetition::PerpetualCheckWin));
    }

    /// Both sides checking on every move is legal — an evasion may give check
    /// back — and the rule names one loser, so it falls to a draw.
    #[test]
    fn mutual_perpetual_check_is_a_draw() {
        let both = entries(&[
            (1, true),
            (2, true),
            (1, true),
            (2, true),
            (1, true),
            (2, true),
            (1, true),
        ]);
        assert_eq!(verdict(&both), Some(Repetition::Draw));
    }

    /// One quiet move anywhere in the run breaks it: 連続 means every move.
    ///
    /// Sabotage: `any` instead of `all` in the **`we_checked`** run, which
    /// reports a perpetual check that nobody delivered.
    ///
    /// ⚠️ **The same mutation in `they_checked` fires nothing, here or in any
    /// other test.** Every fixture in the module gives that run a uniform
    /// sequence, and `any` and `all` agree on a uniform sequence — so only the
    /// mixed run can tell them apart, and this is the only fixture with one.
    #[test]
    fn one_unchecked_ply_makes_it_an_ordinary_draw() {
        let mut nearly = entries(&[
            (1, false),
            (2, true),
            (1, false),
            (2, true),
            (1, false),
            (2, true),
            (1, false),
        ]);
        nearly[3].in_check = false;
        assert_eq!(verdict(&nearly), Some(Repetition::Draw));
    }

    /// Plies before the four occurrences say nothing about them.
    ///
    /// Sabotage: start the runs at the *oldest* match rather than the
    /// fourth-from-last and a fifth occurrence drags an unrelated earlier ply
    /// into the window.
    #[test]
    fn only_the_four_occurrences_are_in_the_window() {
        // Nine entries, five occurrences of the repeated position at
        // 0, 2, 4, 6, 8. The first cycle is quiet; the last three check.
        let five = entries(&[
            (1, false),
            (2, false),
            (1, false),
            (2, true),
            (1, false),
            (2, true),
            (1, false),
            (2, true),
            (1, false),
        ]);
        assert_eq!(verdict(&five), Some(Repetition::PerpetualCheckLoss));
    }

    #[test]
    fn a_draw_is_worth_nothing_and_a_perpetual_check_is_not() {
        assert_eq!(Repetition::Draw.score(), Score::ZERO);
        assert_eq!(
            Repetition::PerpetualCheckWin.score(),
            -Repetition::PerpetualCheckLoss.score()
        );
        assert!(Repetition::PerpetualCheckWin.score() > Score::ZERO);
        assert!(!Repetition::PerpetualCheckWin.score().is_mate());
        assert!(!Repetition::PerpetualCheckLoss.score().is_mate());
    }
}
