// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Tests of the bus overlay on its own: from a world to an [`Overlay`], and
//! from an overlay to the effects a frame draws. The frame-level tests (what
//! the view draws) are in `tests.rs` and `perf_tests.rs`.

use super::*;
use crate::animation::die3d::layout::{DieLayout, TileKind};
use crate::animation::die3d::test_backend::{chip_key, BusFixture, SEC_NS};
use tt_toplike_bus::{HeaderKind, Reading};

/// Two drawn chips, device indices 0 and 1.
fn two_chips() -> Vec<(usize, EntityKey)> {
    vec![(0, chip_key(0)), (1, chip_key(1))]
}

/// The overlay of `f` at `now`, with fresh memory.
fn once(f: &BusFixture, now: u64) -> Overlay {
    let mut st = OverlayState::default();
    Overlay::from_world(&f.view(now), &two_chips(), now, &mut st)
}

#[test]
fn an_empty_world_gives_an_empty_overlay_and_no_effects() {
    let f = BusFixture::new();
    let o = once(&f, 5 * SEC_NS);
    assert!(o.is_empty(), "{o:?}");
    assert!(o.fx_at(5 * SEC_NS, false).is_empty());
    // Readings the overlay does not draw (a chip's temperature) change
    // nothing either.
    let mut f = BusFixture::new();
    f.sample(
        &chip_key(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(55.0),
        Provenance::Measured,
    );
    assert!(once(&f, 5 * SEC_NS).is_empty());
}

#[test]
fn a_ready_model_flags_its_own_chip_only() {
    let mut f = BusFixture::new();
    f.ready("llama", "Llama 3.1 8B", &[0], 42.4);
    let o = once(&f, 5 * SEC_NS);
    let flag = o.flag(0).expect("chip 0 carries the model");
    assert_eq!(flag.texts()[0], "Llama 3.1 8B ready 42t/s");
    assert_eq!(flag.colour, SERVE_TEAL);
    assert!(o.flag(1).is_none(), "chip 1 runs nothing");
}

#[test]
fn flag_text_carries_progress_waiting_and_the_estimate_mark() {
    let mut flag = ModelFlag {
        label: "A very long model name".into(),
        phase: ModelPhase::Loading,
        colour: LOAD_PURPLE,
        tokens_per_s: None,
        waiting: None,
        progress: Some(0.5),
        estimated: false,
    };
    assert_eq!(flag.texts()[0], "A very long model name loading 50%");
    assert_eq!(
        flag.texts().last().unwrap(),
        "A very long\u{2026} loading 50%"
    );
    flag.phase = ModelPhase::Ready;
    flag.progress = None;
    flag.tokens_per_s = Some(7.25);
    flag.waiting = Some(3);
    flag.estimated = true;
    let texts = flag.texts();
    assert_eq!(texts[0], "~A very long model name ready 7.2t/s 3 waiting");
    assert!(texts.iter().all(|t| t.starts_with('~')), "{texts:?}");
}

#[test]
fn estimated_readings_mark_the_flag() {
    let mut f = BusFixture::new();
    let m = EntityKey::model("mock");
    f.sample(
        &m,
        tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
        Value::F64(12.0),
        Provenance::Estimated,
    );
    f.event(Event::ModelReady {
        model: m,
        chips: vec![chip_key(0)],
    });
    let o = once(&f, 5 * SEC_NS);
    assert!(o.flag(0).unwrap().texts()[0].starts_with("~mock ready 12t/s"));
}

/// A world that hands out models whose chip lists are empty, as an attached
/// world does for another machine's models once `ReadView::only_chips` has
/// cut them.
struct Foreign(tt_toplike_bus::LocalView);

impl WorldRead for Foreign {
    fn seq(&self) -> u64 {
        self.0.seq()
    }
    fn models(&self) -> Vec<ModelView> {
        let mut m = self.0.models();
        m.iter_mut().for_each(|m| m.chips.clear());
        m
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<ModelView> {
        self.0.model_on_chip(chip).map(|mut m| {
            m.chips.clear();
            m
        })
    }
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<Reading> {
        self.0.reading(entity, signal)
    }
    fn events_since(&self, seq: u64) -> Vec<(u64, Event)> {
        self.0.events_since(seq)
    }
    fn reset_seq(&self, chip: &EntityKey) -> Option<u64> {
        self.0.reset_seq(chip)
    }
    fn header_kind(&self) -> HeaderKind {
        self.0.header_kind()
    }
    fn now_mono_ns(&self) -> u64 {
        self.0.now_mono_ns()
    }
}

#[test]
fn a_model_with_no_chips_draws_nothing() {
    let mut f = BusFixture::new();
    // Announced with no chips at all.
    f.event(Event::ModelReady {
        model: EntityKey::model("nowhere"),
        chips: vec![],
    });
    let mut st = OverlayState::default();
    let now = 5 * SEC_NS;
    assert!(Overlay::from_world(&f.view(now), &two_chips(), now, &mut st).is_empty());
    f.event(Event::RequestsCompleted {
        model: EntityKey::model("nowhere"),
        n: 5,
    });
    assert!(Overlay::from_world(&f.view(now), &two_chips(), now, &mut st).is_empty());

    // A foreign model: on chip 0 in the world, but its chip list cut to
    // nothing. No flag, no fill, no pulse.
    let mut f = BusFixture::new();
    f.loading("foreign", &[0], Some(0.5));
    let mut st = OverlayState::default();
    let w = Foreign(f.view(now));
    assert!(Overlay::from_world(&w, &two_chips(), now, &mut st).is_empty());
    f.event(Event::RequestsCompleted {
        model: EntityKey::model("foreign"),
        n: 5,
    });
    let w = Foreign(f.view(now));
    assert!(Overlay::from_world(&w, &two_chips(), now, &mut st).is_empty());
}

/// The die columns whose Tensix tiles `fx` tints, on row 5.
fn tinted_columns(fx: &ChipFx) -> Vec<u8> {
    let base = [10, 10, 10];
    DieLayout::blackhole()
        .tiles
        .iter()
        .filter(|t| t.kind == TileKind::Tensix && t.die.1 == 5)
        .filter(|t| fx.tint(t.die.0 as f32, base) != base)
        .map(|t| t.die.0)
        .collect()
}

/// Every Tensix column of a Blackhole die, on row 5.
fn tensix_columns() -> Vec<u8> {
    DieLayout::blackhole()
        .tiles
        .iter()
        .filter(|t| t.kind == TileKind::Tensix && t.die.1 == 5)
        .map(|t| t.die.0)
        .collect()
}

#[test]
fn loading_progress_fills_from_the_dram_edge_toward_the_middle() {
    let now = 5 * SEC_NS;
    let at = |p: f32| {
        let mut f = BusFixture::new();
        f.loading("m", &[0], Some(p));
        let o = once(&f, now);
        assert_eq!(o.chips[0].fill, Some(Fill::Progress(p)));
        o.fx_at(now, false).remove(&0).expect("chip 0 has effects")
    };
    // Half way: the Tensix columns at depths 1/8 to 4/8 from either DRAM
    // edge, half of the eight steps from the edge to the middle. Column 15
    // (depth 1/8 from the east edge) holds no Tensix tile.
    let half = tinted_columns(&at(0.5));
    assert_eq!(half, vec![1, 2, 3, 4, 12, 13, 14]);
    assert!(half.iter().all(|&x| tile_depth(x as f32) <= 0.5));
    assert!(tensix_columns()
        .iter()
        .filter(|&&x| tile_depth(x as f32) <= 0.5)
        .all(|x| half.contains(x)));
    // All the way: every Tensix column.
    assert_eq!(tinted_columns(&at(1.0)), tensix_columns());
    // Nothing yet: nothing tinted.
    assert!(tinted_columns(&at(0.0)).is_empty());
}

#[test]
fn loading_without_progress_sweeps_and_says_so() {
    let mut f = BusFixture::new();
    f.loading("m", &[0], None);
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    // First sight: the sweep runs from 0, a fixed origin.
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    assert_eq!(o.chips[0].fill, Some(Fill::Sweep { since_ns: 0 }));
    // The band moves with time.
    let a = tinted_columns(&o.fx_at(t0, false)[&0]);
    let b = tinted_columns(&o.fx_at(t0 + SEC_NS, false)[&0]);
    assert_ne!(a, b);
}

#[test]
fn ready_after_loading_fills_then_settles_over_two_seconds() {
    let mut f = BusFixture::new();
    f.loading("m", &[0], Some(0.9));
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    f.ready("m", "M", &[0], 1.0);
    let t1 = t0 + SEC_NS;
    let o = Overlay::from_world(&f.view(t1), &two_chips(), t1, &mut st);
    assert_eq!(o.chips[0].fill, Some(Fill::Settle { since_ns: t1 }));
    // Full at once, fading after.
    let full = o.fx_at(t1, false)[&0].fill;
    assert_eq!(
        full,
        Some(FillFx::UpTo {
            depth: 1.0,
            weight: FILL_WEIGHT
        })
    );
    let half = o.fx_at(t1 + SEC_NS, false)[&0].fill.unwrap();
    assert!(matches!(half, FillFx::UpTo { weight, .. } if weight < FILL_WEIGHT && weight > 0.0));
    // Two seconds on, the normal glow: no fill, in the effects or the overlay.
    assert!(o
        .fx_at(t1 + 2 * SEC_NS, false)
        .get(&0)
        .and_then(|fx| fx.fill)
        .is_none());
    let t3 = t1 + 2 * SEC_NS;
    let o = Overlay::from_world(&f.view(t3), &two_chips(), t3, &mut st);
    assert!(o.chips[0].fill.is_none());
}

#[test]
fn a_model_already_ready_at_first_sight_gets_no_settle() {
    let mut f = BusFixture::new();
    f.loading("m", &[0], Some(0.9));
    f.ready("m", "M", &[0], 1.0);
    assert!(once(&f, 5 * SEC_NS).chips[0].fill.is_none());
}

#[test]
fn a_same_phase_reannounce_changes_nothing() {
    // The host re-announces every model every five minutes. A `ModelReady`
    // for a model already ready must not replay a bring-up or a pulse.
    let mut f = BusFixture::new();
    f.loading("m", &[0], Some(0.9));
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    f.ready("m", "M", &[0], 30.0);
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    // Long after the settle.
    let t1 = t0 + 300 * SEC_NS;
    let before = Overlay::from_world(&f.view(t1), &two_chips(), t1, &mut st);
    f.event(Event::ModelReady {
        model: EntityKey::model("m"),
        chips: vec![chip_key(0)],
    });
    let after = Overlay::from_world(&f.view(t1), &two_chips(), t1, &mut st);
    assert_eq!(before, after);
    assert!(after.chips[0].fill.is_none() && after.chips[0].pulses.is_empty());
}

#[test]
fn requests_completed_pulses_the_models_chips_and_fades_in_a_second() {
    let mut f = BusFixture::new();
    f.ready("m", "M", &[0, 1], 30.0);
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    // First sight: the history makes no pulse.
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    assert!(o.chips.iter().all(|c| c.pulses.is_empty()));
    f.event(Event::RequestsCompleted {
        model: EntityKey::model("m"),
        n: 3,
    });
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    for c in &o.chips {
        assert_eq!(c.pulses.len(), 1, "chip {}", c.index);
    }
    // Drawn half a second in, gone after a second and a half.
    let lit = |t: u64| o.fx_at(t, false).get(&0).map_or(0.0, |fx| fx.pulse);
    assert!(lit(t0) > lit(t0 + SEC_NS / 2) && lit(t0 + SEC_NS / 2) > 0.0);
    assert_eq!(lit(t0 + 3 * SEC_NS / 2), 0.0);
    // The next update drops it from the overlay too.
    let t2 = t0 + 3 * SEC_NS / 2;
    let o = Overlay::from_world(&f.view(t2), &two_chips(), t2, &mut st);
    assert!(o.chips.iter().all(|c| c.pulses.is_empty()));
}

#[test]
fn more_requests_make_a_brighter_pulse_and_extra_pulses_merge() {
    assert!(strength(1) < strength(5));
    assert_eq!(strength(50), 1.0);
    let mut f = BusFixture::new();
    f.ready("m", "M", &[0], 30.0);
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    for _ in 0..7 {
        f.event(Event::RequestsCompleted {
            model: EntityKey::model("m"),
            n: 1,
        });
    }
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    let pulses = &o.chips[0].pulses;
    assert_eq!(pulses.len(), MAX_PULSES);
    assert!(pulses.last().unwrap().strength > pulses[0].strength);
}

#[test]
fn each_gddr_row_takes_its_own_channel_and_falls_back_without_one() {
    let mut f = BusFixture::new();
    f.gddr_temp(0, 0, 85.0).gddr_temp(0, 5, 45.0);
    let now = 5 * SEC_NS;
    let o = once(&f, now);
    let fx = &o.fx_at(now, false)[&0];
    let heat = |ch: usize| fx.gddr[ch].and_then(|g| g.heat);
    assert!((heat(0).unwrap() - 0.9).abs() < 1e-6);
    assert!((heat(5).unwrap() - 0.1).abs() < 1e-6);
    // Channels without a reading take the chip-wide value.
    assert!(fx.gddr[1].is_none());
    // Chip 1 has no per-channel data at all.
    assert!(o.chips.iter().all(|c| c.index == 0));
}

#[test]
fn a_harvested_or_disabled_channel_is_dim() {
    let mut f = BusFixture::new();
    let c0 = EntityKey::dram_channel(&chip_key(0), 2);
    let c1 = EntityKey::dram_channel(&chip_key(0), 3);
    f.sample(
        &c0,
        crate::bus::signals::DRAM_HARVESTED,
        Value::U64(1),
        Provenance::Measured,
    );
    f.sample(
        &c1,
        crate::bus::signals::DRAM_ENABLED,
        Value::U64(0),
        Provenance::Measured,
    );
    let now = 5 * SEC_NS;
    let fx = &once(&f, now).fx_at(now, false)[&0];
    assert!(fx.gddr[2].unwrap().dim && fx.gddr[3].unwrap().dim);
}

#[test]
fn an_ecc_counter_increase_flashes_its_row() {
    let ecc = |f: &mut BusFixture, v: u64| {
        let e = EntityKey::dram_channel(&chip_key(0), 4);
        f.sample(
            &e,
            crate::bus::signals::DRAM_ECC_CORR_RD,
            Value::U64(v),
            Provenance::Measured,
        );
    };
    let mut f = BusFixture::new();
    ecc(&mut f, 5);
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    // First sight of a count: no flash.
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    assert!(o.chips[0].gddr[0].flash_ns.is_none());
    // The same count: no flash.
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    assert!(o.chips[0].gddr[0].flash_ns.is_none());
    ecc(&mut f, 7);
    let t1 = t0 + SEC_NS;
    let o = Overlay::from_world(&f.view(t1), &two_chips(), t1, &mut st);
    assert_eq!(o.chips[0].gddr[0].flash_ns, Some(t1));
    assert!(o.fx_at(t1, false)[&0].gddr[4].unwrap().flash > 0.9);
    assert_eq!(
        o.fx_at(t1 + FLASH_NS, false)[&0].gddr[4].unwrap().flash,
        0.0
    );
    // An `EccTick` event flashes too.
    f.event(Event::EccTick {
        chip: chip_key(0),
        channel: 6,
    });
    let t2 = t1 + 2 * SEC_NS;
    let o = Overlay::from_world(&f.view(t2), &two_chips(), t2, &mut st);
    let row = o.chips[0].gddr.iter().find(|r| r.channel == 6).unwrap();
    assert_eq!(row.flash_ns, Some(t2));
}

#[test]
fn sparks_land_on_the_named_chip_by_the_first_word_of_their_kind() {
    let mut f = BusFixture::new();
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    f.event(Event::Spark {
        chip: Some(chip_key(1)),
        kind: "vLLM.log".into(),
    });
    f.event(Event::Spark {
        chip: None,
        kind: "ttnn.log".into(),
    });
    f.event(Event::KernelCompiled {
        chip: Some(chip_key(0)),
        source: "metal".into(),
    });
    // A chip this view does not draw.
    f.event(Event::Spark {
        chip: Some(chip_key(7)),
        kind: "vLLM.log".into(),
    });
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    let sparks = |i: usize| {
        o.chips
            .iter()
            .find(|c| c.index == i)
            .map(|c| c.sparks.iter().map(|s| s.rgb).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    assert_eq!(sparks(0), vec![LOAD_PURPLE]);
    assert_eq!(sparks(1), vec![SERVE_TEAL]);
    assert_eq!(o.chips.len(), 2);
    // Gone after their life.
    let t1 = t0 + SPARK_NS;
    assert!(Overlay::from_world(&f.view(t1), &two_chips(), t1, &mut st).is_empty());
    assert_eq!(spark_rgb("driver.reset"), TRAIN_AMBER);
}

#[test]
fn the_inset_gets_the_fill_and_the_pulses_only() {
    let mut f = BusFixture::new();
    f.loading("m", &[0], Some(0.5)).gddr_temp(0, 0, 80.0);
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    f.event(Event::Spark {
        chip: Some(chip_key(0)),
        kind: "vLLM.log".into(),
    });
    let o = Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    let full = &o.fx_at(t0, false)[&0];
    let inset = &o.fx_at(t0, true)[&0];
    assert!(full.gddr[0].is_some() && !full.sparks.is_empty());
    assert!(inset.fill.is_some());
    assert!(inset.gddr.iter().all(Option::is_none) && inset.sparks.is_empty());
}

/// Final fix wave S3: events that piled up while the view was not updated
/// (off screen for 10 s) make no pulses or sparks on the first update back.
/// A fresh event after that does.
#[test]
fn coming_back_after_ten_seconds_away_replays_nothing() {
    let mut f = BusFixture::new();
    f.ready("m", "M", &[0, 1], 30.0);
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    for _ in 0..40 {
        f.event(Event::RequestsCompleted {
            model: EntityKey::model("m"),
            n: 2,
        });
    }
    f.event(Event::Spark {
        chip: Some(chip_key(0)),
        kind: "vllm".into(),
    });
    let back = t0 + 10 * SEC_NS;
    let o = Overlay::from_world(&f.view(back), &two_chips(), back, &mut st);
    assert!(
        o.chips
            .iter()
            .all(|c| c.pulses.is_empty() && c.sparks.is_empty()),
        "{o:?}"
    );
    assert_eq!(st.seen_seq, Some(f.world.seq()));
    // One new event, one frame later: it pulses.
    f.event(Event::RequestsCompleted {
        model: EntityKey::model("m"),
        n: 2,
    });
    let next = back + SEC_NS / 30;
    let o = Overlay::from_world(&f.view(next), &two_chips(), next, &mut st);
    assert!(o.chips.iter().all(|c| c.pulses.len() == 1), "{o:?}");
}

/// With a slow data tick (`--interval 2000`) every update is 2 s after the
/// last one. That is the usual spacing, so each update's events still
/// pulse.
#[test]
fn a_slow_data_tick_still_pulses_every_update() {
    let mut f = BusFixture::new();
    f.ready("m", "M", &[0], 30.0);
    let mut st = OverlayState::default();
    let mut t = 5 * SEC_NS;
    Overlay::from_world(&f.view(t), &two_chips(), t, &mut st);
    // The first 2 s gap has no usual spacing to compare with yet: it is
    // longer than a pulse, so it counts as time away.
    t += 2 * SEC_NS;
    Overlay::from_world(&f.view(t), &two_chips(), t, &mut st);
    for _ in 0..5 {
        f.event(Event::RequestsCompleted {
            model: EntityKey::model("m"),
            n: 1,
        });
        t += 2 * SEC_NS;
        let o = Overlay::from_world(&f.view(t), &two_chips(), t, &mut st);
        assert_eq!(o.chips[0].pulses.len(), 1, "at {t}");
    }
}

#[test]
fn a_world_that_went_back_is_a_first_sight() {
    // A restarted host starts its sequence numbers again: its events are
    // history. They make no burst of pulses.
    let mut f = BusFixture::new();
    f.ready("m", "M", &[0], 1.0);
    let mut st = OverlayState::default();
    st.seen_seq = Some(10_000);
    f.event(Event::RequestsCompleted {
        model: EntityKey::model("m"),
        n: 3,
    });
    let now = 5 * SEC_NS;
    let o = Overlay::from_world(&f.view(now), &two_chips(), now, &mut st);
    assert!(o.chips[0].pulses.is_empty());
    assert_eq!(st.seen_seq, Some(f.world.seq()));
}

#[test]
fn an_estimated_progress_or_waiting_figure_marks_the_flag() {
    // Every reading that puts text in the flag counts.
    for (signal, value, phase_event) in [
        (
            tt_toplike_bus::signals::MODEL_PROGRESS,
            Value::F64(0.4),
            false,
        ),
        (tt_toplike_bus::signals::MODEL_WAITING, Value::U64(3), true),
        (tt_toplike_bus::signals::MODEL_RUNNING, Value::U64(1), true),
    ] {
        let mut f = BusFixture::new();
        let m = EntityKey::model("m");
        if phase_event {
            f.ready("m", "M", &[0], 10.0);
        } else {
            f.loading("m", &[0], None);
        }
        f.sample(&m, signal, value, Provenance::Estimated);
        let o = once(&f, 5 * SEC_NS);
        let flag = o.flag(0).unwrap();
        // Running is not in the text, so it does not mark the flag.
        let want = signal != tt_toplike_bus::signals::MODEL_RUNNING;
        assert_eq!(flag.estimated, want, "{signal}: {:?}", flag.texts());
        assert_eq!(flag.texts()[0].starts_with('~'), want, "{signal}");
    }
}

#[test]
fn an_ecc_counter_reset_rebaselines_without_a_flash() {
    let ecc = |f: &mut BusFixture, v: u64| {
        let e = EntityKey::dram_channel(&chip_key(0), 1);
        f.sample(
            &e,
            crate::bus::signals::DRAM_ECC_CORR_RD,
            Value::U64(v),
            Provenance::Measured,
        );
    };
    let flash = |o: &Overlay| o.chips[0].gddr[0].flash_ns;
    let mut f = BusFixture::new();
    let mut st = OverlayState::default();
    let t0 = 5 * SEC_NS;
    ecc(&mut f, 10);
    Overlay::from_world(&f.view(t0), &two_chips(), t0, &mut st);
    // A chip reset clears the counter: a lower count, no flash.
    ecc(&mut f, 3);
    let t1 = t0 + SEC_NS;
    assert_eq!(
        flash(&Overlay::from_world(&f.view(t1), &two_chips(), t1, &mut st)),
        None
    );
    // The lower count is the new baseline: the next rise flashes, though
    // it is still under the old count.
    ecc(&mut f, 4);
    let t2 = t1 + SEC_NS;
    assert_eq!(
        flash(&Overlay::from_world(&f.view(t2), &two_chips(), t2, &mut st)),
        Some(t2)
    );
}
