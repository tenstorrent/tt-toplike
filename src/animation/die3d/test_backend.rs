// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! A test-only `TelemetryBackend` whose power, TDP and PCIe rate are set
//! directly, shared by the Die 3D tests.
//!
//! The mock backend simulates its own load, so a test that needs a chip's load
//! to cross a trigger level at a known tick cannot rely on it. This fake holds
//! plain values that the test changes between ticks.

use crate::backend::pcie_counters::PcieBandwidth;
use crate::backend::TelemetryBackend;
use crate::models::{Device, DeviceProcess, SmbusTelemetry, Telemetry};

pub(super) struct Fake {
    pub(super) devices: Vec<Device>,
    /// One telemetry record per device, indexed by device index.
    pub(super) telem: Vec<Telemetry>,
    /// Shared by every device: the TDP lives here.
    pub(super) smbus: SmbusTelemetry,
    pub(super) has_smbus: bool,
    /// Shared by every device.
    pub(super) pcie: Option<PcieBandwidth>,
    /// Which processes have which devices open.
    pub(super) procs: Vec<DeviceProcess>,
}

impl Fake {
    /// One p300 chip drawing `power` watts, with the TDP in the SMBUS block,
    /// the tt-smi `limits` block, both or neither.
    pub(super) fn new(power: f32, smbus_tdp: Option<&str>, limit_tdp: Option<f32>) -> Fake {
        let mut f = Fake::boards(&["p300"], smbus_tdp);
        f.devices[0].limits = limit_tdp.map(|t| crate::models::telemetry::DeviceLimits {
            tdp_limit: Some(t),
            tdc_limit: None,
            asic_fmax: None,
            thm_limit: None,
        });
        f.set_power(0, power);
        f
    }

    /// One device per board type (for example `"p300"` or `"n300"`), each at
    /// 0 W, all sharing one SMBUS block with the given TDP.
    pub(super) fn boards(board_types: &[&str], smbus_tdp: Option<&str>) -> Fake {
        let devices: Vec<Device> = board_types
            .iter()
            .enumerate()
            .map(|(i, bt)| {
                Device::new(
                    i,
                    bt.to_string(),
                    format!("0000:{:02x}:00.0", i + 1),
                    format!("(0,{i})"),
                )
            })
            .collect();
        let telem = devices
            .iter()
            .map(|_| {
                let mut t = Telemetry::new();
                t.power = Some(0.0);
                t
            })
            .collect();
        let mut smbus = SmbusTelemetry::new();
        smbus.tdp = smbus_tdp.map(str::to_string);
        Fake {
            devices,
            telem,
            smbus,
            has_smbus: true,
            pcie: None,
            procs: Vec::new(),
        }
    }

    /// Process `pid` opens (or, with [`close`](Self::close_device), loses) device `device`.
    pub(super) fn open_device(&mut self, pid: i64, device: usize) {
        self.procs.push(DeviceProcess {
            pid: Some(pid),
            device: Some(device),
            ..DeviceProcess::default()
        });
    }

    /// Process `pid` closes `device`.
    pub(super) fn close_device(&mut self, pid: i64, device: usize) {
        self.procs
            .retain(|p| !(p.pid == Some(pid) && p.device == Some(device)));
    }

    /// Every GDDR channel at `celsius` (all four register pairs, shared by
    /// every device).
    pub(super) fn set_gddr_temp(&mut self, celsius: f32) {
        self.smbus.gddr_temps = [Some(crate::models::telemetry::GddrTempPair([celsius; 4])); 4];
    }

    /// Set device `i`'s power draw in watts.
    pub(super) fn set_power(&mut self, i: usize, watts: f32) {
        self.telem[i].power = Some(watts);
    }
}

impl TelemetryBackend for Fake {
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
        self.has_smbus.then_some(&self.smbus)
    }
    fn backend_info(&self) -> String {
        "fake".into()
    }
    fn pcie_bandwidth(&self, _i: usize) -> Option<PcieBandwidth> {
        self.pcie
    }
    fn device_processes(&self) -> &[DeviceProcess] {
        &self.procs
    }
}

