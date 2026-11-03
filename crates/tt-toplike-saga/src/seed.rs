//! The world seed, taken from the machine's own jitter.
//!
//! The low bits of a measured float (a temperature, a power reading) are
//! noise: they change from one reading to the next and carry no meaning. A
//! [`SeedPool`] collects the lowest byte of each sample into a small ring and
//! hashes the ring into 64-bit seeds. Two machines, or two runs, give
//! different worlds. A replay of the same observations gives the same world.
//!
//! The pool sees only [`Observation`]s. An observation carries no sequence
//! number and no timestamp, so neither can reach the seed. Events and ticks
//! are skipped; only sample values count.
//!
//! The hash is FNV-1a 64. It uses integer operations only and is fixed by its
//! published definition, so it gives the same answer on every platform.

use crate::observe::Observation;

/// Number of bytes the ring holds.
const RING_LEN: usize = 64;

/// FNV-1a 64-bit offset basis.
const FNV_OFFSET: u64 = 14_695_981_039_346_656_037;

/// FNV-1a 64-bit prime.
const FNV_PRIME: u64 = 1_099_511_628_211;

/// FNV-1a 64 over `bytes`: for each byte, XOR it into the hash, then multiply
/// by the prime (wrapping). Kept separate from the pool so it can be checked
/// against the published test vectors.
fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = FNV_OFFSET;
    for &b in bytes {
        hash ^= u64::from(b);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

/// A ring of the last 64 low bytes seen in samples.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeedPool {
    /// The ring. Once 64 samples have arrived it holds the 64 newest bytes.
    /// Before that, the unused slots are zero.
    ring: [u8; RING_LEN],
    /// Where the next byte goes. It is also where the oldest byte sits, so
    /// reading the ring from `next` onward goes oldest to newest.
    next: usize,
}

impl Default for SeedPool {
    fn default() -> Self {
        SeedPool::new()
    }
}

impl SeedPool {
    /// An empty pool: all zero bytes.
    pub fn new() -> SeedPool {
        SeedPool {
            ring: [0; RING_LEN],
            next: 0,
        }
    }

    /// Takes in one observation. A sample contributes the low 8 bits of its
    /// value's `f64::to_bits`. The fold replaces the oldest byte in the ring
    /// with the new one, so changing any bit of a byte that is still in the
    /// ring changes the ring. Events and ticks are ignored.
    pub fn push(&mut self, o: &Observation) {
        if let Observation::Sample { value, .. } = o {
            // Truncation to the low byte is the point.
            self.ring[self.next] = (value.to_bits() & 0xff) as u8;
            self.next = (self.next + 1) % RING_LEN;
        }
    }

    /// The seed for `ordinal`: FNV-1a 64 over the 64 ring bytes, oldest to
    /// newest, then the 8 bytes of `ordinal` in little-endian order. Distinct
    /// ordinals give distinct seeds from the same pool.
    pub fn draw(&self, ordinal: u64) -> u64 {
        let mut bytes = [0u8; RING_LEN + 8];
        // Oldest first: from `next` to the end, then from the start to `next`.
        let tail = RING_LEN - self.next;
        bytes[..tail].copy_from_slice(&self.ring[self.next..]);
        bytes[tail..RING_LEN].copy_from_slice(&self.ring[..self.next]);
        bytes[RING_LEN..].copy_from_slice(&ordinal.to_le_bytes());
        fnv1a64(&bytes)
    }
}

#[cfg(test)]
#[path = "seed_tests.rs"]
mod seed_tests;
