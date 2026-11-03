//! Tests for the world seed.

use super::*;
use tt_toplike_bus::{EntityKey, Event};

/// A sample with value `value`.
fn sample(entity: &str, signal: &str, value: f64) -> Observation {
    Observation::Sample {
        entity: EntityKey::chip(entity),
        signal: signal.into(),
        value,
    }
}

fn pool_of(values: &[f64]) -> SeedPool {
    let mut p = SeedPool::new();
    for &v in values {
        p.push(&sample("0000:01:00.0", "temp", v));
    }
    p
}

/// An `f32` reading widened to `f64`, the way the backend source sends
/// temperatures and power. Its `f64` bits end in 29 zeros, so its low byte
/// is always 0.
fn widened(x: f32) -> f64 {
    f64::from(x)
}

/// `x` with the lowest mantissa bit of its `f32` form flipped, widened.
/// This is the smallest change a real `f32` reading can make.
fn nudged(x: f32) -> f64 {
    f64::from(f32::from_bits(x.to_bits() ^ 1))
}

/// A fixed stream of values the backend can produce, longer than the ring
/// so wrap-around is used: `f32` temperatures and powers widened to `f64`,
/// and integer counters.
fn stream() -> Vec<f64> {
    (0..150u32)
        .map(|i| match i % 3 {
            0 => widened(45.0 + i as f32 * 0.137),
            1 => widened(60.25 - i as f32 * 0.011),
            _ => f64::from(1000 + i),
        })
        .collect()
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

/// The smallest change a real `f32` reading can make, its lowest mantissa
/// bit, changes the seed. Before the final fix wave the ring kept the low
/// byte of the `f64` bits, which is 0 for every widened `f32`, and this
/// failed (finding I1).
#[test]
fn the_lowest_bit_of_an_f32_reading_changes_the_seed() {
    let base = stream();
    let want = pool_of(&base).draw(3);
    // Values still in the ring (the last 64 of 150), at both ends and in
    // the middle, of each of the three kinds in the stream.
    for idx in [86, 87, 100, 148] {
        let mut changed = base.clone();
        let x = changed[idx] as f32;
        assert_eq!(f64::from(x), changed[idx], "value {idx} is an f32");
        changed[idx] = nudged(x);
        assert_ne!(pool_of(&changed).draw(3), want, "value {idx}");
    }
    // A value that has already left the ring no longer matters.
    let mut old = base.clone();
    old[0] = nudged(old[0] as f32);
    assert_eq!(pool_of(&old).draw(3), want);
}

/// Integer readings (counters, heartbeats, clocks, fan speeds) change the
/// seed. An integer below 2^45 has eight zero low bits as an `f64`, so the
/// old low-byte ring gave 1, 2 and 3 the same seed (finding I1).
#[test]
fn small_integer_readings_change_the_seed() {
    let seeds: Vec<u64> = [1.0, 2.0, 3.0]
        .iter()
        .map(|&v| {
            let mut values = vec![7.0; 64];
            values[40] = v;
            pool_of(&values).draw(0)
        })
        .collect();
    assert_ne!(seeds[0], seeds[1]);
    assert_ne!(seeds[1], seeds[2]);
    assert_ne!(seeds[0], seeds[2]);
}

/// Every one of a value's 64 bits reaches the seed.
#[test]
fn every_bit_of_a_value_changes_the_seed() {
    let base = stream();
    let want = pool_of(&base).draw(3);
    for bit in 0..64 {
        let mut changed = base.clone();
        changed[120] = f64::from_bits(changed[120].to_bits() ^ (1 << bit));
        assert_ne!(pool_of(&changed).draw(3), want, "bit {bit}");
    }
}

/// The draw hashes the ring words little-endian, oldest first, then the
/// ordinal little-endian. Pinned on a hand-built byte string so the order
/// cannot change without this test seeing it.
#[test]
fn the_draw_hashes_the_words_oldest_first_then_the_ordinal() {
    let values: Vec<f64> = (0..70u32).map(|i| f64::from(i) + 0.5).collect();
    let pool = pool_of(&values);
    let mut bytes = Vec::new();
    // The ring holds the last 64 values, 6 to 69, oldest first.
    for v in &values[6..] {
        bytes.extend_from_slice(&v.to_bits().to_le_bytes());
    }
    bytes.extend_from_slice(&9u64.to_le_bytes());
    assert_eq!(bytes.len(), 64 * 8 + 8);
    assert_eq!(pool.draw(9), fnv1a64(&bytes));
}

#[test]
fn the_seed_ignores_everything_but_the_ring_and_the_ordinal() {
    let values = stream();
    let want = pool_of(&values).draw(5);
    let mut p = SeedPool::new();
    for &v in &values {
        // Other entity, other signal: the same value.
        p.push(&sample("0000:02:00.0", "power", v));
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
        p.push(&sample("0000:01:00.0", "temp", f64::from(i)));
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
