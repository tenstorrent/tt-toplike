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
