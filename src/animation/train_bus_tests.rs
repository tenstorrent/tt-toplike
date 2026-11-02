// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Training view frames with and without the signal bus.
//!
//! The golden hashes pin the frames the Training view drew before it read the
//! bus. They were taken by running `golden_hashes` against the commit before
//! the bus port (the commit that added this file holds only these tests), so
//! a frame drawn from an empty world must hash to exactly these values.

use super::*;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use crate::workload::train::{LogSource, StepSample, TrainEvent, TrainProcess, TrainState};
use ratatui::style::Color;

/// A backend whose readings never change: two p300 chips at fixed power,
/// current, temperature and clock. The mock backend adds random noise, so its
/// frames cannot be pinned. (The arcade's bus tests use the same backend.)
pub(super) struct Fixed {
    devices: Vec<Device>,
    telem: Vec<Telemetry>,
    smbus: SmbusTelemetry,
}

impl Fixed {
    /// `n` p300 chips with PCI addresses `0000:01:00.0`, `0000:02:00.0`, ...
    pub(super) fn new(n: usize) -> Fixed {
        let devices: Vec<Device> = (0..n)
            .map(|i| {
                Device::new(
                    i,
                    "p300".into(),
                    format!("0000:{:02x}:00.0", i + 1),
                    format!("(0,{i})"),
                )
            })
            .collect();
        let telem = (0..n)
            .map(|i| {
                let mut t = Telemetry::new();
                t.power = Some(45.0 + 9.0 * i as f32);
                t.current = Some(40.0 + 5.0 * i as f32);
                t.asic_temperature = Some(52.0 + 4.0 * i as f32);
                t.aiclk = Some(1000);
                t.voltage = Some(0.85);
                t.heartbeat = Some(7);
                t
            })
            .collect();
        Fixed {
            devices,
            telem,
            smbus: SmbusTelemetry::new(),
        }
    }
}

impl TelemetryBackend for Fixed {
    fn init(&mut self) -> crate::error::BackendResult<()> {
        Ok(())
    }
    fn update(&mut self) -> crate::error::BackendResult<()> {
        Ok(())
    }
    fn devices(&self) -> &[Device] {
        &self.devices
    }
    fn telemetry(&self, i: usize) -> Option<&Telemetry> {
        self.telem.get(i)
    }
    fn smbus_telemetry(&self, _i: usize) -> Option<&SmbusTelemetry> {
        Some(&self.smbus)
    }
    fn backend_info(&self) -> String {
        "fixed".into()
    }
}

/// FNV-1a, 64 bit: the same number on every run and machine.
struct Fnv(u64);

impl Fnv {
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.0 ^= x as u64;
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    fn color(&mut self, c: Option<Color>) {
        match c {
            None => self.bytes(&[0]),
            Some(Color::Reset) => self.bytes(&[1]),
            Some(Color::Rgb(r, g, b)) => self.bytes(&[2, r, g, b]),
            Some(other) => self.bytes(format!("{other:?}").as_bytes()),
        }
    }
}

