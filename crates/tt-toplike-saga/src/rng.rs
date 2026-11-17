//! Seeded PCG32 generator (XSH-RR 64/32), integer arithmetic only.
//!
//! Every random choice in the saga core comes from this generator, so the same
//! seed always gives the same saga on every platform. It uses wrapping `u64`
//! and `u32` operations and nothing else: no floating point, no clock, no
//! external randomness crate. The algorithm and constants follow the PCG
//! reference implementation (pcg-random.org, `pcg32_srandom_r` and
//! `pcg32_random_r`).

/// The LCG multiplier from the PCG reference implementation.
const MULTIPLIER: u64 = 6_364_136_223_846_793_005;

/// Stream selector used by [`Pcg32::from_seed`]. It is the reference demo's
/// value, which lets the known-answer test use published outputs.
const DEFAULT_STREAM: u64 = 54;

/// A PCG32 generator: 64 bits of state, 32 bits of output per step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Pcg32 {
    /// The LCG state.
    state: u64,
    /// The stream increment. It is always odd.
    inc: u64,
}

impl Pcg32 {
    /// Seeds a generator exactly as the reference `pcg32_srandom_r` does:
    /// state starts at 0, `inc = (initseq << 1) | 1`, one step runs, `initstate`
    /// is added to the state, and a second step runs.
    pub fn new(initstate: u64, initseq: u64) -> Pcg32 {
        let mut rng = Pcg32 {
            state: 0,
            inc: (initseq << 1) | 1,
        };
        rng.next_u32();
        rng.state = rng.state.wrapping_add(initstate);
        rng.next_u32();
        rng
    }

    /// Seeds a generator from one number, on the default stream.
    pub fn from_seed(seed: u64) -> Pcg32 {
        Pcg32::new(seed, DEFAULT_STREAM)
    }

    /// Returns the next 32 random bits (XSH-RR output function).
    pub fn next_u32(&mut self) -> u32 {
        let old = self.state;
        self.state = old.wrapping_mul(MULTIPLIER).wrapping_add(self.inc);
        let xorshifted = (((old >> 18) ^ old) >> 27) as u32;
        let rot = (old >> 59) as u32;
        xorshifted.rotate_right(rot)
    }

    /// Returns a value in `lo..hi_exclusive`. An empty or inverted range
    /// returns `lo` without drawing.
    ///
    /// The draw is unbiased: it uses rejection sampling as the reference
    /// `pcg32_boundedrand_r` does, discarding values from the short final
    /// block so every result is equally likely.
    pub fn range(&mut self, lo: u32, hi_exclusive: u32) -> u32 {
        if hi_exclusive <= lo {
            return lo;
        }
        let bound = hi_exclusive - lo;
        // `bound.wrapping_neg() % bound` is 2^32 mod bound: the size of the
        // low block that would make some results more likely than others.
        let threshold = bound.wrapping_neg() % bound;
        loop {
            let r = self.next_u32();
            if r >= threshold {
                return lo + r % bound;
            }
        }
    }

    /// Returns true with probability `num / den`. A zero `den` is never true;
    /// `num >= den` is always true.
    pub fn chance(&mut self, num: u32, den: u32) -> bool {
        if den == 0 {
            return false;
        }
        if num >= den {
            return true;
        }
        self.range(0, den) < num
    }

    /// Picks one item, or `None` when the slice is empty.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            return None;
        }
        // Slices longer than u32::MAX are clamped; saga lists are tiny.
        let n = u32::try_from(items.len()).unwrap_or(u32::MAX);
        items.get(self.range(0, n) as usize)
    }

    /// Shuffles a slice in place (Fisher-Yates).
    pub fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let n = u32::try_from(i + 1).unwrap_or(u32::MAX);
            let j = self.range(0, n) as usize;
            items.swap(i, j);
        }
    }
}

#[cfg(test)]
#[path = "rng_tests.rs"]
mod rng_tests;
