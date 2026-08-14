//! Deterministic sampling: a seeded generator and a shuffle, and nothing else.
//!
//! No `rand`: the whole point of a seed in a provenance header is that the
//! byte stream behind it never moves, and a dependency's algorithm is not
//! ours to freeze. splitmix64 is Sebastiano Vigna's public-domain generator;
//! shunsai's `examples/gen_bench_positions.rs` (MIT, same author) uses the
//! same pair for the same reason. ⚠️ That path is in shunsai's **repository**;
//! the published crate ships `src/` only, so the provenance scan CLAUDE.md §2
//! requires has to read it there.

/// The next value of the generator, advancing `state`.
pub fn splitmix64(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// Fisher–Yates driven by [`splitmix64`].
///
/// The modulo bias at 64 bits is unobservable at any slice length this crate
/// shuffles; determinism, not uniformity to the last bit, is the contract.
pub fn shuffle<T>(items: &mut [T], state: &mut u64) {
    for i in (1..items.len()).rev() {
        #[allow(clippy::cast_possible_truncation)]
        let j = (splitmix64(state) % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The first outputs from a zero seed, as published for the algorithm.
    /// Any change to the constants or the mixing steps moves these, and with
    /// them every committed file that records a seed.
    #[test]
    fn the_generator_matches_the_published_sequence() {
        let mut state = 0u64;
        assert_eq!(splitmix64(&mut state), 0xE220_A839_7B1D_CDAF);
        assert_eq!(splitmix64(&mut state), 0x6E78_9E6A_A1B9_65F4);
        assert_eq!(splitmix64(&mut state), 0x06C4_5D18_8009_454F);
    }

    #[test]
    fn one_seed_is_one_permutation() {
        let mut a: Vec<u32> = (0..64).collect();
        let mut b: Vec<u32> = (0..64).collect();
        let mut state_a = 42u64;
        let mut state_b = 42u64;
        shuffle(&mut a, &mut state_a);
        shuffle(&mut b, &mut state_b);
        assert_eq!(a, b);

        let mut c: Vec<u32> = (0..64).collect();
        let mut state_c = 43u64;
        shuffle(&mut c, &mut state_c);
        assert_ne!(a, c, "a neighbouring seed gives another order");

        let mut sorted = a.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..64).collect::<Vec<_>>(), "still a permutation");
    }

    #[test]
    fn short_slices_survive_a_shuffle() {
        let mut state = 1u64;
        let mut empty: [u32; 0] = [];
        shuffle(&mut empty, &mut state);
        let mut one = [7u32];
        shuffle(&mut one, &mut state);
        assert_eq!(one, [7]);
    }
}
