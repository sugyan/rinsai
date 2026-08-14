//! The time source a search measures its budget against.
//!
//! Apart from the search because a budget test cannot be written against a wall
//! clock: it would have to sleep, and a sleeping test measures the machine.

use core::time::Duration;
use std::time::Instant;

/// A monotonic time source.
///
/// `now()` returns time since an origin the implementation chose, not a
/// wall-clock reading. ⚠️ [`Instant`] has no public constructor — the only way
/// to obtain one is [`Instant::now`] — so a trait returning `Instant` could
/// only be implemented by something that reads the wall clock, which is the
/// one thing this trait exists to make optional.
///
/// ⚠️ **`now()` must be non-decreasing.** A deadline is computed from one
/// reading and compared against later ones, so a clock that steps backwards
/// postpones expiry rather than reporting anything wrong.
///
/// `Send` because [`Searcher`](crate::Searcher) is, and a searcher holding a
/// clock is only `Send` if the clock is.
pub trait Clock: Send {
    fn now(&self) -> Duration;
}

/// The clock a search uses when nobody supplies one.
#[derive(Clone, Copy, Debug)]
pub struct RealClock(Instant);

impl RealClock {
    #[must_use]
    pub fn new() -> Self {
        Self(Instant::now())
    }
}

impl Default for RealClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for RealClock {
    fn now(&self) -> Duration {
        self.0.elapsed()
    }
}
