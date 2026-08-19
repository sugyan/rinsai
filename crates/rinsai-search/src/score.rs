//! Evaluation scores and search depths.
//!
//! [`Score`] is a newtype rather than an `i32` because sign errors in
//! `-search(-beta, -alpha)` are the largest bug class in an alpha-beta engine
//! and a bare integer catches none of them.

use core::fmt;
use core::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// The deepest ply the search will ever reach, counting quiescence.
///
/// It sizes three things together so they cannot drift apart: the mate band
/// below, the search's per-ply state and the move buffer. 128 is the
/// conventional value; nothing here has yet forced it.
pub const MAX_PLY: usize = 128;

/// A search depth, in **whole plies**, signed.
///
/// Signed because quiescence runs at negative depth, entering at zero and
/// counting down towards `-QS_MAX_CHECK_PLIES`. ⚠️ Note which constant that
/// is: it counts *checked* plies, not plies, so a capture chain never moves it.
pub type Depth = i32;

/// An evaluation, in **centipawns**, from the point of view of the side to move.
///
/// Positive is good for whoever is to move at that node; a parent negates its
/// child. At the root the side to move *is* the engine, so USI's `score cp`
/// needs no flip.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(transparent)]
pub struct Score(i32);

impl Score {
    /// A dead-equal position, and the value of a draw by repetition.
    pub const ZERO: Self = Self(0);

    /// Mate delivered at the root. Real mate scores are `MATE - ply`, so every
    /// score with an absolute value above `MATE - MAX_PLY` is a mate score.
    pub const MATE: Self = Self(32_000);

    /// The alpha-beta window's outer bound. Strictly greater than [`Self::MATE`]
    /// so that a mate score fits inside an open window, and small enough that
    /// `-INFINITE` is representable without overflow.
    pub const INFINITE: Self = Self(32_001);

    /// The lowest absolute value that still means mate.
    const MATE_FLOOR: i32 = Self::MATE.0 - MAX_PLY as i32;

    /// A game won by 連続王手の千日手 — the opponent checked its way into a
    /// fourfold repetition and loses. Negated, the same repetition lost.
    ///
    /// It sits in a band of its own between the evaluations and the mates, so
    /// [`Self::is_mate`] answers `false` and the deepening loop does not break
    /// on it. Two consequences worth knowing:
    ///
    /// * ⚠️ **`info` spells it `score cp`**, because USI has no vocabulary for
    ///   a win that is not a mate. Reporting it as `score mate` would announce
    ///   a mate whose principal variation does not deliver one.
    /// * ⚠️ **It carries no distance to the root**, unlike a mate score, and
    ///   needs none: a repetition ends the game where it stands rather than
    ///   `n` plies further on. That keeps the table's mate-by-ply adjustment
    ///   the only ply-relative score in the engine. It does **not** mean the
    ///   value stays out of the table.
    pub const REPETITION: Self = Self(30_000);

    /// A score in centipawns, where a pawn is 100.
    #[inline]
    #[must_use]
    pub const fn cp(centipawns: i32) -> Self {
        Self(centipawns)
    }

    #[inline]
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }

    /// Mate delivered by the side to move, `ply` plies from the root. Nearer
    /// mates score higher, which is what makes the search prefer the shortest.
    #[inline]
    #[must_use]
    pub const fn mate_in(ply: usize) -> Self {
        debug_assert!(ply <= MAX_PLY);
        Self(Self::MATE.0 - ply as i32)
    }

    /// Mate delivered *against* the side to move, `ply` plies from the root.
    #[inline]
    #[must_use]
    pub const fn mated_in(ply: usize) -> Self {
        debug_assert!(ply <= MAX_PLY);
        Self(-Self::MATE.0 + ply as i32)
    }

    /// Whether this is a mate score rather than an ordinary evaluation.
    ///
    /// [`Self::INFINITE`] answers `true` as well; it is never compared as an
    /// evaluation, so distinguishing it here would only invite a caller to rely
    /// on it.
    #[inline]
    #[must_use]
    pub const fn is_mate(self) -> bool {
        self.0.abs() >= Self::MATE_FLOOR
    }

    /// Plies to mate: positive when the side to move mates, negative when it is
    /// mated, `None` for an ordinary evaluation.
    ///
    /// USI's `score mate` is reported in plies, so this is the number to print.
    #[inline]
    #[must_use]
    pub const fn mate_plies(self) -> Option<i32> {
        if self.0 >= Self::MATE_FLOOR {
            Some(Self::MATE.0 - self.0)
        } else if self.0 <= -Self::MATE_FLOOR {
            Some(-Self::MATE.0 - self.0)
        } else {
            None
        }
    }

    /// Clamps into the ordinary-evaluation band, so a static evaluation can
    /// never masquerade as a mate or as a repetition. Caller: E3's inference
    /// boundary, where the network's own scale makes the conversion unbounded
    /// by construction.
    ///
    /// ⚠️ Material evaluation deliberately does **not** use it: `eval` asserts
    /// its own range instead, because a clamp there would hide a broken value
    /// table rather than fail on it.
    #[inline]
    #[must_use]
    pub const fn clamp_to_eval(self) -> Self {
        let limit = Self::REPETITION.0 - 1;
        if self.0 > limit {
            Self(limit)
        } else if self.0 < -limit {
            Self(-limit)
        } else {
            self
        }
    }
}

