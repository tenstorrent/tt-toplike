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
use crate::models::{Device, SmbusTelemetry, Telemetry};

pub(super) struct Fake {
    pub(super) devices: Vec<Device>,
    /// One telemetry record per device, indexed by device index.
    pub(super) telem: Vec<Telemetry>,
    /// Shared by every device: the TDP lives here.
    pub(super) smbus: SmbusTelemetry,
    pub(super) has_smbus: bool,
    /// Shared by every device.
    pub(super) pcie: Option<PcieBandwidth>,
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
        }
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
}