/// A hash of rendered lines, cell by cell: each character with its colours
/// and modifiers, and a marker at each line end. How cells are grouped into
/// spans does not enter it.
pub(super) fn frame_hash(lines: &[Line<'static>]) -> u64 {
    let mut h = Fnv(0xcbf2_9ce4_8422_2325);
    for line in lines {
        for span in &line.spans {
            let style = line.style.patch(span.style);
            for ch in span.content.chars() {
                h.bytes(&(ch as u32).to_le_bytes());
                h.color(style.fg);
                h.color(style.bg);
                h.bytes(&style.add_modifier.bits().to_le_bytes());
                h.bytes(&style.sub_modifier.bits().to_le_bytes());
            }
        }
        h.bytes(&[0xff, 0xfe]);
    }
    h.0
}

/// Run `f` with truecolor forced, so colours compare as RGB, and with the
/// default theme set under the theme lock, so a test that flips the
/// app-wide theme cannot change the colours mid-test.
pub(super) fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

/// The trainer process the pinned runs use.
fn proc_of(pid: i32) -> TrainProcess {
    TrainProcess {
        pid,
        binary: "nano_gpt".into(),
        config_path: None,
        start_time: Some(99),
    }
}

/// The states the golden hashes pin: no run (scanning), a run with a log, and
/// a run whose stdout is not redirected. None of them has `first_seen`, whose
/// elapsed time would change the frame from run to run.
pub(super) fn pinned_states() -> Vec<TrainState> {
    let scanning = TrainState::new();

    let mut live = TrainState::new();
    live.proc = Some(proc_of(4242));
    live.log = Some(LogSource::File("/tmp/train.log".into()));
    live.apply_event(TrainEvent::MaxSteps(500));
    live.apply_event(TrainEvent::BatchSize(32));
    for i in 1..=60u64 {
        live.apply_event(TrainEvent::Step {
            step: i,
            loss: 4.0 - (i as f32) * 0.04 + ((i % 5) as f32) * 0.03,
        });
    }
    live.step_history = (1..=40u64)
        .map(|i| StepSample {
            step: 20 + i,
            seq: i,
            ms: 300.0 + ((i * 37) % 90) as f32,
            cache_delta: u32::from(i == 3),
            checkpoint: i == 30,
        })
        .collect();
    live.step_seq = 40;
    live.step_ms = 320.0;
    live.step_time_source = crate::workload::train::StepTimeSource::Reported;
    live.cache_entries = 21;
    live.host_cpu_pct = Some(112.0);
    live.host_rss_bytes = Some(3 << 30);

    let mut blind = TrainState::new();
    blind.proc = Some(proc_of(77));
    blind.log = Some(LogSource::NotRedirected);
    blind.host_cpu_pct = Some(40.0);

    vec![scanning, live, blind]
}

/// The sizes the golden hashes pin: a small terminal and a large one.
pub(super) const SIZES: [(usize, usize); 2] = [(60, 18), (160, 50)];
/// The updates after which a frame is hashed.
pub(super) const HASHED_AT: [u32; 3] = [1, 10, 30];

/// Hashes of the frames of a Training view at `w` x `h` over [`Fixed`] for
/// `st`, after each update in [`HASHED_AT`]. `before_update` runs before
/// every update (the bus tests hand the view a world there).
pub(super) fn hashes_with(
    w: usize,
    h: usize,
    st: &TrainState,
    mut before_update: impl FnMut(&mut TrainView, &Fixed, u32),
) -> Vec<u64> {
    tc(|| {
        let backend = Fixed::new(2);
        let mut v = TrainView::new(w, h);
        let mut out = Vec::new();
        for i in 1..=*HASHED_AT.last().unwrap() {
            before_update(&mut v, &backend, i);
            v.update();
            if HASHED_AT.contains(&i) {
                out.push(frame_hash(&v.render(st, &backend)));
            }
        }
        out
    })
}

/// The frames as the Training view drew them before the bus port: for each
/// of [`pinned_states`], at each of [`SIZES`]. Taken from the commit that
/// added this file.
const GOLDEN: [[[u64; 3]; 2]; 3] = [
    [
        [
            3157963607483462246,
            12308280724662930480,
            9115938886791446319,
        ],
        [
            4554886910839386837,
            13271476333669106344,
            2843338526006443111,
        ],
    ],
    [
        [
            7792015726040430907,
            16934302597298010239,
            11542335149295043401,
        ],
        [
            1464901081232295801,
            4051027690435515937,
            1086426012591978747,
        ],
    ],
    [
        [42671386742250059, 42671386742250059, 42671386742250059],
        [
            8855002232082236177,
            8293645879742830987,
            15879053204002556156,
        ],
    ],
];

/// Prints the hashes for [`GOLDEN`]. Run with
/// `cargo test --locked --lib --features tui train_golden_hashes -- --ignored --nocapture`.
#[test]
#[ignore = "prints the hashes to pin"]
fn train_golden_hashes() {
    for st in pinned_states() {
        let row: Vec<Vec<u64>> = SIZES
            .iter()
            .map(|&(w, h)| hashes_with(w, h, &st, |_, _, _| {}))
            .collect();
        println!("{row:?},");
    }
}

#[test]
fn the_training_frames_with_no_bus_are_the_pinned_frames() {
    for (s, st) in pinned_states().iter().enumerate() {
        for (i, (w, h)) in SIZES.into_iter().enumerate() {
            assert_eq!(
                hashes_with(w, h, st, |_, _, _| {}),
                GOLDEN[s][i].to_vec(),
                "state {s} at {w}x{h}"
            );
        }
    }
}

// ---- The bus ---------------------------------------------------------------

use crate::animation::train_bus::{
    proc_of_key, PulseKind, TrainBusState, TrainFx, LOSS_PULSE_FRAC, MAX_PULSE_COLS, PACE_MAX,
    PACE_MIN, PACE_REF_MS,
};
use tt_toplike_bus::{
    EntityKey, EntityKind, Event, HeaderKind, LocalView, Payload, Provenance, Record, Sample,
    Value, World, WorldRead,
};

const SEC: u64 = 1_000_000_000;
/// The gap between two view updates at 60 frames a second.
const TICK: u64 = 16_000_000;
/// The view size the bus tests draw at.
const W: usize = 134;
const H: usize = 40;

/// The chip key of device `i` of a [`Fixed`]: its PCI address.
fn chip(i: usize) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}