impl Neg for Score {
    type Output = Self;
    #[inline]
    fn neg(self) -> Self {
        Self(-self.0)
    }
}

// Callers for the four impls below: E1's aspiration windows (`alpha - delta`,
// `beta + delta`) and the transposition table's mate-score-by-ply adjustment.

impl Add<i32> for Score {
    type Output = Self;
    #[inline]
    fn add(self, rhs: i32) -> Self {
        Self(self.0 + rhs)
    }
}

impl Sub<i32> for Score {
    type Output = Self;
    #[inline]
    fn sub(self, rhs: i32) -> Self {
        Self(self.0 - rhs)
    }
}

impl AddAssign<i32> for Score {
    #[inline]
    fn add_assign(&mut self, rhs: i32) {
        self.0 += rhs;
    }
}

impl SubAssign<i32> for Score {
    #[inline]
    fn sub_assign(&mut self, rhs: i32) {
        self.0 -= rhs;
    }
}

impl fmt::Debug for Score {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match *self {
            Self::INFINITE => f.write_str("Score::INFINITE"),
            s if s.0.abs() == Self::REPETITION.0 => {
                let outcome = if s.0 > 0 { "won" } else { "lost" };
                write!(f, "Score(repetition {outcome})")
            }
            s => match s.mate_plies() {
                Some(plies) => write!(f, "Score(mate {plies})"),
                None => write!(f, "Score({} cp)", s.0),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The window must be able to hold every mate score, and `-INFINITE` must
    /// be representable — the two properties that decide the constants.
    #[test]
    fn infinite_brackets_every_mate_score() {
        assert!(Score::INFINITE > Score::MATE);
        assert!(-Score::INFINITE < -Score::MATE);
        assert!(Score::INFINITE.get().checked_neg().is_some());
    }

    /// Three bands, in order: evaluations, then a repetition won or lost, then
    /// the mates. Every rule that reads a score reads one of the boundaries, so
    /// they are asserted as one chain rather than one at a time.
    ///
    /// Sabotage: raise `REPETITION` into the mate band and `is_mate` starts
    /// answering `true` for it, which makes the deepening loop break on a
    /// repetition as though it were a proof of mate.
    #[test]
    fn the_three_bands_do_not_overlap() {
        let repetition = Score::REPETITION;
        assert!(Score::cp(0).clamp_to_eval() < repetition);
        assert!(Score::cp(i32::MAX).clamp_to_eval() < repetition);
        assert!(repetition < Score::mate_in(MAX_PLY));
        assert!(Score::mate_in(MAX_PLY) < Score::INFINITE);

        assert!(!repetition.is_mate());
        assert!(!(-repetition).is_mate());
        assert_eq!(repetition.mate_plies(), None);
    }

    /// A nearer mate must score higher, or the search has no reason to prefer
    /// the shorter one.
    #[test]
    fn nearer_mates_score_higher() {
        assert!(Score::mate_in(1) > Score::mate_in(5));
        assert!(Score::mated_in(1) < Score::mated_in(5));
        assert_eq!(-Score::mate_in(7), Score::mated_in(7));
    }

    /// Sabotage: widen the mate band (`MATE_FLOOR` too low) and an ordinary
    /// evaluation starts reporting as mate — this is the test that catches it.
    #[test]
    fn ordinary_evaluations_are_not_mates() {
        assert!(!Score::ZERO.is_mate());
        assert!(!Score::cp(2_500).is_mate());
        assert!(!Score::cp(-2_500).is_mate());
        assert_eq!(Score::cp(150).mate_plies(), None);

        assert!(Score::mate_in(0).is_mate());
        assert!(Score::mate_in(MAX_PLY).is_mate());
        assert!(Score::mated_in(MAX_PLY).is_mate());
    }

    #[test]
    fn mate_plies_round_trips() {
        for ply in [0, 1, 2, 17, MAX_PLY] {
            assert_eq!(Score::mate_in(ply).mate_plies(), Some(ply as i32));
            assert_eq!(Score::mated_in(ply).mate_plies(), Some(-(ply as i32)));
        }
    }

    /// A static evaluation must never be able to claim a mate *or* a
    /// repetition, however extreme the material count gets.
    #[test]
    fn clamping_keeps_evaluations_below_both_upper_bands() {
        assert!(!Score::cp(999_999).clamp_to_eval().is_mate());
        assert!(!Score::cp(-999_999).clamp_to_eval().is_mate());
        assert!(Score::cp(999_999).clamp_to_eval() < Score::REPETITION);
        assert!(Score::cp(-999_999).clamp_to_eval() > -Score::REPETITION);
        assert_eq!(Score::cp(120).clamp_to_eval(), Score::cp(120));
    }

    #[test]
    fn debug_reads_as_the_domain_not_the_integer() {
        assert_eq!(format!("{:?}", Score::cp(-42)), "Score(-42 cp)");
        assert_eq!(format!("{:?}", Score::mate_in(3)), "Score(mate 3)");
        assert_eq!(format!("{:?}", Score::INFINITE), "Score::INFINITE");
        assert_eq!(format!("{:?}", Score::REPETITION), "Score(repetition won)");
        assert_eq!(
            format!("{:?}", -Score::REPETITION),
            "Score(repetition lost)"
        );
    }
}
