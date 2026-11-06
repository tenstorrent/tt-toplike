//! The world seed, taken from the machine's own readings.
//!
//! A [`SeedPool`] keeps the full 64-bit pattern (`f64::to_bits`) of each of
//! the last 64 sample values in a ring, and hashes the ring into 64-bit
//! seeds. Two machines, or two runs, give different worlds. A replay of the
//! same observations gives the same world.
//!
//! # Why the whole value
//!
//! The readings that vary from one run to the next do so in different bits.
//! A temperature or power reading is an `f32` widened to `f64`, so the low
//! 29 bits of its `f64` pattern are always zero and its noise sits in bits
//! 29 to 51. An integer reading (a counter, a heartbeat, a clock) varies in
//! the high mantissa bits, and its low eight bits are zero below 2^45. Only
//! a native `f64` (the PCIe byte rates on real hardware) varies in the low
//! bits. Keeping the whole word keeps every bit that can change. An earlier
//! version kept only the lowest byte, which was zero for every widened `f32`
//! and every small integer, so a fresh start repeated its first world
//! (ledger ruling R26, final review finding I1).
//!
//! The pool sees only [`Observation`]s. An observation carries no sequence
//! number and no timestamp, so neither can reach the seed. Events and ticks
//! are skipped; only sample values count.
//!
//! The hash is FNV-1a 64. It uses integer operations only and is fixed by its
//! published definition, so it gives the same answer on every platform.

use crate::observe::Observation;

/// Number of values (64-bit words) the ring holds.
pub const RING_LEN: usize = 64;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;

/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// FNV-1a 64 over `bytes`: for each byte, XOR it into the hash, then multiply
/// by the prime (wrapping). Kept separate from the pool so it can be checked
/// against the published test vectors. The world lifecycle also uses it to
/// hash a chip key into that chip's floor seed.
pub(crate) fn fnv1a64(bytes: &[u8]) -> u64 {
    fnv1a64_extend(FNV_OFFSET, bytes)
}

/// Continues an FNV-1a 64 hash: `hash` is the value after some bytes, and
/// the result is the hash of those bytes followed by `bytes`. So
/// `fnv1a64_extend(fnv1a64(a), b) == fnv1a64(a ++ b)`. The saga's digest
/// uses it to hash a stream without keeping the stream.
pub(crate) fn fnv1a64_extend(hash: u64, bytes: &[u8]) -> u64 {
    let mut hash = hash;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A ring of the bit patterns of the last 64 sample values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPool {
    /// The ring of `f64::to_bits` words. Once 64 samples have arrived it
    /// holds the 64 newest. Before that, the unused slots are zero.
    ring: [u64; RING_LEN],
    /// Where the next word goes. It is also where the oldest word sits, so
    /// reading the ring from `next` onward goes oldest to newest.
    next: usize,
    /// Sample observations pushed so far. It keeps counting after the ring
    /// is full, so `pushed() >= 64` means every slot holds a real value.
    pushed: u64,
}

impl Default for SeedPool {
    fn default() -> Self {
        SeedPool::new()
    }
}

impl SeedPool {
    /// An empty pool: all zero words.
    pub fn new() -> SeedPool {
        SeedPool {
            ring: [0; RING_LEN],
            next: 0,
            pushed: 0,
        }
    }

    /// Takes in one observation. A sample's value goes into the ring whole,
    /// as its `f64::to_bits`, in place of the oldest word, so changing any
    /// bit of a value that is still in the ring changes the ring. Events and
    /// ticks are ignored.
    pub fn push(&mut self, o: &Observation) {
        if let Observation::Sample { value, .. } = o {
            self.ring[self.next] = value.to_bits();
            self.next = (self.next + 1) % RING_LEN;
            self.pushed += 1;
        }
    }

    /// The number of sample observations pushed so far. Events and ticks are
    /// not counted. The world lifecycle waits for 64 (a full ring) before it
    /// draws the first world's seed, so a fresh start does not seed from a
    /// ring that is still mostly zeros.
    pub fn pushed(&self) -> u64 {
        self.pushed
    }

    /// The seed for `ordinal`: FNV-1a 64 over the 64 ring words, oldest to
    /// newest, each as 8 bytes little-endian (512 bytes), then the 8 bytes of
    /// `ordinal` little-endian. Distinct ordinals almost always give
    /// distinct seeds from the same pool.
    pub fn draw(&self, ordinal: u64) -> u64 {
        // Oldest first: from `next` to the end, then from the start to `next`.
        let oldest_first = self.ring[self.next..].iter().chain(&self.ring[..self.next]);
        let mut hash = fnv1a64(&[]);
        for word in oldest_first {
            hash = fnv1a64_extend(hash, &word.to_le_bytes());
        }
        fnv1a64_extend(hash, &ordinal.to_le_bytes())
    }
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod seed_tests;
