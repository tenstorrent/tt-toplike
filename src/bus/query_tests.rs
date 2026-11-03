//! Tests for `entities_with`. The worlds are built from records on a real
//! bus, with fixed values and no clock.

use super::*;
use tt_toplike_bus::{Bus, BusConfig, HeaderKind, LocalView, Payload, Provenance, Sample, Value};

/// The chip with PCI bus id `0000:0N:00.0`, so key order follows `n`.
fn chip(n: u32) -> EntityKey {
    EntityKey::chip(&format!("0000:{n:02}:00.0"))
}

/// A view of one world where each `(chip index, signals)` entry has a
/// reading of every listed signal.
fn view_with(chips: &[(u32, &[&str])]) -> LocalView {
    let bus = Bus::open_with_world(BusConfig::default()).unwrap();
    for (idx, signals) in chips {
        for sig in *signals {
            bus.publish(
                "test",
                Payload::Sample(Sample {
                    entity: chip(*idx),
                    signal: (*sig).into(),
                    value: Value::F64(1.0),
                    prov: Provenance::Measured,
                }),
            );
        }
    }
    let snap = bus.snapshot().unwrap();
    LocalView {
        seq: bus.head(),
        world: snap.world,
        now_mono_ns: 0,
        header_kind: HeaderKind::Live,
    }
}

/// Three chips, and chip 1 lacks `power_w`. Asking for both signals
/// returns chips 0 and 2 in key order; asking for `temp_c` alone returns
/// all three.
#[test]
fn entities_with_returns_only_entities_that_carry_every_signal() {
    let view = view_with(&[
        (2, &["temp_c", "power_w"]),
        (0, &["temp_c", "power_w"]),
        (1, &["temp_c"]),
    ]);
    assert_eq!(
        entities_with(&view, EntityKind::Chip, &["temp_c", "power_w"]),
        vec![chip(0), chip(2)],
    );
    assert_eq!(
        entities_with(&view, EntityKind::Chip, &["temp_c"]),
        vec![chip(0), chip(1), chip(2)],
    );
}

/// The check covers every named signal, including the third: a chip
/// missing it is out.
#[test]
fn entities_with_checks_a_third_signal_too() {
    let view = view_with(&[(0, &["a", "b", "c"]), (1, &["a", "b"])]);
    assert_eq!(
        entities_with(&view, EntityKind::Chip, &["a", "b", "c"]),
        vec![chip(0)],
    );
}

/// No named signals means every entity of the kind.
#[test]
fn entities_with_no_signals_returns_every_entity_of_the_kind() {
    let view = view_with(&[(1, &["temp_c"]), (0, &["power_w"])]);
    assert_eq!(
        entities_with(&view, EntityKind::Chip, &[]),
        vec![chip(0), chip(1)],
    );
}

/// A kind with no entities gives an empty list, with or without signals.
#[test]
fn entities_with_a_kind_that_has_no_entities_is_empty() {
    let view = view_with(&[(0, &["temp_c"])]);
    assert!(entities_with(&view, EntityKind::Model, &["temp_c"]).is_empty());
    assert!(entities_with(&view, EntityKind::Model, &[]).is_empty());
}