/// The run key the tests use.
fn run_key() -> EntityKey {
    EntityKey::training_run("nano_gpt:4242@99")
}

/// A bus world built record by record, one millisecond apart from 1 s on
/// the bus clock.
#[derive(Clone)]
struct Bus {
    world: World,
    seq: u64,
    t: u64,
}

impl Bus {
    fn new() -> Bus {
        Bus {
            world: World::new(),
            seq: 0,
            t: SEC,
        }
    }
    fn push(&mut self, payload: Payload) -> &mut Self {
        self.seq += 1;
        self.t += 1_000_000;
        self.world.apply(&Record {
            seq: self.seq,
            t_mono_ns: self.t,
            t_wall_ms: 1_790_000_000_000 + self.seq,
            source: "test".into(),
            payload,
        });
        self
    }
    fn sample(&mut self, e: &EntityKey, signal: &str, v: Value, prov: Provenance) -> &mut Self {
        self.push(Payload::Sample(Sample {
            entity: e.clone(),
            signal: signal.into(),
            value: v,
            prov,
        }))
    }
    /// One feed of the run, as `TrainingSource` sends it: step, budget,
    /// loss, step time and host cost, all with `prov`.
    fn run(&mut self, step: u64, loss: f64, ms: f64, prov: Provenance) -> &mut Self {
        use crate::bus::signals as s;
        let r = run_key();
        self.sample(&r, s::TRAIN_STEP, Value::U64(step), prov)
            .sample(&r, s::TRAIN_MAX_STEPS, Value::U64(100), prov)
            .sample(&r, s::TRAIN_LOSS, Value::F64(loss), prov)
            .sample(&r, s::TRAIN_STEP_MS, Value::F64(ms), prov)
            .sample(&r, s::TRAIN_CPU_PCT, Value::F64(150.0), prov)
            .sample(&r, s::TRAIN_RSS_BYTES, Value::U64(2 << 30), prov)
    }
    fn chips(&mut self, text: &str) -> &mut Self {
        self.sample(
            &run_key(),
            crate::bus::signals::TRAIN_CHIPS,
            Value::Text(text.into()),
            Provenance::Measured,
        )
    }
    fn view(&self, now: u64) -> LocalView {
        LocalView {
            seq: self.world.seq(),
            world: self.world.clone(),
            now_mono_ns: now,
            header_kind: HeaderKind::Live,
        }
    }
}

/// The chips of a [`Fixed`] of `n` devices, as `set_bus` builds them.
fn chips_of(n: usize) -> Vec<(usize, EntityKey)> {
    (0..n).map(|i| (i, chip(i))).collect()
}

fn rows_text(lines: &[Line<'static>]) -> Vec<String> {
    lines
        .iter()
        .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
        .collect()
}

/// Each row as (character, foreground) cells.
fn cells_fg(lines: &[Line<'static>]) -> Vec<Vec<(char, Option<Color>)>> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, s.style.fg)))
                .collect()
        })
        .collect()
}

