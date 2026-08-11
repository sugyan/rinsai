//! The transposition table.
//!
//! One slot per position, holding what an earlier visit proved: a [`Score`]
//! with the [`Bound`] and depth it was proved at, and the move that was best
//! there. The move pays even where the score is unusable.
//!
//! ⚠️ **A hit is a hit on a Zobrist key and nothing else.** A 64-bit collision
//! is unlikely, not impossible, and the move that comes back is then from
//! another position — which `do_move`, validating nothing, will panic on.

use core::fmt;

use shogi_core::{CompactMove, Move};

use crate::score::{Depth, Score};

/// The table size [`NegamaxSearcher::new`](crate::NegamaxSearcher::new) takes,
/// in MiB.
///
/// ⚠️ **`USI_Hash` advertises this same number and the two must not drift**, or
/// the engine reports a configuration it does not have. The option table reads
/// it from here; `usi::options` has the test that keeps the pair honest.
pub const DEFAULT_HASH_MB: usize = 256;

/// What a stored score says about the position's true value.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Bound {
    /// A node that failed low: the true value is **at most** the stored score.
    Upper,
    /// A node that failed high: the true value is **at least** the stored score.
    Lower,
    /// The value was inside the window, so it is the value.
    Exact,
}

impl Bound {
    /// The bits of [`Entry::age_bound`] a bound occupies.
    const MASK: u8 = 0b11;

    fn code(self) -> u8 {
        match self {
            Self::Upper => 1,
            Self::Lower => 2,
            Self::Exact => 3,
        }
    }

    /// `None` for code 0, which is what makes an all-zero [`Entry`] read as
    /// empty.
    fn from_code(age_bound: u8) -> Option<Self> {
        match age_bound & Self::MASK {
            1 => Some(Self::Upper),
            2 => Some(Self::Lower),
            3 => Some(Self::Exact),
            _ => None,
        }
    }
}

/// How much [`Table::new_search`] advances the generation by — one step past
/// the bits [`Bound`] owns.
const GENERATION_STEP: u8 = Bound::MASK + 1;

/// One slot.
///
/// ⚠️ **The move is a [`CompactMove`] because `Option<CompactMove>` is two
/// bytes *guaranteed*** — `#[repr(transparent)]` over a `NonZeroU16`, so the
/// niche is promised. [`Move`] carries no `#[repr]`, so its size may change
/// between compiler versions, and here that would change how many positions fit
/// in the operator's MiB. Same rule as `moves.rs`'s buffer reservation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Entry {
    key: u64,
    score: i16,
    mv: Option<CompactMove>,
    depth: i8,
    /// The search generation, already shifted clear of the bottom two bits,
    /// `|`-ed with [`Bound::code`]. Zero means no bound, which means empty.
    age_bound: u8,
}

/// How many slots fit in one MiB.
const ENTRIES_PER_MB: usize = (1 << 20) / size_of::<Entry>();

/// What [`Table::allocate`] falls back to when the allocator refuses. The
/// smallest size `USI_Hash` admits, so it is a size the engine already claims
/// to work at.
const FALLBACK_MB: usize = 1;

/// What a probe found.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Hit {
    pub(crate) score: Score,
    pub(crate) depth: Depth,
    pub(crate) bound: Bound,
    /// ⚠️ Not known to be legal here — see the module doc.
    pub(crate) mv: Option<Move>,
}

pub(crate) struct Table {
    entries: Vec<Entry>,
    /// One below the slot count, which is a power of two, so this is the index
    /// mask.
    mask: usize,
    /// Already shifted clear of the bits [`Bound`] owns, so it can be compared
    /// against a slot's `age_bound & !Bound::MASK` directly.
    generation: u8,
}

