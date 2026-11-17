//! Tests for the seeded PCG32 generator.

use super::*;

/// Reference output of the PCG32 demo program (`pcg32_srandom_r(&rng, 42, 54)`).
/// These values come from the published reference implementation, so they
/// check the algorithm and its platform stability independently of this code.
const REFERENCE_42_54: [u32; 6] = [
    0xa15c02b7, 0x7b47f409, 0xba1d3330, 0x83d2f293, 0xbfa4784b, 0xcbed606e,
];

#[test]
fn known_answer_matches_reference_pcg32() {
    let mut rng = Pcg32::new(42, 54);
    for (i, want) in REFERENCE_42_54.iter().enumerate() {
        assert_eq!(rng.next_u32(), *want, "output {i}");
    }
}

#[test]
fn from_seed_uses_the_reference_stream() {
    let mut a = Pcg32::from_seed(42);
    let mut b = Pcg32::new(42, 54);
    for _ in 0..16 {
        assert_eq!(a.next_u32(), b.next_u32());
    }
}

#[test]
fn range_is_unbiased_enough() {
    let mut rng = Pcg32::from_seed(7);
    let mut counts = [0u32; 6];
    for _ in 0..60_000 {
        let v = rng.range(10, 16);
        counts[(v - 10) as usize] += 1;
    }
    // Expected 10,000 per bucket; allow 5%.
    for (i, c) in counts.iter().enumerate() {
        assert!((9_500..=10_500).contains(c), "bucket {i} had {c}");
    }
}

#[test]
fn range_edge_cases() {
    let mut rng = Pcg32::from_seed(1);
    assert_eq!(rng.range(5, 6), 5);
    // An empty or inverted range returns lo.
    assert_eq!(rng.range(5, 5), 5);
    assert_eq!(rng.range(9, 3), 9);
    for _ in 0..1000 {
        assert!((0..u32::MAX).contains(&rng.range(0, u32::MAX)));
    }
}

#[test]
fn chance_edges_and_rate() {
    let mut rng = Pcg32::from_seed(3);
    assert!(!rng.chance(0, 5));
    assert!(rng.chance(5, 5));
    assert!(rng.chance(9, 5));
    assert!(!rng.chance(1, 0));
    let hits = (0..40_000).filter(|_| rng.chance(1, 4)).count();
    assert!((9_500..=10_500).contains(&hits), "hits {hits}");
}

#[test]
fn pick_handles_empty_and_returns_members() {
    let mut rng = Pcg32::from_seed(5);
    let empty: [u8; 0] = [];
    assert!(rng.pick(&empty).is_none());
    let items = [1, 2, 3];
    for _ in 0..100 {
        assert!(items.contains(rng.pick(&items).unwrap()));
    }
}

#[test]
fn shuffle_is_a_permutation_and_repeatable() {
    let original: Vec<u32> = (0..50).collect();
    let mut a = original.clone();
    let mut b = original.clone();
    Pcg32::from_seed(99).shuffle(&mut a);
    Pcg32::from_seed(99).shuffle(&mut b);
    assert_eq!(a, b, "same seed, same order");
    assert_ne!(a, original, "a 50-item shuffle should move something");
    let mut sorted = a.clone();
    sorted.sort();
    assert_eq!(sorted, original, "still a permutation");
    let mut c = original.clone();
    Pcg32::from_seed(100).shuffle(&mut c);
    assert_ne!(a, c, "different seed, different order");
}

#[test]
fn range_rejects_the_short_block_for_large_bounds() {
    // With bound = 3 * 2^30, a plain `r % bound` would give values below 2^30
    // twice the weight of the rest (1/2 instead of 1/3). Rejection sampling
    // keeps them at 1/3.
    let bound = 3u32 << 30;
    let mut rng = Pcg32::from_seed(11);
    let draws = 30_000;
    let low = (0..draws)
        .filter(|_| rng.range(0, bound) < (1u32 << 30))
        .count();
    assert!((9_500..=10_500).contains(&low), "low-third count {low}");
}

#[test]
fn shuffle_reaches_every_permutation_evenly() {
    // 3 items have 6 orders. A shuffle that skips a position would favour
    // some of them even though each result is still a valid permutation.
    let mut rng = Pcg32::from_seed(21);
    let mut seen = std::collections::BTreeMap::new();
    for _ in 0..6_000 {
        let mut v = [0u8, 1, 2];
        rng.shuffle(&mut v);
        *seen.entry(v).or_insert(0u32) += 1;
    }
    assert_eq!(seen.len(), 6);
    for (order, n) in &seen {
        assert!((850..=1_150).contains(n), "{order:?} appeared {n} times");
    }
}
