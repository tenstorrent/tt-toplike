//! Sources: things that turn an existing data feed into bus payloads.
//!
//! A [`Source`] emits only what it has. A source that is absent, failed or has
//! nothing new emits nothing; it never emits zeros. A source sets its own rate
//! by returning early from `poll`.
//!
//! [`BackendSource`] reads a [`TelemetryBackend`] through its trait, using the
//! values the backend has already cached. It never calls `update()` (the host
//! does that) and never blocks on I/O.
//!
//! The backend is not owned by the source. The host passes it on every poll
//! in a [`PollCtx`]. Sources that own their input (the monitors) ignore it.
//!
//! Chips are keyed by `EntityKey::chip(&device.bus_id)`. `devices()` and
//! `telemetry(idx)` come from the same backend snapshot, so that key always
//! names the chip the data belongs to. The identity map is for sources that
//! only know an index.

use crate::backend::TelemetryBackend;
use crate::bus::signals as sig;
use crate::models::telemetry::{real_fan_rpm, REGISTER_NO_READING};
use std::collections::{HashMap, HashSet};
use std::time::Instant;
use tt_toplike_bus::{EntityKey, Event, Payload, Provenance, RawFrame, Sample, Sweep, Value};

/// What the host hands every source on each poll.
pub struct PollCtx<'a> {
    /// The backend whose cached values `BackendSource` reads.
    pub backend: &'a dyn TelemetryBackend,
}

/// Something that produces bus payloads.
pub trait Source {
    /// Short stable name, used as the record's source label.
    fn name(&self) -> &'static str;
    /// Emit whatever is new. Return early to run slower than the host's tick.
    /// Sources that own their input ignore `ctx`.
    fn poll(&mut self, ctx: &PollCtx<'_>, now: Instant, out: &mut dyn FnMut(Payload));
    /// Forget what was already sent, so the next poll sends the current
    /// value of everything this source otherwise sends only on change (text
    /// samples, slow-lane values, model announcements).
    ///
    /// The host calls it every few minutes. Log retention deletes whole
    /// segments, and a value sent once at start would be lost with the
    /// segment that held it, leaving a replay of what is left without it.
    /// The default does nothing: a source that sends everything on every
    /// poll has nothing to resend.
    fn resend_all(&mut self) {}
}

/// Reads a backend's cached telemetry and emits chip, DRAM and link records.
pub struct BackendSource {
    /// Last slow-lane value emitted per (entity, signal).
    last: HashMap<(EntityKey, &'static str), Value>,
    /// Last `snapshot_json()` text emitted.
    last_raw: Option<String>,
    /// Devices skipped for having an empty `bus_id`, summed over all polls.
    /// (The identity map only counts these in `refresh`; this source does not
    /// use the map, so it keeps its own count.)
    unkeyed: u64,
    /// Provenance of every sample and sweep this source emits.
    prov: Provenance,
}

impl BackendSource {
    /// A source for a real backend: its readings are `Measured`.
    /// The source's name ([`Source::name`]).
    pub const NAME: &'static str = "backend";

    pub fn new() -> Self {
        Self::with_provenance(Provenance::Measured)
    }

    /// A source whose samples and sweeps carry `prov`. A simulated backend
    /// (`--mock`) uses `Estimated`: its values are made up, and a log or a
    /// view must be able to tell them from readings of real chips.
    pub fn with_provenance(prov: Provenance) -> Self {
        Self {
            last: HashMap::new(),
            last_raw: None,
            unkeyed: 0,
            prov,
        }
    }

    /// Device-polls skipped because the device had no `bus_id`.
    pub fn unkeyed(&self) -> u64 {
        self.unkeyed
    }

    /// Emit a slow-lane sample only if finite and different from the last one.
    fn slow(
        &mut self,
        entity: &EntityKey,
        signal: &'static str,
        value: Value,
        out: &mut dyn FnMut(Payload),
    ) {
        if !value.is_finite() {
            return;
        }
        let k = (entity.clone(), signal);
        if self.last.get(&k) == Some(&value) {
            return;
        }
        self.last.insert(k, value.clone());
        out(Payload::Sample(Sample {
            entity: entity.clone(),
            signal: signal.to_string(),
            value,
            prov: self.prov,
        }));
    }