impl fmt::Debug for Table {
    /// A summary, not the slots. ⚠️ Deriving this would make `{:?}` on a
    /// searcher print millions of entries.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Table")
            .field("entries", &self.entries.len())
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl Table {
    /// A cleared table of `mb` MiB, or as close below it as a power of two
    /// gets — see [`Self::entries_for_mb`].
    pub(crate) fn new(mb: usize) -> Self {
        let (entries, len) = Self::allocate(Self::entries_for_mb(mb));
        Self {
            entries,
            mask: len - 1,
            generation: 0,
        }
    }

    /// How many slots `mb` MiB buys, rounded **down** to a power of two.
    ///
    /// ⚠️ **A table can therefore be nearly half the size that was asked for**,
    /// because `key & mask` needs a power of two. What must not follow is the
    /// engine reporting the size it was *asked* for: [`Self::mib`] and
    /// [`Self::hashfull`] describe the table that exists.
    ///
    /// Separate from [`Self::new`] so its test need not allocate.
    fn entries_for_mb(mb: usize) -> usize {
        // Never zero, so `mask` is meaningful and `probe` always has somewhere
        // to look.
        let wanted = mb.saturating_mul(ENTRIES_PER_MB).max(1);
        1usize << wanted.ilog2()
    }

    /// `len` cleared slots, or [`FALLBACK_MB`] worth if the allocator refuses.
    ///
    /// ⚠️ **Fallible on purpose.** `USI_Hash` advertises 64 GiB as its maximum,
    /// which almost no machine can give, and `vec![_; len]` answers a refusal by
    /// aborting the process — from the worker thread, mid-tournament, on a value
    /// the engine itself offered. Returning a smaller table keeps the promise
    /// `usi.rs` opens with: bad input never stops the loop.
    fn allocate(len: usize) -> (Vec<Entry>, usize) {
        let mut entries = Vec::new();
        if entries.try_reserve_exact(len).is_ok() {
            // Cannot reallocate: the capacity was just reserved.
            entries.resize(len, Entry::default());
            return (entries, len);
        }
        let fallback = FALLBACK_MB * ENTRIES_PER_MB;
        (vec![Entry::default(); fallback], fallback)
    }

    /// The table's real size in MiB, which is what an operator has to be told
    /// when it is not the size they asked for.
    pub(crate) fn mib(&self) -> usize {
        self.entries.len() / ENTRIES_PER_MB
    }

    /// Reallocates to `mb` MiB, dropping everything stored.
    ///
    /// ⚠️ **It clears even when the size is unchanged**, or what is in the
    /// table would depend on the order the `setoption` lines arrived in.
    pub(crate) fn resize(&mut self, mb: usize) {
        let len = Self::entries_for_mb(mb);
        if len == self.entries.len() {
            self.clear();
            return;
        }
        // The old allocation goes back before the new one is asked for: held at
        // once, a resize would need both.
        self.entries = Vec::new();
        let (entries, len) = Self::allocate(len);
        self.entries = entries;
        self.mask = len - 1;
        self.generation = 0;
    }

    /// Empties every slot. Called on `usinewgame`, where positions from the
    /// previous game are not merely stale but describe a different tree.
    pub(crate) fn clear(&mut self) {
        self.entries.fill(Entry::default());
        self.generation = 0;
    }

    /// Marks the start of a search, so that entries from earlier ones become
    /// the first candidates for replacement without being erased — they are
    /// still facts about their positions, and still worth a hit.
    pub(crate) fn new_search(&mut self) {
        self.generation = self.generation.wrapping_add(GENERATION_STEP);
    }

    /// What an earlier visit to `key` proved, or `None`.
    ///
    /// `ply` is the probing node's distance from the search root, and is what
    /// rewrites a stored mate distance into this node's frame.
    pub(crate) fn probe(&self, key: u64, ply: usize) -> Option<Hit> {
        let entry = self.entries[key as usize & self.mask];
        if entry.key != key {
            return None;
        }
        // After the key, because an empty slot has key 0 and a position whose
        // key is 0 is a legal thing for shunsai to hand over.
        let bound = Bound::from_code(entry.age_bound)?;
        Some(Hit {
            score: from_table(entry.score, ply),
            depth: Depth::from(entry.depth),
            bound,
            // Spelled out: `Move` and `CompactMove` convert both ways, so a
            // bare `.map(Move::from)` is ambiguous rather than obvious.
            mv: entry.mv.map(<Move as From<CompactMove>>::from),
        })
    }

    /// Records what this visit proved.
    ///
    /// ⚠️ **`score` must be a value the search actually established.** A
    /// placeholder from an abandoned subtree outlives the search that made it
    /// and is indistinguishable from a real entry afterwards.
    pub(crate) fn store(
        &mut self,
        key: u64,
        ply: usize,
        depth: Depth,
        bound: Bound,
        score: Score,
        mv: Option<Move>,
    ) {
        debug_assert!(
            score != Score::INFINITE,
            "a sentinel reached the table: {score:?}"
        );
        let (Ok(score), Ok(depth)) = (i16::try_from(to_table(score, ply)), i8::try_from(depth))
        else {
            // Unreachable — `every_reachable_score_and_depth_fits` checks it.
            // Skipped rather than truncated: a missing entry costs nodes, a
            // truncated one costs the answer.
            return;
        };

        let slot = &mut self.entries[key as usize & self.mask];
        let occupied = Bound::from_code(slot.age_bound).is_some();
        let stale = slot.age_bound & !Bound::MASK != self.generation;
        // Depth-preferred within a search, and anything from an earlier one
        // gives way. ⚠️ Conventional and untuned: nothing here has measured a
        // replacement policy against another.
        if occupied && !stale && depth < slot.depth {
            return;
        }

        // A fail-low node has no move to offer, and the one a deeper visit to
        // this same position already found is worth more than nothing.
        let mv = mv
            .map(<CompactMove as From<Move>>::from)
            .or_else(|| (slot.key == key).then_some(slot.mv).flatten());

        *slot = Entry {
            key,
            score,
            mv,
            depth,
            age_bound: self.generation | bound.code(),
        };
    }

    /// How full the table is, in **permille** — USI's `hashfull` unit.
    ///
    /// Sampled over the first thousand slots rather than counted, because a
    /// count walks the whole table and this is read once per iteration.
    ///
    /// ⚠️ **Only entries from the current search count**, so a table carried
    /// into a new search reads as empty and fills again.
    pub(crate) fn hashfull(&self) -> u32 {
        const SAMPLE: usize = 1_000;
        let sample = SAMPLE.min(self.entries.len());
        let used = self.entries[..sample]
            .iter()
            .filter(|entry| {
                Bound::from_code(entry.age_bound).is_some()
                    && entry.age_bound & !Bound::MASK == self.generation
            })
            .count();
        // `sample` is never zero: `entries_for_mb` floors at one slot.
        (used * 1_000 / sample) as u32
    }
}

/// Rewrites a mate score from "plies from the search root" into "plies from the
/// storing node", which is what makes the entry usable at another depth.
/// Ordinary evaluations pass through.
///
/// ⚠️ **This and [`from_table`] are one mechanism**: applying either alone makes
/// the engine announce a mate distance it cannot play.
fn to_table(score: Score, ply: usize) -> i32 {
    let raw = score.get();
    if !score.is_mate() {
        return raw;
    }
    let ply = ply as i32;
    if raw > 0 { raw + ply } else { raw - ply }
}

/// The inverse of [`to_table`], for a node `ply` from the root.
fn from_table(stored: i16, ply: usize) -> Score {
    let score = Score::cp(i32::from(stored));
    if !score.is_mate() {
        return score;
    }
    let ply = ply as i32;
    if stored > 0 { score - ply } else { score + ply }
}

#[cfg(test)]
mod tests {
    use shogi_core::{Piece, Square};