#[test]
fn an_empty_world_draws_the_pinned_frames() {
    // An empty world, and one holding only readings the view does not draw
    // (a chip temperature, and a model on no chip of a run), give exactly
    // the frames the view drew before the bus.
    let mut quiet = Bus::new();
    quiet.sample(
        &chip(0),
        crate::bus::signals::ASIC_TEMPERATURE_C,
        Value::F64(51.0),
        Provenance::Measured,
    );
    quiet.push(Payload::Event(Event::ModelReady {
        model: EntityKey::model("m"),
        chips: vec![chip(0)],
    }));
    for world in [Bus::new(), quiet] {
        for (s, st) in pinned_states().iter().enumerate() {
            for (i, (w, h)) in SIZES.into_iter().enumerate() {
                let got = hashes_with(w, h, st, |v, b, n| {
                    let view = world.view(5 * SEC + n as u64 * TICK);
                    v.set_bus(Some(&view), b);
                });
                assert_eq!(got, GOLDEN[s][i].to_vec(), "state {s} at {w}x{h}");
            }
        }
    }
}

#[test]
fn a_run_key_names_its_trainer() {
    let p = proc_of_key(&run_key());
    assert_eq!(
        (p.binary.as_str(), p.pid, p.start_time),
        ("nano_gpt", 4242, Some(99))
    );
    let p = proc_of_key(&EntityKey::training_run("local"));
    assert_eq!((p.binary.as_str(), p.pid), ("training", 0));
    let p = proc_of_key(&EntityKey::training_run("a:b:12@3"));
    assert_eq!((p.binary.as_str(), p.pid), ("a:b", 12));
}

/// A view fed `steps` reads of a run (one step per read, the loss falling
/// slowly), one update apart, with readings marked `prov`.
fn view_fed(steps: u64, prov: Provenance) -> (TrainView, Fixed, Bus, u64) {
    let b = Fixed::new(2);
    let mut v = TrainView::new(W, H);
    let mut bus = Bus::new();
    let mut now = 5 * SEC;
    for i in 1..=steps {
        bus.run(
            i,
            3.0 - i as f64 * 0.001,
            300.0 + (i % 7) as f64 * 10.0,
            prov,
        );
        now += TICK;
        v.set_bus(Some(&bus.view(now)), &b);
        v.update();
    }
    (v, b, bus, now)
}

#[test]
fn a_run_seen_on_the_bus_is_drawn_with_the_same_look() {
    tc(|| {
        let (v, b, _, _) = view_fed(30, Provenance::Measured);
        assert!(v.shows_bus_run());
        // No local trainer: the bus run is drawn.
        let rows = rows_text(&v.render(&TrainState::new(), &b));
        let all = rows.join("\n");
        assert!(
            rows[0].contains("TRAINING · bus ]  nano_gpt · pid 4242"),
            "{}",
            rows[0]
        );
        assert!(!rows[0].contains("~bus"), "{}", rows[0]);
        assert!(rows[0].contains("LOSS 2.9700"), "{}", rows[0]);
        assert!(rows[1].contains("seen on the signal bus"), "{}", rows[1]);
        assert!(rows[1].contains("step 30 / 100"), "{}", rows[1]);
        assert!(!all.contains("SCANNING"), "{all}");
        // The step band and the river come from the view's own history of
        // reads: thirty step samples and thirty losses.
        assert!(all.contains("STEP ANATOMY  last 30 steps"), "{all}");
        assert!(all.contains("LOSS  · mountains"), "{all}");
        assert!(all.contains("cpu     150%"), "{all}");
        let st = v.bus_state.world_state().unwrap();
        assert_eq!(st.loss_history.len(), 30);
        assert_eq!(st.step_history.len(), 30);
        assert_eq!(st.prev_loss, Some(2.971));
        // No stdout notice: the bus run is a stream.
        assert!(!all.contains("not redirected"), "{all}");
    });
}

#[test]
fn an_estimated_run_says_so_in_the_title() {
    tc(|| {
        let (v, b, _, _) = view_fed(3, Provenance::Estimated);
        let rows = rows_text(&v.render(&TrainState::new(), &b));
        assert!(rows[0].contains("TRAINING · ~bus ]"), "{}", rows[0]);
    });
}