    /// Read the backend's cached values and emit what is new.
    ///
    /// Per chip: one `Sweep` of fast-lane fields (fields the backend reports
    /// as `None` are left out), then slow-lane samples that changed. Then one
    /// `Raw` record if `snapshot_json()` has new text. A device with an empty
    /// `bus_id` emits nothing and is counted in [`unkeyed`](Self::unkeyed).
    fn poll_backend(&mut self, backend: &dyn TelemetryBackend, out: &mut dyn FnMut(Payload)) {
        let mut seen: HashSet<EntityKey> = HashSet::new();
        for device in backend.devices() {
            if device.bus_id.is_empty() {
                self.unkeyed += 1;
                continue;
            }
            let key = EntityKey::chip(&device.bus_id);
            if !seen.insert(key.clone()) {
                continue;
            }
            let idx = device.index;
            self.fast_lane(backend, idx, &key, out);
            self.slow_lane(backend, idx, &key, out);
        }
        if let Some(body) = backend.snapshot_json() {
            if self.last_raw.as_deref() != Some(body.as_str()) {
                self.last_raw = Some(body.clone());
                out(Payload::Raw(RawFrame {
                    source: sig::RAW_SOURCE_TT_SMI.to_string(),
                    body,
                }));
            }
        }
    }

    fn fast_lane(
        &mut self,
        backend: &dyn TelemetryBackend,
        idx: usize,
        key: &EntityKey,
        out: &mut dyn FnMut(Payload),
    ) {
        let mut signals: Vec<String> = Vec::new();
        let mut values: Vec<Value> = Vec::new();
        let mut put = |name: &str, v: Value| {
            if v.is_finite() {
                signals.push(name.to_string());
                values.push(v);
            }
        };
        if let Some(t) = backend.telemetry(idx) {
            if let Some(v) = t.power {
                put(sig::TDP_W, Value::F64(v as f64));
            }
            if let Some(v) = t.voltage {
                put(sig::VOLTAGE_V, Value::F64(v as f64));
            }
            if let Some(v) = t.current {
                put(sig::CURRENT_A, Value::F64(v as f64));
            }
            if let Some(v) = t.asic_temperature {
                put(sig::ASIC_TEMPERATURE_C, Value::F64(v as f64));
            }
            if let Some(v) = t.aiclk {
                put(sig::AI_CLOCK_MHZ, Value::U64(v as u64));
            }
        }
        if let Some(p) = backend.pcie_bandwidth(idx) {
            put(sig::PCIE_IN_BPS, Value::F64(p.rx_bytes_per_sec));
            put(sig::PCIE_OUT_BPS, Value::F64(p.tx_bytes_per_sec));
        }
        if !signals.is_empty() {
            out(Payload::Sweep(Sweep {
                entity: key.clone(),
                signals,
                values,
                prov: self.prov,
            }));
        }
    }