    use super::*;
    use crate::score::MAX_PLY;

    /// A 1 MiB table — what every test here wants, and what the rest of the
    /// suite wants too. ⚠️ Nothing in a test may take [`DEFAULT_HASH_MB`]:
    /// `usi_conformance.rs` alone drives thirty-one dialogues.
    fn table() -> Table {
        Table::new(1)
    }

    fn mv(from: (u8, u8), to: (u8, u8)) -> Move {
        Move::Normal {
            from: Square::new(from.0, from.1).expect("a square"),
            to: Square::new(to.0, to.1).expect("a square"),
            promote: false,
        }
    }

    /// The two numbers the MiB arithmetic rests on.
    ///
    /// ⚠️ **It does not catch storing a [`Move`] instead** — measured, and
    /// `Option<Move>` fits the same sixteen bytes today. [`Entry`] forbids
    /// `Move` because its size is *unpromised*, which is exactly what no size
    /// assertion can test; what this pins is the guarantee underneath.
    #[test]
    fn a_slot_is_sixteen_bytes_and_a_compact_move_is_two() {
        assert_eq!(size_of::<Option<CompactMove>>(), 2);
        assert_eq!(size_of::<Entry>(), 16);
        assert_eq!(ENTRIES_PER_MB, 65_536);
    }

