//! Tests for the world seed.

use super::*;
use tt_toplike_bus::{EntityKey, Event};

/// A sample whose `to_bits` low byte is `low` and whose other 56 bits are
/// taken from `high` (its low byte is ignored).
fn sample(entity: &str, signal: &str, high: u64, low: u8) -> Observation {
    Observation::Sample {
        entity: EntityKey::chip(entity),
        signal: signal.into(),
        value: f64::from_bits((high & !0xff) | u64::from(low)),
    }
}

fn pool_of(bytes: &[u8]) -> SeedPool {
    let mut p = SeedPool::new();
    for &b in bytes {
        p.push(&sample("0000:01:00.0", "temp", 0x3ff0_0000_0000_0000, b));
    }
    p
}

/// A fixed, varied byte stream longer than the ring, so wrap-around is used.
fn stream() -> Vec<u8> {
    (0..150u32).map(|i| (i * 37 + 11) as u8).collect()
}

#[test]
fn fnv1a_matches_the_published_vectors() {
    // From the FNV reference test suite (isthe.com/chongo/tech/comp/fnv).
    assert_eq!(fnv1a64(b""), 0xcbf2_9ce4_8422_2325);
    assert_eq!(fnv1a64(b"a"), 0xaf63_dc4c_8601_ec8c);
    assert_eq!(fnv1a64(b"foobar"), 0x8594_4171_f739_67e8);
}

#[test]
fn the_same_observations_give_the_same_seed() {
    let a = pool_of(&stream());
    let b = pool_of(&stream());
    assert_eq!(a.draw(0), b.draw(0));
    assert_eq!(a.draw(7), b.draw(7));
    // Different ordinals give different draws from one pool.
    assert_ne!(a.draw(0), a.draw(1));
}

#[test]
fn one_changed_low_bit_changes_the_seed() {
    let base = stream();
    let want = pool_of(&base).draw(3);
    // Flip bit 0 of bytes that are still in the ring (the last 64 of 150).
    for idx in [86, 100, 149] {
        let mut changed = base.clone();
        changed[idx] ^= 1;
        assert_ne!(pool_of(&changed).draw(3), want, "byte {idx}");
    }
    // A byte that has already left the ring no longer matters.
    let mut old = base.clone();
    old[0] ^= 1;
    assert_eq!(pool_of(&old).draw(3), want);
    // Every single bit of one byte matters.
    for bit in 0..8 {
        let mut changed = base.clone();
        changed[120] ^= 1 << bit;
        assert_ne!(pool_of(&changed).draw(3), want, "bit {bit}");
    }
}

#[test]
fn the_seed_ignores_everything_but_the_ring_and_the_ordinal() {
    let bytes = stream();
    let want = pool_of(&bytes).draw(5);
    let mut p = SeedPool::new();
    for (i, &b) in bytes.iter().enumerate() {
        // Other entity, other signal, other high bits: same low byte.
        p.push(&sample(
            "0000:02:00.0",
            "power",
            0x4000_0000_0000_0000 + ((i as u64) << 8),
            b,
        ));
        // Events and ticks do not enter the ring.
        p.push(&Observation::Tick);
        p.push(&Observation::Event(Event::ModelGone {
            model: EntityKey::model("m"),
        }));
    }
    assert_eq!(p.draw(5), want);
}

#[test]
fn pushed_counts_only_samples() {
    let mut p = SeedPool::new();
    assert_eq!(p.pushed(), 0);
    for i in 0..70u8 {
        p.push(&sample("0000:01:00.0", "temp", 0x3ff0_0000_0000_0000, i));
        // Ticks and events are not counted.
        p.push(&Observation::Tick);
        p.push(&Observation::Event(Event::ModelGone {
            model: EntityKey::model("m"),
        }));
    }
    // The count keeps going past the ring's length.
    assert_eq!(p.pushed(), 70);
}

#[test]
fn extending_a_hash_equals_hashing_the_joined_bytes() {
    use crate::seed::fnv1a64_extend;
    assert_eq!(fnv1a64_extend(fnv1a64(b"foo"), b"bar"), fnv1a64(b"foobar"));
    assert_eq!(fnv1a64_extend(fnv1a64(b""), b"a"), 0xaf63_dc4c_8601_ec8c);
}
