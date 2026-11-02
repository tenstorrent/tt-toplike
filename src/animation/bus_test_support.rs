// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Test helpers shared by the Memory Castle and Defrag bus tests: a backend
//! whose readings never change, a frame hash that is the same on every run
//! and machine, a bus world built record by record, and the colour lock.
//!
//! [`Clocked`] fixes the bus clock of a real read, for the attached tests.
//!
//! The arcade's bus tests have their own copies of the first two
//! (`arcade_bus_tests.rs`); these are kept separate so neither file depends
//! on the other's private items.

use crate::backend::TelemetryBackend;
use crate::models::telemetry::DeviceLimits;
use crate::models::{Device, SmbusTelemetry, Telemetry};
use ratatui::style::{Color, Style};
use ratatui::text::Line;
use tt_toplike_bus::{
    EntityKey, Event, HeaderKind, LocalView, Payload, Provenance, Record, Sample, Value, World,
    WorldRead,
};

/// One second on the bus clock.
pub const SEC: u64 = 1_000_000_000;
/// The gap between two updates at 30 a second.
pub const TICK: u64 = 33_000_000;

/// A backend whose readings never change. Chip 0 reads an AI clock of 0
/// (Defrag shows it loading); every other chip reads 1000 MHz, and each
/// chip reports a 100 W TDP limit (Defrag shows a chip above 30 W as
/// running). The mock backend adds random noise, so its frames cannot be
/// pinned.
pub struct Fixed {
    devices: Vec<Device>,
    telem: Vec<Telemetry>,
    smbus: SmbusTelemetry,
}