    /// An all-zero slot has to read as empty, or a cleared table answers every
    /// probe with a stored draw.
    #[test]
    fn a_zeroed_slot_is_empty() {
        assert_eq!(Bound::from_code(0), None);
        assert_eq!(Entry::default().age_bound, 0);
        assert!(table().probe(0, 0).is_none());
        assert!(table().probe(12_345, 0).is_none());
    }

    /// Every bound survives the round trip through two bits, and none of them
    /// collides with the generation above it.
    #[test]
    fn a_bound_round_trips_through_every_generation() {
        let mut generation: u8 = 0;
        for _ in 0..64 {
            for bound in [Bound::Upper, Bound::Lower, Bound::Exact] {
                let packed = generation | bound.code();
                assert_eq!(Bound::from_code(packed), Some(bound));
                assert_eq!(packed & !Bound::MASK, generation);
            }
            generation = generation.wrapping_add(GENERATION_STEP);
        }
        assert_eq!(generation, 0, "the generation did not cycle cleanly");
    }

    #[test]
    fn a_stored_entry_comes_back() {
        let mut tt = table();
        let expected = mv((7, 7), (7, 6));
        tt.store(
            0xdead_beef,
            0,
            7,
            Bound::Exact,
            Score::cp(-42),
            Some(expected),
        );

        let hit = tt.probe(0xdead_beef, 0).expect("the entry is there");
        assert_eq!(hit.score, Score::cp(-42));
        assert_eq!(hit.depth, 7);
        assert_eq!(hit.bound, Bound::Exact);
        assert_eq!(hit.mv, Some(expected));

        assert!(tt.probe(0xdead_bee0, 0).is_none(), "another key hit");
    }

    /// A drop is the other move shape, and it is the one whose colour lives in
    /// the piece rather than in the notation.
    #[test]
    fn a_stored_drop_keeps_its_piece_and_colour() {
        let mut tt = table();
        let expected = Move::Drop {
            piece: Piece::W_S,
            to: Square::SQ_5B,
        };
        tt.store(1, 0, 1, Bound::Lower, Score::ZERO, Some(expected));
        assert_eq!(tt.probe(1, 0).and_then(|hit| hit.mv), Some(expected));
    }

    /// A mate `distance` plies beyond a position must read as the same mate
    /// from any node, each in its own frame.
    ///
    /// ⚠️ **It goes through [`Table::store`] and [`Table::probe`], not through
    /// `to_table`/`from_table`.** Testing the two functions directly leaves the
    /// *call sites* uncovered: deleting either one from the table's own methods
    /// then passes the whole suite, `bench` included, and the engine announces
    /// mate distances it cannot play.
    ///
    /// Sabotage: drop the `to_table` call from `store`, or the `from_table`
    /// call from `probe`.
    #[test]
    fn a_mate_score_survives_being_read_at_another_ply() {
        let mut tt = table();
        for (stored_at, read_at) in [(0, 0), (3, 3), (5, 1), (1, 5), (100, 20), (20, 100)] {
            for distance in [0, 1, 7] {
                for mating in [true, false] {
                    let (stored, expected) = if mating {
                        (
                            Score::mate_in(stored_at + distance),
                            Score::mate_in(read_at + distance),
                        )
                    } else {
                        (
                            Score::mated_in(stored_at + distance),
                            Score::mated_in(read_at + distance),
                        )
                    };
                    tt.store(7, stored_at, 1, Bound::Exact, stored, None);
                    let hit = tt.probe(7, read_at).expect("the entry is there");
                    assert_eq!(
                        hit.score, expected,
                        "mate {distance} away, stored at {stored_at}, read at {read_at}"
                    );
                    assert!(hit.score.is_mate());
                }
            }
        }
    }