#[test]
fn a_local_trainer_wins_over_the_bus_run() {
    tc(|| {
        let (v, b, _, _) = view_fed(5, Provenance::Measured);
        let local = &pinned_states()[1];
        let rows = rows_text(&v.render(local, &b));
        assert!(
            rows[0].contains("═[ TRAINING ]  nano_gpt · pid 4242"),
            "{}",
            rows[0]
        );
        assert!(rows[1].contains("/tmp/train.log"), "{}", rows[1]);
    });
}

#[test]
fn a_stale_run_is_not_shown() {
    let (mut v, b, bus, now) = view_fed(3, Provenance::Measured);
    assert!(v.shows_bus_run());
    let later = now + 31 * SEC;
    v.set_bus(Some(&bus.view(later)), &b);
    assert!(!v.shows_bus_run());
    assert!(v.bus_fx.is_empty());
}

/// Update `prev` from `bus` at `now` for two chips.
fn fx(bus: &Bus, now: u64, prev: &mut TrainBusState) -> TrainFx {
    TrainFx::from_world(&bus.view(now), &chips_of(2), now, prev)
}

#[test]
fn a_step_landing_pulses_and_history_does_not() {
    let mut bus = Bus::new();
    let mut prev = TrainBusState::default();
    // Steps already in the world at the first read: history.
    bus.run(10, 2.0, 300.0, Provenance::Measured);
    let mut now = 5 * SEC;
    assert!(fx(&bus, now, &mut prev).pulses.is_empty());
    // A read with the same step: nothing landed.
    bus.run(10, 2.0, 300.0, Provenance::Measured);
    now += TICK;
    assert!(fx(&bus, now, &mut prev).pulses.is_empty());
    // The step goes up: one pulse, a plain one (the loss did not move).
    bus.run(11, 2.0, 300.0, Provenance::Measured);
    now += TICK;
    let f = fx(&bus, now, &mut prev);
    assert_eq!(f.pulses.len(), 1);
    assert_eq!(f.pulses[0].kind, PulseKind::Step);
    assert_eq!(f.pulses[0].start_ns, now);
    assert_eq!(f.pulses[0].cols(), 1);
    // It fades within PULSE_NS.
    now += 2 * SEC;
    let _ = fx(&bus, now - SEC, &mut prev);
    assert!(fx(&bus, now, &mut prev).pulses.is_empty());
}

#[test]
fn a_step_that_landed_while_the_view_was_away_makes_no_pulse() {
    let mut bus = Bus::new();
    let mut prev = TrainBusState::default();
    let mut now = 5 * SEC;
    for i in 1..=5 {
        bus.run(i, 2.0, 300.0, Provenance::Measured);
        now += TICK;
        let _ = fx(&bus, now, &mut prev);
    }
    // Five seconds away, during which the run moved on.
    bus.run(9, 1.0, 300.0, Provenance::Measured);
    now += 5 * SEC;
    let f = fx(&bus, now, &mut prev);
    assert!(f.pulses.is_empty(), "{:?}", f.pulses);
    // The read still joins the history.
    let st = prev.world_state().unwrap();
    assert_eq!(st.step, 9);
    assert_eq!(st.loss_history.len(), 6);
    // Back at the usual pace, the next step pulses again.
    bus.run(10, 1.0, 300.0, Provenance::Measured);
    now += TICK;
    assert_eq!(fx(&bus, now, &mut prev).pulses.len(), 1);
}

