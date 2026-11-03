//! Tests for the trailing maximum and mean.

use super::*;

#[test]
fn the_max_decays_slowly_and_never_below_the_value() {
    let mut t = Trail::new();
    t.push(4096.0);
    assert_eq!(t.max(), 4096.0);
    // One push of zero: the max loses exactly 1/4096 of itself.
    t.push(0.0);
    assert_eq!(t.max(), 4095.0);
    // A lower value does not drag the max down to it.
    t.push(10.0);
    assert!(t.max() > 4090.0 && t.max() < 4095.0);
    // A higher value lifts it at once.
    t.push(5000.0);
    assert_eq!(t.max(), 5000.0);
    // Decay never takes it below the value just pushed.
    let mut t = Trail::new();
    t.push(1.0);
    for _ in 0..100_000 {
        t.push(1.0);
        assert!(t.max() >= 1.0);
    }
    assert_eq!(t.max(), 1.0);
    // The mean moves 1/16 of the way toward the value.
    let mut t = Trail::new();
    t.push(16.0);
    assert_eq!(t.mean(), 16.0);
    t.push(0.0);
    assert_eq!(t.mean(), 15.0);
    assert_eq!(t.count(), 2);
}

#[test]
fn an_idle_series_gives_ratio_one() {
    let mut t = Trail::new();
    assert_eq!(t.ratio(0.0), 1.0);
    assert_eq!(t.ratio(7.0), 1.0);
    t.push(0.0);
    t.push(0.0);
    assert_eq!(t.ratio(0.0), 1.0);
    t.push(10.0);
    assert_eq!(t.ratio(5.0), 0.5);
}

#[test]
fn non_finite_pushes_are_ignored() {
    let mut t = Trail::new();
    t.push(8.0);
    let before = t;
    t.push(f64::NAN);
    t.push(f64::INFINITY);
    t.push(f64::NEG_INFINITY);
    assert_eq!(t, before);
    assert!(t.ratio(f64::NAN).is_finite());
}

// REGRESSION GUARD ONLY. This runs one pure computation twice in one process,
// so it can catch hidden state (a global, a hash order) but cannot go red on a
// wrong constant or a platform difference. The test below pins the bits.
#[test]
fn the_same_series_gives_bit_identical_results_twice() {
    let series: Vec<f64> = (0..5000u32)
        .map(|i| f64::from(i.wrapping_mul(2_654_435_761) % 1000) / 7.0)
        .collect();
    let run = || {
        let mut t = Trail::new();
        for &v in &series {
            t.push(v);
        }
        (
            t.max().to_bits(),
            t.mean().to_bits(),
            t.ratio(3.0).to_bits(),
        )
    };
    assert_eq!(run(), run());
}

#[test]
fn the_book_keeps_a_trail_per_entity_and_series() {
    let a = EntityKey::chip("0000:01:00.0");
    let b = EntityKey::chip("0000:02:00.0");
    let mut book = TrailBook::new();
    book.push(&a, "temp", 10.0);
    book.push(&b, "temp", 20.0);
    book.push(&a, "power", 30.0);
    assert_eq!(book.get(&a, "temp").unwrap().max(), 10.0);
    assert_eq!(book.get(&b, "temp").unwrap().max(), 20.0);
    assert_eq!(book.get(&a, "power").unwrap().max(), 30.0);
    assert!(book.get(&b, "power").is_none());
}

/// Known-answer test: the bits after a short series, worked out by hand.
///
/// Series 4096.0, 0.0, 10.0. Every step is exact in binary floating point.
///
/// * push 4096: mean = 4096, max = 4096.
/// * push 0: max = 4096 * 4095/4096 = 4095.
///   mean = 4096 + (0 - 4096)/16 = 4096 - 256 = 3840.
/// * push 10: decayed = 4095 * 4095/4096 = 16769025/4096 = 4094 + 1/4096,
///   since 4096 * 4094 = 16769024. That exceeds 10, so max = 4094 + 2^-12.
///   mean = 3840 + (10 - 3840)/16 = 3840 - 239.375 = 3600.625.
///
/// IEEE-754 bits, derived by hand (exponent 11, so the biased field is 0x40A):
///
/// * max = 4094 + 2^-12 = 2048 * (2 - 2^-10 + 2^-23). The mantissa field is
///   2^52 - 2^42 + 2^29 = 0xFFC0020000000, so the bits are 0x40AFFC0020000000.
/// * mean = 3600.625 = 2048 * (1 + 1552.625/2048). The mantissa field is
///   1552.625 * 2^41 = 12421 * 2^38 = 0x3085 << 38 = 0xC214000000000, so the
///   bits are 0x40AC214000000000.
#[test]
fn the_bits_after_a_short_series_match_hand_computed_constants() {
    let mut t = Trail::new();
    t.push(4096.0);
    t.push(0.0);
    assert_eq!(t.max().to_bits(), 4095.0_f64.to_bits());
    assert_eq!(t.mean().to_bits(), 3840.0_f64.to_bits());
    t.push(10.0);
    assert_eq!(t.max().to_bits(), 0x40AF_FC00_2000_0000);
    assert_eq!(t.mean().to_bits(), 0x40AC_2140_0000_0000);
    // The hex constants agree with the decimal values worked out above.
    assert_eq!(f64::from_bits(0x40AF_FC00_2000_0000), 4094.0 + 1.0 / 4096.0);
    assert_eq!(f64::from_bits(0x40AC_2140_0000_0000), 3600.625);
}