// ---- A bus world for the overlay tests -------------------------------------

use tt_toplike_bus::{
    EntityKey, Event, HeaderKind, LocalView, Payload, Provenance, Record, Sample, Value, World,
};

/// Nanoseconds in a second, for the bus clock.
pub(super) const SEC_NS: u64 = 1_000_000_000;

/// A bus world built record by record, read through a [`LocalView`] at a
/// clock reading the test sets. Every record is a millisecond after the
/// one before, from 1 s on the bus clock.
pub(super) struct BusFixture {
    pub(super) world: World,
    seq: u64,
    t: u64,
    pub(super) kind: HeaderKind,
}

impl BusFixture {
    pub(super) fn new() -> BusFixture {
        BusFixture {
            world: World::new(),
            seq: 0,
            t: SEC_NS,
            kind: HeaderKind::Live,
        }
    }

    fn push(&mut self, payload: Payload) {
        self.seq += 1;
        self.t += 1_000_000;
        self.world.apply(&Record {
            seq: self.seq,
            t_mono_ns: self.t,
            t_wall_ms: 1_790_000_000_000 + self.seq,
            source: "test".into(),
            payload,
        });
    }

    /// Publish an event.
    pub(super) fn event(&mut self, e: Event) -> &mut Self {
        self.push(Payload::Event(e));
        self
    }

    /// Publish a sample.
    pub(super) fn sample(
        &mut self,
        entity: &EntityKey,
        signal: &str,
        value: Value,
        prov: Provenance,
    ) -> &mut Self {
        self.push(Payload::Sample(Sample {
            entity: entity.clone(),
            signal: signal.into(),
            value,
            prov,
        }));
        self
    }

    /// A model `key` labelled `label` announced ready on `chips`, serving
    /// `tps` tokens a second.
    pub(super) fn ready(&mut self, key: &str, label: &str, chips: &[usize], tps: f64) -> &mut Self {
        let m = EntityKey::model(key);
        self.sample(
            &m,
            tt_toplike_bus::signals::MODEL_LABEL,
            Value::Text(label.into()),
            Provenance::Measured,
        );
        self.sample(
            &m,
            tt_toplike_bus::signals::MODEL_TOKENS_PER_S,
            Value::F64(tps),
            Provenance::Measured,
        );
        self.event(Event::ModelReady {
            model: m,
            chips: chips.iter().map(|&i| chip_key(i)).collect(),
        })
    }

    /// A model `key` loading on `chips` at `progress`.
    pub(super) fn loading(
        &mut self,
        key: &str,
        chips: &[usize],
        progress: Option<f32>,
    ) -> &mut Self {
        self.event(Event::ModelLoading {
            model: EntityKey::model(key),
            chips: chips.iter().map(|&i| chip_key(i)).collect(),
            progress,
        })
    }

    /// DRAM channel `ch` of chip `i` at `celsius`, top and bottom.
    pub(super) fn gddr_temp(&mut self, i: usize, ch: u8, celsius: f64) -> &mut Self {
        let e = EntityKey::dram_channel(&chip_key(i), ch);
        for s in [
            crate::bus::signals::DRAM_TEMP_TOP_C,
            crate::bus::signals::DRAM_TEMP_BOTTOM_C,
        ] {
            self.sample(&e, s, Value::F64(celsius), Provenance::Measured);
        }
        self
    }

    /// The world as a view reads it, with the bus clock at `now_ns`.
    pub(super) fn view(&self, now_ns: u64) -> LocalView {
        LocalView {
            seq: self.world.seq(),
            world: self.world.clone(),
            now_mono_ns: now_ns,
            header_kind: self.kind,
        }
    }
}

/// The chip key of device `i` of a [`Fake`]: its PCI address
/// (`0000:0{i+1}:00.0`), as `Fake::boards` sets `bus_id`.
pub(super) fn chip_key(i: usize) -> EntityKey {
    EntityKey::chip(&format!("0000:{:02x}:00.0", i + 1))
}
