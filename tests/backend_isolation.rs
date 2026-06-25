// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Cross-backend isolation tests.
//!
//! These guard that the macOS / `--host` support added on this branch is
//! *additive* and does not change the behaviour of the Tenstorrent (TT)
//! architectures or the Linux-only backends:
//!
//!   * Device geometry overrides are `None` for every non-host backend, so a TT
//!     device reports exactly its architecture's channel count and Tensix grid.
//!   * Every backend's devices survive every full-screen visualization without
//!     panicking — including the TT-architecture mock devices (real, non-zero
//!     channel counts and grids) and the 0-channel host CPU device.
//!
//! The mock backend stands in for real TT silicon here: it emits Grayskull /
//! Wormhole / Blackhole devices with the genuine architecture geometry, so
//! these tests exercise the TT rendering paths on every platform — including
//! macOS, where the actual sysfs/hybrid/luwen backends can't run. Linux-only
//! dispatch is covered by the platform-gated unit tests in `backend::factory`.

use tt_toplike::animation::{ArcadeVisualization, HardwareStarfield, MemoryCastle, MemoryFlowVis};
use tt_toplike::backend::host::HostBackend;
use tt_toplike::backend::mock::MockBackend;
use tt_toplike::backend::TelemetryBackend;
use tt_toplike::models::Architecture;

/// Run a backend through every full-screen visualization for several frames.
/// A divide-by-zero, slice index, or modulo-by-zero panic in any of them fails
/// the test — this is the core "no backend can crash the renderer" guard.
fn drive_all_visualizations(backend: &dyn TelemetryBackend) {
    let mut castle = MemoryCastle::new(120, 40);
    let mut flow = MemoryFlowVis::new(120, 40);
    let mut arcade = ArcadeVisualization::new(120, 40);
    let mut starfield = HardwareStarfield::new(120, 40);

    // Enough frames for the frame-counter-driven channel/grid indexing to wrap.
    for _ in 0..12 {
        castle.update(backend);
        flow.update(backend);
        arcade.update(backend);
        starfield.update_from_telemetry(backend);
    }
}

/// A mock backend (TT-architecture devices) that's been initialised + sampled.
fn ready_mock(n: usize) -> MockBackend {
    let mut b = MockBackend::new(n);
    b.init().expect("mock init");
    b.update().expect("mock update");
    b
}

/// A host backend that's been initialised + sampled.
fn ready_host() -> HostBackend {
    let mut b = HostBackend::new();
    b.init().expect("host init");
    b.update().expect("host update");
    b
}

#[test]
fn mock_tt_devices_have_no_geometry_overrides() {
    // 6 devices cycle Grayskull / Wormhole / Blackhole twice.
    let b = ready_mock(6);
    assert!(!b.devices().is_empty());

    for d in b.devices() {
        // The override fields are a host-only concept. If a non-host backend
        // ever set them, TT devices would silently mis-report their geometry.
        assert_eq!(d.grid_override, None, "mock dev {} grid_override", d.index);
        assert_eq!(
            d.channels_override, None,
            "mock dev {} channels_override",
            d.index
        );

        // With no override, the Device geometry methods must be transparent
        // pass-throughs to the architecture — i.e. unchanged from before.
        assert_eq!(d.memory_channels(), d.architecture.memory_channels());
        assert_eq!(d.tensix_grid(), d.architecture.tensix_grid());

        // And every TT architecture has a real, non-zero channel count.
        assert!(
            d.memory_channels() >= 1,
            "TT arch {:?} must report >= 1 channel",
            d.architecture
        );
    }
}

#[test]
fn tt_architecture_geometry_is_unchanged() {
    // Pin the canonical TT geometry so a future override refactor can't drift it.
    assert_eq!(Architecture::Grayskull.memory_channels(), 4);
    assert_eq!(Architecture::Wormhole.memory_channels(), 8);
    assert_eq!(Architecture::Blackhole.memory_channels(), 12);
    assert_eq!(Architecture::Grayskull.tensix_grid(), (10, 12));
    assert_eq!(Architecture::Wormhole.tensix_grid(), (8, 10));
    assert_eq!(Architecture::Blackhole.tensix_grid(), (14, 16));
    // Unknown stays empty — that's *why* the host backend needs an override.
    assert_eq!(Architecture::Unknown.memory_channels(), 0);
    assert_eq!(Architecture::Unknown.tensix_grid(), (0, 0));
}

#[test]
fn host_device_uses_overrides_to_become_renderable() {
    let b = ready_host();
    let d = &b.devices()[0];

    // The host CPU is the Unknown architecture, whose bare geometry is empty…
    assert_eq!(d.architecture, Architecture::Unknown);
    assert_eq!(d.architecture.memory_channels(), 0);
    assert_eq!(d.architecture.tensix_grid(), (0, 0));

    // …so the overrides are what make Starfield/Castle/Flow render anything.
    assert!(d.grid_override.is_some(), "host must set a grid override");
    assert_eq!(d.channels_override, Some(4));
    let (rows, cols) = d.tensix_grid();
    assert!(rows > 0 && cols > 0, "host grid must be non-empty: {rows}x{cols}");
    assert!(d.memory_channels() >= 1);
}

#[test]
fn mock_backends_survive_all_visualizations() {
    // Single chip, one-of-each-arch, doubled, and fleet-scale counts.
    for n in [1usize, 3, 6, 40] {
        drive_all_visualizations(&ready_mock(n));
    }
}

#[test]
fn host_backend_survives_all_visualizations() {
    drive_all_visualizations(&ready_host());
}

#[test]
fn every_backend_device_reports_at_least_one_channel() {
    // The visualizations index DDR channels with `% channels`; every device a
    // backend hands them must report >= 1 channel (TT via architecture, host via
    // override) so the `.max(1)` guard never masks genuine data loss.
    for d in ready_mock(6).devices() {
        assert!(d.memory_channels() >= 1, "mock dev {}", d.index);
    }
    for d in ready_host().devices() {
        assert!(d.memory_channels() >= 1, "host dev {}", d.index);
    }
}