#[test]
fn a_loss_fall_pulses_bright_and_wide_and_a_rise_dim() {
    let mut bus = Bus::new();
    let mut prev = TrainBusState::default();
    let mut now = 5 * SEC;
    // The first read is a first sight: it sets the loss to compare with.
    bus.run(1, 2.0, 300.0, Provenance::Measured);
    assert!(fx(&bus, now, &mut prev).pulses.is_empty());
    let mut land = |bus: &mut Bus, step: u64, loss: f64| {
        bus.run(step, loss, 300.0, Provenance::Measured);
        now += TICK;
        let f = fx(bus, now, &mut prev);
        *f.pulses.last().unwrap()
    };
    // A 2% fall: a fall pulse three columns wide (1 + 2).
    let p = land(&mut bus, 2, 1.96);
    assert_eq!(p.kind, PulseKind::LossFall);
    assert!((p.size - 0.02).abs() < 1e-4, "{}", p.size);
    assert_eq!(p.cols(), 3);
    // A 3% rise: a rise pulse four columns wide.
    let p = land(&mut bus, 3, 1.96 * 1.03);
    assert_eq!(p.kind, PulseKind::LossRise);
    assert_eq!(p.cols(), 4);
    // Under the threshold: a plain step.
    let p = land(
        &mut bus,
        4,
        1.96 * 1.03 * (1.0 - LOSS_PULSE_FRAC as f64 / 2.0),
    );
    assert_eq!(p.kind, PulseKind::Step);
    // A large fall is capped.
    let p = land(&mut bus, 5, 0.5);
    assert_eq!(p.kind, PulseKind::LossFall);
    assert_eq!(p.cols(), MAX_PULSE_COLS);
}

#[test]
fn a_pulse_is_drawn_on_the_newest_step_columns() {
    tc(|| {
        let (mut v, b, mut bus, mut now) = view_fed(30, Provenance::Measured);
        let st = TrainState::new();
        let before = cells_fg(&v.render(&st, &b));
        // A 2% fall lands: three columns at the right of the band.
        let last = v.bus_state.world_state().unwrap().loss.unwrap() as f64;
        bus.run(31, last * 0.98, 300.0, Provenance::Measured);
        now += TICK;
        v.set_bus(Some(&bus.view(now)), &b);
        v.update();
        let after = cells_fg(&v.render(&st, &b));
        let (x0, w) = v.network_bounds();
        let geom = TrainView::band_geom(x0, w);
        let end = geom.x_data + geom.data_w;
        let (top, bottom) = v.pulse_rows.get();
        assert!(bottom > top);
        let mint = Some(Color::Rgb(120, 230, 190));
        for y in top..bottom {
            for x in end - 3..end {
                assert_eq!(after[y][x].1, mint, "row {y} col {x}: {:?}", after[y][x]);
            }
            // The column left of the pulse is untouched by it.
            assert_ne!(after[y][end - 4].1, mint, "row {y}");
        }
        // Outside the pulse rows the bus drew nothing new: the CHIPS row is
        // as it was.
        let chips_row = v.layout().chips_row;
        assert_eq!(before[chips_row], after[chips_row]);
        // The pulse fades: a second later it is gone.
        v.set_bus(Some(&bus.view(now + 2 * SEC)), &b);
        let gone = cells_fg(&v.render(&st, &b));
        for y in top..bottom {
            assert_ne!(gone[y][end - 1].1, mint, "row {y}");
        }
    });
}

#[test]
fn the_step_time_sets_the_sky_pace() {
    let pace = |ms: Option<f32>| {
        TrainFx {
            pace_ms: ms,
            ..TrainFx::default()
        }
        .sky_pace()
    };
    assert_eq!(pace(None), 1.0);
    assert_eq!(pace(Some(PACE_REF_MS)), 1.0);
    assert_eq!(pace(Some(PACE_REF_MS / 2.0)), 2.0);
    assert_eq!(pace(Some(PACE_REF_MS * 2.0)), 0.5);
    assert_eq!(pace(Some(1.0)), PACE_MAX);
    assert_eq!(pace(Some(1e9)), PACE_MIN);
    assert_eq!(pace(Some(0.0)), 1.0);

    // Wired into the view: a 250 ms step makes the sky clock run at twice
    // the frame rate, and a 1 s step at half.
    for (ms, want) in [(250.0, 2.0), (1000.0, 0.5)] {
        let b = Fixed::new(2);
        let mut v = TrainView::new(W, H);
        let mut bus = Bus::new();
        bus.run(1, 2.0, ms, Provenance::Measured);
        v.set_bus(Some(&bus.view(5 * SEC)), &b);
        let _ = v.render(&TrainState::new(), &b);
        let start = v.sky_frame();
        for i in 0..10u64 {
            v.set_bus(Some(&bus.view(5 * SEC + (i + 1) * TICK)), &b);
            v.update();
        }
        let _ = v.render(&TrainState::new(), &b);
        assert_eq!((v.sky_frame() - start) as f64, 10.0 * want, "{ms} ms");
    }
}