impl Fixed {
    /// `n` p300 chips with PCI addresses `0000:01:00.0`, `0000:02:00.0`, ...
    pub fn new(n: usize) -> Fixed {
        let devices: Vec<Device> = (0..n)
            .map(|i| {
                let mut d = Device::new(
                    i,
                    "p300".into(),
                    format!("0000:{:02x}:00.0", i + 1),
                    format!("(0,{i})"),
                );
                d.limits = Some(DeviceLimits {
                    tdp_limit: Some(100.0),
                    ..DeviceLimits::default()
                });
                d
            })
            .collect();
        let telem = (0..n)
            .map(|i| {
                let mut t = Telemetry::new();
                t.power = Some(45.0 + 9.0 * i as f32);
                t.current = Some(40.0 + 5.0 * i as f32);
                t.asic_temperature = Some(52.0 + 4.0 * i as f32);
                t.aiclk = Some(if i == 0 { 0 } else { 1000 });
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
pub fn frame_hash(lines: &[Line<'static>]) -> u64 {
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
pub fn tc<R>(f: impl FnOnce() -> R) -> R {
    crate::ui::colors::with_theme(crate::ui::colors::Theme::Default, || {
        crate::ui::colors::with_truecolor(true, f)
    })
}

/// Each row as (character, style) cells, one per character.
pub fn cells(lines: &[Line<'static>]) -> Vec<Vec<(char, Style)>> {
    lines
        .iter()
        .map(|l| {
            l.spans
                .iter()
                .flat_map(|s| s.content.chars().map(move |c| (c, l.style.patch(s.style))))
                .collect()
        })
        .collect()
}

/// The text of row `row`.
pub fn row_text(lines: &[Line<'static>], row: usize) -> String {
    lines[row]
        .spans
        .iter()
        .map(|s| s.content.as_ref())
        .collect()
}

/// The foreground `colors::rgb` gives for `c` (truecolor under [`tc`]).
pub fn fg(c: [u8; 3]) -> Option<Color> {
    Some(crate::ui::colors::rgb(c[0], c[1], c[2]))
}

/// The chip key of device `i` of a [`Fixed`]: its PCI address.
pub fn chip(i: usize) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}

/// A bus world built record by record, one millisecond apart from 1 s on
/// the bus clock.
#[derive(Clone)]
pub struct Bus {
    pub world: World,
    seq: u64,
    t: u64,
}

impl Default for Bus {
    fn default() -> Self {
        Bus::new()
    }
}

impl Bus {
    pub fn new() -> Bus {
        Bus {
            world: World::new(),
            seq: 0,
            t: SEC,
        }
    }
    pub fn push(&mut self, payload: Payload) -> &mut Self {
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
    pub fn event(&mut self, e: Event) -> &mut Self {
        self.push(Payload::Event(e))
    }
    pub fn sample(&mut self, e: &EntityKey, signal: &str, v: Value, prov: Provenance) -> &mut Self {
        self.push(Payload::Sample(Sample {
            entity: e.clone(),
            signal: signal.into(),
            value: v,
            prov,
        }))
    }
    /// Model `key` ready on `chips`, with a tokens-per-second reading when
    /// `tps` is given.
    pub fn ready(&mut self, key: &str, chips: &[usize], tps: Option<f64>) -> &mut Self {
        let m = EntityKey::model(key);
        if let Some(t) = tps {
            self.sample(
                &m,
                tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
                Value::F64(t),
                Provenance::Measured,
            );
        }
        self.event(Event::ModelReady {
            model: m,
            chips: chips.iter().map(|&i| chip(i)).collect(),
        })
    }
    pub fn loading(&mut self, key: &str, chips: &[usize], progress: Option<f32>) -> &mut Self {
        self.event(Event::ModelLoading {
            model: EntityKey::model(key),
            chips: chips.iter().map(|&i| chip(i)).collect(),
            progress,
        })
    }
    /// Set model `key`'s phase reading (`"compiling"`, `"down"`, ...).
    pub fn phase(&mut self, key: &str, phase: &str) -> &mut Self {
        self.sample(
            &EntityKey::model(key),
            tt_toplike_bus::signals::MODEL_PHASE,
            Value::Text(phase.into()),
            Provenance::Measured,
        )
    }
    pub fn gone(&mut self, key: &str) -> &mut Self {
        self.event(Event::ModelGone {
            model: EntityKey::model(key),
        })
    }
    pub fn requests(&mut self, key: &str, n: u32) -> &mut Self {
        self.event(Event::RequestsCompleted {
            model: EntityKey::model(key),
            n,
        })
    }
    pub fn spark(&mut self, c: usize, kind: &str) -> &mut Self {
        self.event(Event::Spark {
            chip: Some(chip(c)),
            kind: kind.into(),
        })
    }
    pub fn compiled(&mut self, c: usize) -> &mut Self {
        self.event(Event::KernelCompiled {
            chip: Some(chip(c)),
            source: "ttnn".into(),
        })
    }
    pub fn reset(&mut self, c: usize) -> &mut Self {
        self.event(Event::Reset {
            chips: vec![chip(c)],
        })
    }
    pub fn ecc(&mut self, c: usize, channel: u8) -> &mut Self {
        self.event(Event::EccTick {
            chip: chip(c),
            channel,
        })
    }
    /// The world as a view at bus time `now`.
    pub fn view(&self, now: u64) -> LocalView {
        LocalView {
            seq: self.world.seq(),
            world: self.world.clone(),
            now_mono_ns: now,
            header_kind: HeaderKind::Live,
        }
    }
}

/// A world read through `0` with the bus clock replaced by `1`. The
/// attached tests read a real bus host, whose clock is the machine's: two
/// reads more than a second apart (a loaded machine can stall that long)
/// would count as being away and drop the events under test. A fixed clock
/// a tick apart keeps those tests independent of timing.
pub struct Clocked<'a>(pub &'a dyn WorldRead, pub u64);

impl WorldRead for Clocked<'_> {
    fn seq(&self) -> u64 {
        self.0.seq()
    }
    fn models(&self) -> Vec<tt_toplike_bus::ModelView> {
        self.0.models()
    }
    fn model_on_chip(&self, chip: &EntityKey) -> Option<tt_toplike_bus::ModelView> {
        self.0.model_on_chip(chip)
    }
    fn entities(&self, kind: tt_toplike_bus::EntityKind) -> Vec<EntityKey> {
        self.0.entities(kind)
    }
    fn reading(&self, entity: &EntityKey, signal: &str) -> Option<tt_toplike_bus::Reading> {
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
        self.1
    }
}