    /// An ordinary evaluation must not be touched by the mate adjustment, or
    /// every score in the table drifts by its ply. Through the table, for the
    /// reason above.
    #[test]
    fn an_ordinary_score_is_stored_unchanged() {
        let mut tt = table();
        for cp in [0, 1, -1, 215, -2_500, 30_000, -30_000] {
            let score = Score::cp(cp);
            tt.store(7, 17, 1, Bound::Exact, score, None);
            assert_eq!(tt.probe(7, 42).expect("stored").score, score);
        }
    }

    /// The conversions in `store` are total over everything the search can
    /// produce — which is why refusing to write is unreachable rather than a
    /// silent hole in the table.
    #[test]
    fn every_reachable_score_and_depth_fits() {
        for ply in [0, 1, MAX_PLY] {
            for score in [
                Score::mate_in(ply),
                Score::mated_in(ply),
                Score::cp(30_000),
                Score::cp(-30_000),
                Score::ZERO,
            ] {
                assert!(
                    i16::try_from(to_table(score, ply)).is_ok(),
                    "{score:?} at ply {ply}"
                );
            }
        }
        // `MAX_PLY - 1` is the deepest iteration `negamax.rs` will start.
        assert!(i8::try_from(MAX_PLY as i32 - 1).is_ok());
    }

    /// A deeper result must not be thrown away for a shallower one arriving
    /// later in the same search.
    ///
    /// Sabotage: replace unconditionally.
    #[test]
    fn a_shallower_result_does_not_evict_a_deeper_one() {
        let mut tt = table();
        tt.new_search();
        tt.store(7, 0, 8, Bound::Exact, Score::cp(100), None);
        tt.store(7, 0, 2, Bound::Exact, Score::cp(-100), None);
        let hit = tt.probe(7, 0).expect("the deep entry is still there");
        assert_eq!(hit.depth, 8);
        assert_eq!(hit.score, Score::cp(100));
    }

    /// …but a new search may have the slot, however deep the old entry was.
    #[test]
    fn a_new_search_may_replace_a_deeper_entry_from_the_last_one() {
        let mut tt = table();
        tt.new_search();
        tt.store(7, 0, 8, Bound::Exact, Score::cp(100), None);
        tt.new_search();
        tt.store(7, 0, 2, Bound::Exact, Score::cp(-100), None);
        assert_eq!(tt.probe(7, 0).expect("replaced").depth, 2);
    }

    /// A fail-low node stores no move; the one already there is worth keeping.
    ///
    /// Sabotage: overwrite the move unconditionally.
    #[test]
    fn a_store_without_a_move_keeps_the_one_already_there() {
        let mut tt = table();
        let kept = mv((2, 7), (2, 6));
        tt.store(9, 0, 4, Bound::Exact, Score::ZERO, Some(kept));
        tt.store(9, 0, 4, Bound::Upper, Score::cp(-30), None);

        let hit = tt.probe(9, 0).expect("the entry is there");
        assert_eq!(hit.bound, Bound::Upper);
        assert_eq!(hit.mv, Some(kept), "the move was thrown away");
    }

    /// …and it must not carry a move across to a *different* position that
    /// happens to land on the same slot. That move is not legal there.
    #[test]
    fn a_different_position_does_not_inherit_the_slots_move() {
        let mut tt = table();
        let len = tt.entries.len() as u64;
        tt.store(3, 0, 4, Bound::Exact, Score::ZERO, Some(mv((2, 7), (2, 6))));
        // Same index, different key.
        tt.store(3 + len, 0, 4, Bound::Upper, Score::ZERO, None);
        assert_eq!(tt.probe(3 + len, 0).expect("stored").mv, None);
    }