#[test]
fn the_runs_chips_are_marked_and_another_machines_are_not() {
    tc(|| {
        let (mut v, b, mut bus, now) = view_fed(3, Provenance::Measured);
        // Chip 0 is ours; 0000:ee:00.0 is not a chip this TUI has.
        bus.chips("chip:0000:01:00.0,chip:0000:ee:00.0");
        v.set_bus(Some(&bus.view(now + TICK)), &b);
        assert_eq!(v.bus_fx.run_devices, vec![0]);
        let rows = rows_text(&v.render(&TrainState::new(), &b));
        let chips_row = v.layout().chips_row;
        let row = &rows[chips_row];
        assert!(row.contains("▸dev0"), "{row}");
        assert!(!row.contains("▸dev1"), "{row}");
        assert_eq!(row.matches('▸').count(), 1, "{row}");
    });
}

/// Publish a model ready on `chips` with tokens per second.
fn model_ready(bus: &mut Bus, key: &str, chips: Vec<EntityKey>, prov: Provenance) {
    let m = EntityKey::model(key);
    bus.sample(
        &m,
        tt_toplike_bus::signals::MODEL_LABEL,
        Value::Text("Llama 3.1 8B".into()),
        prov,
    )
    .sample(
        &m,
        tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
        Value::F64(42.0),
        prov,
    )
    .push(Payload::Event(Event::ModelReady { model: m, chips }));
}

#[test]
fn a_model_on_the_runs_chips_shows_in_the_same_chips_strip() {
    tc(|| {
        let (mut v, b, mut bus, now) = view_fed(3, Provenance::Measured);
        bus.chips("chip:0000:01:00.0");
        model_ready(&mut bus, "llama", vec![chip(0)], Provenance::Estimated);
        // A model on the other chip is not a neighbour.
        model_ready(&mut bus, "other", vec![chip(1)], Provenance::Measured);
        v.set_bus(Some(&bus.view(now + TICK)), &b);
        assert_eq!(v.bus_fx.neighbours.len(), 1);
        let rows = rows_text(&v.render(&TrainState::new(), &b));
        let row = &rows[v.layout().chips_row + 1];
        assert!(
            row.contains("SAME CHIPS ~Llama 3.1 8B ready 42t/s dev0"),
            "{row}"
        );
    });
}

#[test]
fn neighbours_need_the_runs_chips() {
    // With no chips sample the run's chips are unknown: no marks and no
    // neighbours, though a model is on chip 0.
    let (mut v, b, mut bus, now) = view_fed(3, Provenance::Measured);
    model_ready(&mut bus, "llama", vec![chip(0)], Provenance::Measured);
    v.set_bus(Some(&bus.view(now + TICK)), &b);
    assert!(v.bus_fx.run_devices.is_empty());
    assert!(v.bus_fx.neighbours.is_empty());
}