    fn slow_lane(
        &mut self,
        backend: &dyn TelemetryBackend,
        idx: usize,
        key: &EntityKey,
        out: &mut dyn FnMut(Payload),
    ) {
        if let Some(t) = backend.telemetry(idx) {
            if let Some(v) = t.board_power {
                self.slow(key, sig::BOARD_POWER_W, Value::F64(v as f64), out);
            }
            if let Some(v) = t.heartbeat {
                self.slow(key, sig::HEARTBEAT, Value::U64(v as u64), out);
            }
        }
        let Some(s) = backend.smbus_telemetry(idx) else {
            return;
        };
        if let Some(v) = s.fan_speed.as_deref().and_then(real_fan_rpm) {
            self.slow(key, sig::FAN_SPEED_RPM, Value::U64(v as u64), out);
        }
        let parse = |f: &Option<String>| {
            f.as_deref()
                .and_then(crate::models::telemetry::parse_hex_or_dec)
        };
        if let Some(v) = parse(&s.axiclk) {
            self.slow(key, sig::AXI_CLOCK_MHZ, Value::U64(v as u64), out);
        }
        if let Some(v) = parse(&s.arcclk) {
            self.slow(key, sig::ARC_CLOCK_MHZ, Value::U64(v as u64), out);
        }
        if let Some(v) = parse(&s.therm_trip_count).filter(|v| *v != REGISTER_NO_READING) {
            self.slow(key, sig::THERM_TRIP_COUNT, Value::U64(v as u64), out);
        }
        if let Some(v) = s.harvesting_state {
            self.slow(key, sig::HARVESTING_STATE, Value::U64(v as u64), out);
        }
        if let Some(v) = s.enabled_tensix_col {
            self.slow(key, sig::ENABLED_TENSIX_COL, Value::U64(v as u64), out);
        }
        if let Some(mask) = s.eth_live_status {
            // A change needs a previous value; the first sight only records it.
            let changed = matches!(
                self.last.get(&(key.clone(), sig::ETH_LIVE_MASK)),
                Some(Value::U64(prev)) if *prev != mask
            );
            self.slow(key, sig::ETH_LIVE_MASK, Value::U64(mask), out);
            if changed {
                out(Payload::Event(Event::LinkChanged {
                    chip: key.clone(),
                    mask: (mask & 0xFFFF) as u16,
                }));
            }
        }
        if let Some(g) = &s.gddr_telemetry {
            for c in &g.channels {
                let Ok(ch) = u8::try_from(c.channel) else {
                    continue;
                };
                let e = EntityKey::dram_channel(key, ch);
                let b = |x: bool| Value::U64(x as u64);
                self.slow(&e, sig::DRAM_HARVESTED, b(c.harvested), out);
                self.slow(&e, sig::DRAM_ENABLED, b(c.enabled), out);
                self.slow(&e, sig::DRAM_TRAINING_PASS, b(c.training_pass), out);
                self.slow(&e, sig::DRAM_BIST_PASS, b(c.bist_pass), out);
                if let Some(v) = c.temp_top {
                    self.slow(&e, sig::DRAM_TEMP_TOP_C, Value::F64(v as f64), out);
                }
                if let Some(v) = c.temp_bottom {
                    self.slow(&e, sig::DRAM_TEMP_BOTTOM_C, Value::F64(v as f64), out);
                }
                // All-ones in a 32-bit register means "no reading", not a count.
                let ecc = [
                    (sig::DRAM_ECC_CORR_RD, c.corr_rd),
                    (sig::DRAM_ECC_CORR_WR, c.corr_wr),
                    (sig::DRAM_ECC_UNCORR_RD, c.uncorr_rd),
                    (sig::DRAM_ECC_UNCORR_WR, c.uncorr_wr),
                ];
                for (name, v) in ecc {
                    if let Some(v) = v.filter(|v| *v != REGISTER_NO_READING as u64) {
                        self.slow(&e, name, Value::U64(v), out);
                    }
                }
            }
        }
    }
}

impl Default for BackendSource {
    fn default() -> Self {
        Self::new()
    }
}

impl Source for BackendSource {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn poll(&mut self, ctx: &PollCtx<'_>, _now: Instant, out: &mut dyn FnMut(Payload)) {
        self.poll_backend(ctx.backend, out);
    }
    /// The next poll sends every slow-lane value and the raw frame again.
    /// The Ethernet mask is then a first sight, so it gives no
    /// `LinkChanged`.
    fn resend_all(&mut self) {
        self.last.clear();
        self.last_raw = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::pcie_counters::PcieBandwidth;
    use crate::error::BackendResult;
    use crate::models::device::Device;
    use crate::models::telemetry::{GddrChannel, GddrTelemetry, SmbusTelemetry, Telemetry};

    const A: &str = "0000:01:00.0";
    const B: &str = "0000:02:00.0";

    /// A backend with every value settable. Lookups are by device index, as in
    /// the real backends.
    #[derive(Default)]
    struct Fake {
        devices: Vec<Device>,
        telem: HashMap<usize, Telemetry>,
        smbus: HashMap<usize, SmbusTelemetry>,
        pcie: HashMap<usize, PcieBandwidth>,
        json: Option<String>,
    }
    impl TelemetryBackend for Fake {
        fn init(&mut self) -> BackendResult<()> {
            Ok(())
        }
        fn update(&mut self) -> BackendResult<()> {
            Ok(())
        }
        fn devices(&self) -> &[Device] {
            &self.devices
        }
        fn telemetry(&self, i: usize) -> Option<&Telemetry> {
            self.telem.get(&i)
        }
        fn smbus_telemetry(&self, i: usize) -> Option<&SmbusTelemetry> {
            self.smbus.get(&i)
        }
        fn backend_info(&self) -> String {
            "fake".into()
        }
        fn snapshot_json(&self) -> Option<String> {
            self.json.clone()
        }
        fn pcie_bandwidth(&self, i: usize) -> Option<PcieBandwidth> {
            self.pcie.get(&i).copied()
        }
    }

    fn dev(index: usize, bus: &str) -> Device {
        Device::new(index, "n150".into(), bus.into(), String::new())
    }
    fn full_telem(power: f32) -> Telemetry {
        let mut t = Telemetry::new();
        t.power = Some(power);
        t.voltage = Some(0.85);
        t.current = Some(20.0);
        t.asic_temperature = Some(55.0);
        t.aiclk = Some(1000);
        t
    }
    fn setup(devs: Vec<Device>) -> (Fake, BackendSource) {
        let f = Fake {
            devices: devs,
            ..Default::default()
        };
        (f, BackendSource::new())
    }
    fn poll(s: &mut BackendSource, f: &Fake) -> Vec<Payload> {
        poll_dyn(s, f)
    }
    fn poll_dyn(s: &mut dyn Source, b: &dyn TelemetryBackend) -> Vec<Payload> {
        let mut v = Vec::new();
        s.poll(&PollCtx { backend: b }, Instant::now(), &mut |p| v.push(p));
        v
    }
    fn sweeps(v: &[Payload]) -> Vec<&Sweep> {
        v.iter()
            .filter_map(|p| match p {
                Payload::Sweep(s) => Some(s),
                _ => None,
            })
            .collect()
    }
    fn samples<'a>(v: &'a [Payload], e: &EntityKey, name: &str) -> Vec<&'a Sample> {
        v.iter()
            .filter_map(|p| match p {
                Payload::Sample(s) if &s.entity == e && s.signal == name => Some(s),
                _ => None,
            })
            .collect()
    }
    fn smbus_with(f: impl FnOnce(&mut SmbusTelemetry)) -> SmbusTelemetry {
        let mut s = SmbusTelemetry::new();
        f(&mut s);
        s
    }
    fn chan(n: usize, top: f32, corr: u64) -> GddrChannel {
        GddrChannel {
            channel: n,
            enabled: true,
            training_pass: true,
            bist_pass: true,
            temp_top: Some(top),
            temp_bottom: Some(top - 1.0),
            corr_rd: Some(corr),
            ..Default::default()
        }
    }
    fn gddr(ch: Vec<GddrChannel>) -> SmbusTelemetry {
        smbus_with(|s| {
            s.gddr_telemetry = Some(GddrTelemetry {
                channels: ch,
                ..Default::default()
            })
        })
    }

