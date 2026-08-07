//! Evaluation scores and search depths.
//!
//! Both are newtype-free of the mistakes that make them worth defining: a
//! [`Score`] is a distinct type from a node count, a depth or a millisecond
//! count, because sign errors in `-search(-beta, -alpha)` are the single
//! largest bug class in an alpha-beta engine and a bare `i32` catches none of
//! them.

use core::fmt;
use core::ops::{Add, AddAssign, Neg, Sub, SubAssign};

/// The deepest ply the search will ever reach, counting quiescence.
///
/// It sizes three things together so they cannot drift apart: the mate band
/// below, the search's per-ply state (the principal variation, and still only
/// that) and the move buffer. 128 is the conventional value; nothing here has
/// yet forced it.
///
/// The per-ply state was expected to gain a static evaluation at step 3 and did
/// not: quiescence computes its stand-pat as a local and nothing reads it
/// again. The second field — and with it the `Stack` struct step 2 declined to
/// write — arrives with E1's killers or its futility margins, whichever lands
/// first. Recorded because a prediction that did not come true is worth as much
/// as one that did.
pub const MAX_PLY: usize = 128;

/// A search depth, in **whole plies**, signed.
///
/// Signed because quiescence search runs at negative depth — which it does,
/// from E0 step 3a, counting down from zero towards `QS_MAX_PLIES`. There is
/// deliberately no fractional `ONE_PLY` scheme: modern engines converged away
/// from it, and it buys nothing that a reduction table does not.
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

    /// "No score recorded" — an empty transposition-table slot.
    ///
    /// **No caller yet**; its caller is E0 step 3b's transposition table. Kept
    /// under the rule in PROGRESS.md: a surface whose caller can be *named*
    /// stays, with the name written down.
    ///
    /// ⚠️ It is **not** a "nothing found yet" seed for a maximum. It compares
    /// as +32_002 — above every real score, [`Self::INFINITE`] included — so
    /// `best = Score::NONE; if score > best { … }` never fires, and the search
    /// keeps its first candidate forever without a single failing assertion.
    /// `Option<Score>` is the right shape for that, and the search uses it.
    ///
    /// ⚠️ It is also **not printable**. Sitting above the mate floor means
    /// [`Self::mate_plies`] answers `Some(-2)` for it and `Some(-1)` for
    /// [`Self::INFINITE`], so either one reaching an `info` line spells
    /// `score mate -2` — the engine announcing a loss it never found. `info.rs`
    /// asserts against both on the way to the wire.
    pub const NONE: Self = Self(32_002);

    /// The lowest absolute value that still means mate.
    const MATE_FLOOR: i32 = Self::MATE.0 - MAX_PLY as i32;

    /// A score in centipawns, where a pawn is 100.
    #[inline]
    #[must_use]
    pub const fn cp(centipawns: i32) -> Self {
        Self(centipawns)
    }

    /// The raw centipawn value.
    #[inline]
    #[must_use]
    pub const fn get(self) -> i32 {
        self.0
    }

    /// Mate delivered by the side to move, `ply` plies from the root.
    ///
    /// Nearer mates score higher, which is what makes the search prefer the
    /// shortest one.
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
    /// [`Self::NONE`] and [`Self::INFINITE`] answer `true` as well; neither is
    /// ever compared as an evaluation, so distinguishing them here would only
    /// invite a caller to rely on it.
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
    /// never masquerade as a mate.
    ///
    /// **No caller yet**; its caller is E3's inference boundary, where the
    /// network emits its own scale and the conversion into centipawns is
    /// unbounded by construction. Step 2's material evaluation deliberately
    /// does *not* use it: the largest balance a legal position can hold is far
    /// below the mate band, and `eval` asserts that instead — a clamp there
    /// would hide a broken value table rather than fail on it.
    #[inline]
    #[must_use]
    pub const fn clamp_to_eval(self) -> Self {
        let limit = Self::MATE_FLOOR - 1;
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

// The four arithmetic impls below have **no caller yet**, and are kept for the
// same reason as `Score::NONE` and `clamp_to_eval`: their callers are named.
// E1's aspiration windows widen with `alpha - delta` and `beta + delta`, and
// step 3b's transposition table adjusts a stored mate score by ply on the way
// in and out. Step 3a needs none of it — the evaluator accumulates in `i32` and
// wraps once, the mate constructors produce mate scores, and the root window is
// `±INFINITE`.

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
            Self::NONE => f.write_str("Score::NONE"),
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
        assert!(Score::mate_in(MAX_PLY) > Score::cp(30_000));
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

    /// A static evaluation must never be able to claim mate, however extreme
    /// the material count gets.
    #[test]
    fn clamping_keeps_evaluations_out_of_the_mate_band() {
        assert!(!Score::cp(999_999).clamp_to_eval().is_mate());
        assert!(!Score::cp(-999_999).clamp_to_eval().is_mate());
        assert_eq!(Score::cp(120).clamp_to_eval(), Score::cp(120));
    }

    #[test]
    fn debug_reads_as_the_domain_not_the_integer() {
        assert_eq!(format!("{:?}", Score::cp(-42)), "Score(-42 cp)");
        assert_eq!(format!("{:?}", Score::mate_in(3)), "Score(mate 3)");
        assert_eq!(format!("{:?}", Score::INFINITE), "Score::INFINITE");
    }
}