/// Contract rule 4 through the read path an attached TUI uses: a real
/// `ReadView` limited to this TUI's two chips (`ReadView::only_chips`). The
/// run is drawn, its chip on the host is not marked, and a model on the
/// host's chip is not a neighbour; the same run on chip 1 is marked.
#[test]
fn attached_through_only_chips_a_hosts_chip_draws_nothing() {
    use crate::bus::host::BusHost;
    use crate::bus::view::{BusView, ReadCache};
    use std::collections::BTreeSet;
    use std::sync::Arc;

    tc(|| {
        let b = Fixed::new(2);
        let mut v = TrainView::new(W, H);
        let host = BusHost::new(tt_toplike_bus::BusConfig::default(), &b).unwrap();
        let bus = host.bus().clone();
        let view = BusView::Local(Box::new(host));
        let cache = ReadCache::new();
        let local: Arc<BTreeSet<EntityKey>> = Arc::new([chip(0), chip(1)].into_iter().collect());
        let read = || view.read(&cache).only_chips(local.clone());
        let foreign = EntityKey::chip("0000:ee:00.0");
        let put = |signal: &str, value: Value| {
            bus.publish(
                "test",
                Payload::Sample(Sample {
                    entity: run_key(),
                    signal: signal.into(),
                    value,
                    prov: Provenance::Measured,
                }),
            );
        };
        put(crate::bus::signals::TRAIN_STEP, Value::U64(7));
        put(crate::bus::signals::TRAIN_LOSS, Value::F64(2.5));
        put(
            crate::bus::signals::TRAIN_CHIPS,
            Value::Text(foreign.as_str().into()),
        );
        bus.publish(
            "test",
            Payload::Event(Event::ModelReady {
                model: EntityKey::model("theirs"),
                chips: vec![foreign.clone()],
            }),
        );
        let w = read();
        // The run is not chip-keyed, so the limited view still lists it.
        assert_eq!(w.entities(EntityKind::TrainingRun), vec![run_key()]);
        // A host chip is not listed.
        assert!(!w.entities(EntityKind::Chip).contains(&foreign));
        v.set_bus(Some(&w), &b);
        assert!(v.shows_bus_run());
        assert!(v.bus_fx.run_devices.is_empty());
        assert!(v.bus_fx.neighbours.is_empty());
        let rows = rows_text(&v.render(&TrainState::new(), &b));
        assert!(rows[0].contains("TRAINING · bus"), "{}", rows[0]);
        assert!(!rows.join("\n").contains('▸'));

        // The run moves to chip 1 as well: that one is marked.
        put(
            crate::bus::signals::TRAIN_CHIPS,
            Value::Text(format!("{},{}", foreign.as_str(), chip(1).as_str())),
        );
        v.set_bus(Some(&read()), &b);
        assert_eq!(v.bus_fx.run_devices, vec![1]);
        // The host's model is still on the host's chip only: no neighbour.
        assert!(v.bus_fx.neighbours.is_empty());
    });
}

/// The view's bus read must not be on the frame path: `render` reads only
/// what `set_bus` stored. A world that changes between `set_bus` and
/// `render` changes nothing until the next `set_bus`.
#[test]
fn render_reads_only_what_set_bus_stored() {
    tc(|| {
        let (v, b, mut bus, _) = view_fed(10, Provenance::Measured);
        let st = TrainState::new();
        let a = frame_hash(&v.render(&st, &b));
        bus.run(11, 0.1, 50.0, Provenance::Measured);
        let again = frame_hash(&v.render(&st, &b));
        // Same frame number, same stored effects: only the sky clock could
        // move, and it moves with `update`, so the frames match.
        assert_eq!(a, again);
    });
}

#[test]
fn the_legend_and_explain_text_name_what_the_bus_draws() {
    use crate::animation::train_bus::{EXPLAIN_TEXT, LEGEND_TEXT};
    let flat = |t: &[&str]| t.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
    for (what, text) in [
        ("legend", flat(LEGEND_TEXT)),
        ("explain", flat(EXPLAIN_TEXT)),
    ] {
        for phrase in [
            "┊",
            "loss",
            "1%",
            "sky",
            "decoration",
            "▸",
            "SAME CHIPS",
            "tokens/s",
            "bus",
            "~",
            "estimate",
        ] {
            assert!(text.contains(phrase), "{what} lacks {phrase:?}: {text}");
        }
    }
    // The legend's lines fit the overlay's 52 columns beside its bar.
    for l in LEGEND_TEXT {
        assert!(l.chars().count() <= 52, "{l}");
    }
}

/// Prints a frame of a bus run with a pulse, marked chips and a neighbour,
/// for a person to look at. Run with
/// `cargo test --locked --lib --features tui train_bus_smoke_frame -- --ignored --nocapture`.
#[test]
#[ignore = "prints a frame for a person to look at"]
fn train_bus_smoke_frame() {
    tc(|| {
        let (mut v, b, mut bus, now) = view_fed(40, Provenance::Measured);
        bus.chips("chip:0000:01:00.0,chip:0000:02:00.0");
        model_ready(&mut bus, "llama", vec![chip(1)], Provenance::Measured);
        bus.run(41, 2.5, 320.0, Provenance::Measured);
        v.set_bus(Some(&bus.view(now + TICK)), &b);
        v.update();
        for row in rows_text(&v.render(&TrainState::new(), &b)) {
            println!("{row}");
        }
    });
}