    #[test]
    fn one_sweep_per_chip_with_expected_signals_measured() {
        let (mut f, mut s) = setup(vec![dev(0, A), dev(1, B)]);
        f.telem.insert(0, full_telem(30.0));
        f.telem.insert(1, full_telem(40.0));
        for i in 0..2 {
            f.pcie.insert(
                i,
                PcieBandwidth {
                    rx_bytes_per_sec: 100.0,
                    tx_bytes_per_sec: 200.0,
                },
            );
        }
        let out = poll(&mut s, &f);
        let sw = sweeps(&out);
        assert_eq!(sw.len(), 2);
        assert_eq!(sw[0].entity, EntityKey::chip(A));
        assert_eq!(sw[1].entity, EntityKey::chip(B));
        for x in &sw {
            assert_eq!(x.prov, Provenance::Measured);
            assert_eq!(
                x.signals,
                [
                    sig::TDP_W,
                    sig::VOLTAGE_V,
                    sig::CURRENT_A,
                    sig::ASIC_TEMPERATURE_C,
                    sig::AI_CLOCK_MHZ,
                    sig::PCIE_IN_BPS,
                    sig::PCIE_OUT_BPS
                ]
            );
        }
        assert_eq!(sw[1].values[0], Value::F64(40.0));
        assert_eq!(sw[0].values[4], Value::U64(1000));
    }