    #[test]
    fn clearing_empties_the_table() {
        let mut tt = table();
        tt.store(5, 0, 3, Bound::Exact, Score::ZERO, None);
        assert!(tt.probe(5, 0).is_some());
        tt.clear();
        assert!(tt.probe(5, 0).is_none());
    }

    #[test]
    fn resizing_empties_the_table() {
        for mb in [1, 2] {
            let mut tt = table();
            tt.store(5, 0, 3, Bound::Exact, Score::ZERO, None);
            tt.resize(mb);
            assert!(tt.probe(5, 0).is_none(), "survived a resize to {mb} MiB");
            assert_eq!(tt.entries.len(), Table::entries_for_mb(mb));
        }
    }

    /// The size arithmetic, without allocating any of it. ⚠️ The rounding is
    /// *down*, so the 3 MiB row below buys two.
    #[test]
    fn a_table_is_a_power_of_two_slots_no_larger_than_asked_for() {
        for (mb, expected) in [
            (1, 65_536),
            (2, 131_072),
            (3, 131_072),
            (4, 262_144),
            (256, 16_777_216),
            (65_536, 4_294_967_296),
        ] {
            let len = Table::entries_for_mb(mb);
            assert_eq!(len, expected, "{mb} MiB");
            assert!(len.is_power_of_two());
            assert!(len * size_of::<Entry>() <= mb * (1 << 20));
        }
        // The option table's `min 1` is a commitment that this works, and the
        // floor keeps `mask` meaningful whatever is asked for.
        assert_eq!(Table::entries_for_mb(0), 1);
    }

    /// A size the allocator cannot give must produce a smaller table, not a
    /// dead process. `USI_Hash`'s advertised maximum is 64 GiB.
    ///
    /// The request below overflows `isize` bytes, so `try_reserve_exact` refuses
    /// on arithmetic alone and no allocator is troubled — the same branch a
    /// genuine out-of-memory takes, reached deterministically.
    ///
    /// Sabotage: go back to `vec![Entry::default(); len]` and this fails on a
    /// `capacity overflow` panic from inside `Vec`. ⚠️ That is the *polite* half
    /// of what is being fixed — a size that overflows nothing and merely exceeds
    /// free memory takes the `handle_alloc_error` path instead, which aborts the
    /// process rather than panicking, and no test can catch that.
    #[test]
    fn a_table_too_large_to_allocate_falls_back_instead_of_aborting() {
        let (entries, len) = Table::allocate(1 << 60);
        assert_eq!(len, FALLBACK_MB * ENTRIES_PER_MB);
        assert_eq!(entries.len(), len);
        assert!(entries.iter().all(|entry| *entry == Entry::default()));
    }

    /// …and the size an operator is told about is the size that exists.
    #[test]
    fn mib_reports_the_table_that_exists() {
        for mb in [1, 2, 4] {
            assert_eq!(Table::new(mb).mib(), mb);
        }
        // Rounded down to a power of two, so three MiB buys two — and `mib`
        // says two rather than three.
        assert_eq!(Table::new(3).mib(), 2);
    }

    /// `hashfull` counts this search's entries, in permille, and an empty table
    /// reads zero rather than "unknown".
    #[test]
    fn hashfull_counts_this_searchs_entries() {
        let mut tt = table();
        tt.new_search();
        assert_eq!(tt.hashfull(), 0);

        // The sample is the first thousand slots and the index is `key & mask`,
        // so keys 0..250 land one per slot inside it.
        for key in 0..250u64 {
            tt.store(key, 0, 1, Bound::Exact, Score::ZERO, None);
        }
        assert_eq!(tt.hashfull(), 250);

        // A new search has not used any of it yet, even though the entries are
        // still there and still answer a probe.
        tt.new_search();
        assert_eq!(tt.hashfull(), 0);
        assert!(tt.probe(1, 0).is_some());
    }
}