    #[test]
    fn device_with_no_telemetry_emits_nothing() {
        let (mut f, mut s) = setup(vec![dev(0, A), dev(1, B)]);
        f.telem.insert(1, full_telem(40.0));
        let out = poll(&mut s, &f);
        assert_eq!(out.len(), 1);
        assert_eq!(sweeps(&out)[0].entity, EntityKey::chip(B));
    }

    #[test]
    fn none_fields_are_absent_not_zero() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        let mut t = full_telem(30.0);
        t.voltage = None;
        t.aiclk = None;
        f.telem.insert(0, t);
        let out = poll(&mut s, &f);
        let sw = sweeps(&out);
        assert_eq!(
            sw[0].signals,
            [sig::TDP_W, sig::CURRENT_A, sig::ASIC_TEMPERATURE_C]
        );
    }

    #[test]
    fn pcie_fields_absent_when_backend_has_no_counters() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        f.telem.insert(0, full_telem(30.0));
        let out = poll(&mut s, &f);
        let sw = sweeps(&out);
        assert!(!sw[0].signals.iter().any(|n| n.starts_with("toplike.pcie")));
        assert_eq!(sw[0].signals.len(), 5);
    }

    #[test]
    fn non_finite_reading_is_not_emitted() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        let mut t = full_telem(f32::NAN);
        t.board_power = Some(f32::INFINITY);
        f.telem.insert(0, t);
        let out = poll(&mut s, &f);
        assert!(!sweeps(&out)[0].signals.contains(&sig::TDP_W.to_string()));
        assert!(samples(&out, &EntityKey::chip(A), sig::BOARD_POWER_W).is_empty());
    }

    #[test]
    fn unchanged_slow_value_is_not_reemitted_and_changed_one_is() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        let mut t = full_telem(30.0);
        t.board_power = Some(55.0);
        f.telem.insert(0, t.clone());
        let e = EntityKey::chip(A);
        let out = poll(&mut s, &f);
        assert_eq!(samples(&out, &e, sig::BOARD_POWER_W).len(), 1);
        let out = poll(&mut s, &f);
        assert!(samples(&out, &e, sig::BOARD_POWER_W).is_empty());
        t.board_power = Some(56.0);
        f.telem.insert(0, t);
        let out = poll(&mut s, &f);
        let got = samples(&out, &e, sig::BOARD_POWER_W);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].value, Value::F64(56.0));
    }

    #[test]
    fn slow_chip_fields_are_emitted_with_parsed_values() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        f.smbus.insert(
            0,
            smbus_with(|m| {
                m.fan_speed = Some("3000".into());
                m.axiclk = Some("500".into());
                m.arcclk = Some("0x320".into());
                m.therm_trip_count = Some("2".into());
                m.harvesting_state = Some(3);
                m.enabled_tensix_col = Some(0x3FFF);
            }),
        );
        let out = poll(&mut s, &f);
        let e = EntityKey::chip(A);
        let v = |n| samples(&out, &e, n)[0].value.clone();
        assert_eq!(v(sig::FAN_SPEED_RPM), Value::U64(3000));
        assert_eq!(v(sig::AXI_CLOCK_MHZ), Value::U64(500));
        assert_eq!(v(sig::ARC_CLOCK_MHZ), Value::U64(0x320));
        assert_eq!(v(sig::THERM_TRIP_COUNT), Value::U64(2));
        assert_eq!(v(sig::HARVESTING_STATE), Value::U64(3));
        assert_eq!(v(sig::ENABLED_TENSIX_COL), Value::U64(0x3FFF));
    }

    #[test]
    fn fan_and_trip_sentinels_are_not_emitted() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        f.smbus.insert(
            0,
            smbus_with(|m| {
                m.fan_speed = Some("0xFFFF".into());
                m.therm_trip_count = Some("0xFFFFFFFF".into());
            }),
        );
        let out = poll(&mut s, &f);
        assert!(out.is_empty());
    }

    #[test]
    fn snapshot_json_emits_once_per_new_text() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        f.json = Some("{\"a\":1}".into());
        let raws = |o: &[Payload]| o.iter().filter(|p| matches!(p, Payload::Raw(_))).count();
        let out = poll(&mut s, &f);
        assert_eq!(raws(&out), 1);
        match out.last().unwrap() {
            Payload::Raw(r) => {
                assert_eq!(r.source, "tt-smi");
                assert_eq!(r.body, "{\"a\":1}");
            }
            _ => panic!("expected raw"),
        }
        assert_eq!(raws(&poll(&mut s, &f)), 0);
        f.json = Some("{\"a\":2}".into());
        assert_eq!(raws(&poll(&mut s, &f)), 1);
    }

    #[test]
    fn eth_mask_change_emits_exactly_one_link_changed() {
        let (mut f, mut s) = setup(vec![dev(0, A)]);
        let links = |o: &[Payload]| -> Vec<(EntityKey, u16)> {
            o.iter()
                .filter_map(|p| match p {
                    Payload::Event(Event::LinkChanged { chip, mask }) => {
                        Some((chip.clone(), *mask))
                    }
                    _ => None,
                })
                .collect()
        };
        f.smbus
            .insert(0, smbus_with(|m| m.eth_live_status = Some(0xFF)));
        assert!(
            links(&poll(&mut s, &f)).is_empty(),
            "first sight is not a change"
        );
        assert!(links(&poll(&mut s, &f)).is_empty());
        f.smbus
            .insert(0, smbus_with(|m| m.eth_live_status = Some(0x0F)));
        let out = poll(&mut s, &f);
        assert_eq!(links(&out), vec![(EntityKey::chip(A), 0x0F)]);
        assert!(links(&poll(&mut s, &f)).is_empty());
    }

    #[test]
    fn gddr_channels_land_on_the_right_chips_dram_entities() {
        // List order is the reverse of index order, so loop position and
        // device index disagree.
        let (mut f, mut s) = setup(vec![dev(1, B), dev(0, A)]);
        f.smbus
            .insert(0, gddr(vec![chan(0, 40.0, 1), chan(1, 41.0, 2)]));
        f.smbus.insert(1, gddr(vec![chan(0, 60.0, 9)]));
        let out = poll(&mut s, &f);
        let ea0 = EntityKey::dram_channel(&EntityKey::chip(A), 0);
        let ea1 = EntityKey::dram_channel(&EntityKey::chip(A), 1);
        let eb0 = EntityKey::dram_channel(&EntityKey::chip(B), 0);
        assert_eq!(
            samples(&out, &ea0, sig::DRAM_TEMP_TOP_C)[0].value,
            Value::F64(40.0)
        );
        assert_eq!(
            samples(&out, &ea1, sig::DRAM_TEMP_TOP_C)[0].value,
            Value::F64(41.0)
        );
        assert_eq!(
            samples(&out, &eb0, sig::DRAM_TEMP_TOP_C)[0].value,
            Value::F64(60.0)
        );
        assert_eq!(
            samples(&out, &eb0, sig::DRAM_ECC_CORR_RD)[0].value,
            Value::U64(9)
        );
        assert_eq!(
            samples(&out, &ea0, sig::DRAM_ECC_CORR_RD)[0].value,
            Value::U64(1)
        );
        assert!(samples(&out, &eb0, sig::DRAM_ECC_UNCORR_RD).is_empty());
        // Only a changed counter is re-emitted.
        f.smbus
            .insert(0, gddr(vec![chan(0, 40.0, 5), chan(1, 41.0, 2)]));
        let out = poll(&mut s, &f);
        assert_eq!(
            samples(&out, &ea0, sig::DRAM_ECC_CORR_RD)[0].value,
            Value::U64(5)
        );
        assert!(samples(&out, &ea1, sig::DRAM_ECC_CORR_RD).is_empty());
        assert!(samples(&out, &eb0, sig::DRAM_ECC_CORR_RD).is_empty());
    }

    #[test]
    fn swapped_indexes_keep_data_on_the_right_chip() {
        // Indexes are not zero-based or contiguous, telemetry exists only at
        // 3 and 7, and the list order is the reverse of the index order. A
        // lookup by loop position finds nothing or the wrong chip.
        let (mut f, mut s) = setup(vec![dev(7, B), dev(3, A)]);
        let mut ta = full_telem(10.0);
        ta.voltage = Some(0.81);
        ta.current = Some(11.0);
        ta.asic_temperature = Some(41.0);
        ta.aiclk = Some(1001);
        let mut tb = full_telem(20.0);
        tb.voltage = Some(0.92);
        tb.current = Some(22.0);
        tb.asic_temperature = Some(62.0);
        tb.aiclk = Some(2002);
        let pa = PcieBandwidth {
            rx_bytes_per_sec: 111.0,
            tx_bytes_per_sec: 112.0,
        };
        let pb = PcieBandwidth {
            rx_bytes_per_sec: 221.0,
            tx_bytes_per_sec: 222.0,
        };
        // Before: A at index 3, B at index 7.
        f.telem.insert(3, ta.clone());
        f.telem.insert(7, tb.clone());
        f.pcie.insert(3, pa);
        f.pcie.insert(7, pb);
        let expect_a = vec![
            Value::F64(10.0),
            Value::F64(0.81f32 as f64),
            Value::F64(11.0),
            Value::F64(41.0),
            Value::U64(1001),
            Value::F64(111.0),
            Value::F64(112.0),
        ];
        let expect_b = vec![
            Value::F64(20.0),
            Value::F64(0.92f32 as f64),
            Value::F64(22.0),
            Value::F64(62.0),
            Value::U64(2002),
            Value::F64(221.0),
            Value::F64(222.0),
        ];
        let check = |o: &[Payload]| {
            let sw = sweeps(o);
            assert_eq!(sw.len(), 2);
            for w in sw {
                let want = if w.entity == EntityKey::chip(A) {
                    &expect_a
                } else {
                    assert_eq!(w.entity, EntityKey::chip(B));
                    &expect_b
                };
                assert_eq!(&w.values, want, "wrong values under {}", w.entity);
            }
        };
        check(&poll(&mut s, &f));
        // After a reset the chips swap indexes: A is now 7 and B is 3. The
        // backend reports each chip's data under its new index.
        f.devices = vec![dev(3, B), dev(7, A)];
        f.telem.clear();
        f.pcie.clear();
        f.telem.insert(7, ta);
        f.telem.insert(3, tb);
        f.pcie.insert(7, pa);
        f.pcie.insert(3, pb);
        check(&poll(&mut s, &f));
    }

    #[test]
    fn unkeyed_device_emits_nothing_and_is_counted() {
        let (mut f, mut s) = setup(vec![dev(0, ""), dev(1, B)]);
        f.telem.insert(0, full_telem(10.0));
        f.telem.insert(1, full_telem(20.0));
        let out = poll(&mut s, &f);
        assert_eq!(sweeps(&out).len(), 1);
        assert_eq!(sweeps(&out)[0].entity, EntityKey::chip(B));
        assert_eq!(s.unkeyed(), 1);
        poll(&mut s, &f);
        assert_eq!(s.unkeyed(), 2);
    }

    #[test]
    fn mock_backend_polls_through_the_trait() {
        use crate::backend::mock::MockBackend;
        let mut b = MockBackend::new(2);
        b.init().unwrap();
        b.update().unwrap();
        let mut s = BackendSource::new();
        let out = poll_dyn(&mut s, &b);
        assert_eq!(sweeps(&out).len(), 2);
        // Mock has 8 GDDR channels per chip.
        let e = EntityKey::dram_channel(&EntityKey::chip(&b.devices()[1].bus_id), 7);
        assert_eq!(samples(&out, &e, sig::DRAM_ENABLED).len(), 1);
        assert!(out
            .iter()
            .any(|p| matches!(p, Payload::Sample(s) if s.signal == sig::FAN_SPEED_RPM)));
        // Same cached values: the slow lane is silent, the fast lane is not.
        let again = poll_dyn(&mut s, &b);
        assert_eq!(sweeps(&again).len(), 2);
        assert!(again.iter().all(|p| matches!(p, Payload::Sweep(_))));
    }

    #[test]
    fn boxed_source_emits_sweeps_through_the_trait() {
        let (mut f, _) = setup(vec![dev(0, A), dev(1, B)]);
        f.telem.insert(0, full_telem(10.0));
        f.telem.insert(1, full_telem(20.0));
        let mut src: Box<dyn Source> = Box::new(BackendSource::new());
        assert_eq!(src.name(), "backend");
        let out = poll_dyn(src.as_mut(), &f);
        assert_eq!(sweeps(&out).len(), 2);
    }
}
