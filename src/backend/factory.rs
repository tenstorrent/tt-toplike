// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Backend Factory - Dynamic backend creation and switching
//!
//! This module provides functionality to create and switch between different
//! telemetry backends at runtime, enabling live backend comparison.

use crate::backend::host::HostBackend;
use crate::backend::json::JSONBackend;
use crate::backend::mock::MockBackend;
#[cfg(feature = "remote")]
use crate::backend::ws::WsBackend;
use crate::backend::{BackendConfig, TelemetryBackend};
use crate::cli::{BackendType, Cli};
use crate::error::{BackendError, BackendResult};

#[cfg(target_os = "linux")]
use crate::backend::sysfs::SysfsBackend;

#[cfg(target_os = "linux")]
use crate::backend::hybrid::HybridBackend;

#[cfg(feature = "luwen-backend")]
use crate::backend::luwen::LuwenBackend;

/// Create a backend based on the specified type
///
/// This function attempts to create and initialize the requested backend.
/// If initialization fails, it returns an error without falling back.
pub fn create_backend(
    backend_type: BackendType,
    config: BackendConfig,
    cli: &Cli,
) -> BackendResult<Box<dyn TelemetryBackend>> {
    match backend_type {
        BackendType::Auto => {
            // Auto-detect tries backends in order until one succeeds
            create_auto_backend(config, cli)
        }
        BackendType::Mock => {
            let mut backend = MockBackend::with_config(cli.effective_mock_devices(), config);
            backend.init()?;
            Ok(Box::new(backend))
        }
        BackendType::Json => {
            let tt_smi_path = cli.tt_smi_path.to_string_lossy().to_string();
            let mut backend = JSONBackend::with_config(tt_smi_path, config);
            backend.init()?;
            Ok(Box::new(backend))
        }
        #[cfg(feature = "luwen-backend")]
        BackendType::Luwen => {
            // Catch panics from Luwen backend
            let luwen_result = std::panic::catch_unwind(|| {
                let mut backend = LuwenBackend::with_config(config.clone());
                match backend.init() {
                    Ok(_) => Ok(backend),
                    Err(e) => Err(e),
                }
            });

            match luwen_result {
                Ok(Ok(backend)) => Ok(Box::new(backend)),
                Ok(Err(e)) => Err(e),
                Err(_) => Err(BackendError::Initialization(
                    "Luwen backend panicked (likely hardware access issue)".to_string(),
                )),
            }
        }
        #[cfg(not(feature = "luwen-backend"))]
        BackendType::Luwen => Err(BackendError::Initialization(
            "Luwen backend not compiled (requires --features luwen-backend)".to_string(),
        )),
        // Sysfs and Hybrid are Linux-only variants (see cli::BackendType); on other
        // platforms these variants don't exist, so no match arm is needed for them.
        #[cfg(target_os = "linux")]
        BackendType::Sysfs => {
            let mut backend = SysfsBackend::with_config(config);
            backend.init()?;
            Ok(Box::new(backend))
        }
        #[cfg(target_os = "linux")]
        BackendType::Hybrid => {
            let tt_smi_path = cli.tt_smi_path.to_string_lossy().to_string();
            let mut backend = HybridBackend::with_config(tt_smi_path, config);
            backend.init()?;
            Ok(Box::new(backend))
        }
        BackendType::Host => {
            let mut backend = HostBackend::with_config(config);
            backend.init()?;
            Ok(Box::new(backend))
        }
        // Remote QuietBox — opt-in only, entered exclusively via --remote <HOST:PORT>.
        // Never reached from auto-detect or Tab-cycling (create_auto_backend and
        // next_backend deliberately omit it), preserving the additive contract.
        // The variant is unconditional (cfg-gating an enum variant would ripple
        // through every match arm); the WS backend behind it is gated instead.
        #[cfg(feature = "remote")]
        BackendType::Remote => {
            let spec = cli.remote.as_deref().ok_or_else(|| {
                BackendError::Initialization(
                    "Remote backend requires --remote <HOST:PORT>".to_string(),
                )
            })?;
            // Resolve a bare box name (`--remote qb2-lab`) via `tt --json
            // discover`; a HOST:PORT passes straight through untouched.
            let resolved = crate::backend::discovery::resolve_remote_spec(spec)
                .map_err(BackendError::Initialization)?;
            let mut backend = WsBackend::from_host_port(&resolved, config)?;
            backend.init()?;
            Ok(Box::new(backend))
        }
        #[cfg(not(feature = "remote"))]
        BackendType::Remote => Err(BackendError::Initialization(
            "Remote backend not compiled (built without the 'remote' feature)".to_string(),
        )),
    }
}

/// Auto-detect backend (tries backends in order until one succeeds)
///
/// SAFE MODE: Never tries Luwen (its Luwen/UMD arbitration is unresolved upstream)
/// Order: Hybrid (sysfs + background JSON) → JSON (tt-smi) → Mock
/// Use --backend luwen explicitly if you need direct hardware access.
/// Use --backend sysfs if you want sysfs-only without any JSON background thread.
fn create_auto_backend(
    config: BackendConfig,
    cli: &Cli,
) -> BackendResult<Box<dyn TelemetryBackend>> {
    log::info!("Auto-detecting backend (safe mode - skipping Luwen)...");

    // Try Hybrid backend first on Linux (sysfs + optional background JSON enrichment).
    // Hybrid always succeeds if hwmon devices are present; it runs sysfs-only when
    // tt-smi is absent. This replaces the old Sysfs-first auto-detect.
    #[cfg(target_os = "linux")]
    {
        log::info!("Trying Hybrid backend (sysfs + background JSON enrichment)...");
        if let Ok(backend) = create_backend(BackendType::Hybrid, config.clone(), cli) {
            log::info!("Hybrid backend initialized successfully");
            return Ok(backend);
        }
        log::warn!("Hybrid backend failed (no hwmon devices?), trying JSON backend");
    }

    // Try JSON backend (tt-smi subprocess - safe, works without hwmon)
    log::info!("Trying JSON backend (tt-smi subprocess)...");
    if let Ok(backend) = create_backend(BackendType::Json, config.clone(), cli) {
        log::info!("JSON backend initialized successfully");
        return Ok(backend);
    }
    log::warn!("JSON backend failed, falling back to mock");

    // Fallback to mock backend (always succeeds). This means auto-detect found
    // no real hardware, which the user should see even at the default (Warn) level.
    log::warn!("No hardware backends available, using mock backend");
    log::info!("Tip: Use --backend luwen for direct hardware access (requires PCI permissions)");
    let mut backend = MockBackend::with_config(cli.effective_mock_devices(), config);
    backend.init()?;
    Ok(Box::new(backend))
}

/// Get the next backend in the cycle
///
/// Cycle order (Linux): Hybrid → Sysfs → JSON → Mock → Host → Hybrid
/// Cycle order (other): JSON → Mock → Host → JSON
/// Luwen and Remote are launch-only and never cycled into (see the arms below).
pub fn next_backend(current: BackendType) -> BackendType {
    match current {
        #[cfg(target_os = "linux")]
        BackendType::Hybrid => BackendType::Sysfs,

        #[cfg(target_os = "linux")]
        BackendType::Sysfs => BackendType::Json,

        // Luwen is deliberately skipped: cycling into it would run a fresh
        // chip-detect/init + first-read ARC mailbox message mid-workload —
        // exactly the Luwen/UMD contention window that is still unresolved
        // upstream (DEVINFRA-4445; pyluwen is warned off multi-host topologies
        // entirely). Like Remote, Luwen is launch-only: reachable via
        // `--backend luwen`, and cycling FROM it exits into the normal cycle.
        BackendType::Json => BackendType::Mock,

        BackendType::Luwen => BackendType::Mock,

        BackendType::Mock => BackendType::Host,

        BackendType::Host => {
            #[cfg(target_os = "linux")]
            return BackendType::Hybrid;
            #[cfg(not(target_os = "linux"))]
            return BackendType::Json;
        }

        BackendType::Auto => {
            #[cfg(target_os = "linux")]
            return BackendType::Hybrid;
            #[cfg(not(target_os = "linux"))]
            return BackendType::Json;
        }

        // Remote is a sticky, explicit mode: it is NEVER a target of the cycle
        // (no other arm returns Remote), so Tab-cycling can never enter it. If a
        // user who launched with --remote presses Tab, we drop them into the
        // normal local cycle start and never return to Remote — preserving the
        // additive rule that remote is opt-in only.
        BackendType::Remote => {
            #[cfg(target_os = "linux")]
            return BackendType::Hybrid;
            #[cfg(not(target_os = "linux"))]
            return BackendType::Json;
        }
    }
}

/// Try to create the next available backend, skipping unavailable ones
///
/// This function cycles through backends until it finds one that initializes successfully.
/// Linux cycle has 5 steps (Hybrid→Sysfs→Json→Mock→Host — Luwen and Remote are
/// launch-only, never cycled into); non-Linux has fewer.
/// We try up to 7 times to cover a full Linux cycle plus one retry margin.
pub fn switch_to_next_backend(
    current: BackendType,
    config: BackendConfig,
    cli: &Cli,
) -> BackendResult<(Box<dyn TelemetryBackend>, BackendType)> {
    let mut attempts = 0;
    let mut next = next_backend(current);

    while attempts < 7 {
        log::info!("Attempting to switch to {:?} backend", next);

        match create_backend(next, config.clone(), cli) {
            Ok(backend) => {
                log::info!("Successfully switched to {:?} backend", next);
                return Ok((backend, next));
            }
            Err(e) => {
                log::warn!("Failed to initialize {:?} backend: {}", next, e);
                next = next_backend(next);
                attempts += 1;
            }
        }
    }

    Err(BackendError::Initialization(
        "Failed to initialize any backend after trying all options".to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_next_backend_cycle() {
        // Full Linux cycle: Hybrid → Sysfs → Json → Mock → Host → Hybrid.
        // Luwen is NOT in the live cycle: its ARC-mailbox/UMD contention story
        // is unresolved upstream (DEVINFRA-4445), so it stays launch-only.
        #[cfg(target_os = "linux")]
        {
            let b1 = next_backend(BackendType::Hybrid);
            assert!(matches!(b1, BackendType::Sysfs));

            let b2 = next_backend(b1);
            assert!(matches!(b2, BackendType::Json));

            let b3 = next_backend(b2);
            assert!(matches!(b3, BackendType::Mock));

            let b4 = next_backend(b3);
            assert!(matches!(b4, BackendType::Host));

            let b5 = next_backend(b4);
            assert!(matches!(b5, BackendType::Hybrid));
        }
    }

    /// Luwen is launch-only sticky (like Remote): cycling FROM it exits into
    /// the normal cycle, and no arm ever returns to it — pressing `b` mid-
    /// workload must never initiate a fresh Luwen chip-detect/init.
    #[test]
    fn test_luwen_is_launch_only() {
        assert!(matches!(
            next_backend(BackendType::Luwen),
            BackendType::Mock
        ));
        // No backend cycles INTO Luwen.
        let starts = [
            BackendType::Json,
            BackendType::Mock,
            BackendType::Host,
            BackendType::Auto,
        ];
        for s in starts {
            let mut cur = s;
            for _ in 0..16 {
                cur = next_backend(cur);
                assert!(
                    !matches!(cur, BackendType::Luwen),
                    "cycle from {s:?} must never enter Luwen"
                );
            }
        }
    }

    /// Off-Linux the Sysfs/Hybrid variants don't exist, so the cycle must be a
    /// well-formed loop over only the cross-platform backends. This guards that
    /// removing the `not(target_os = "linux")` Sysfs/Hybrid arms didn't leave a
    /// dangling or non-terminating cycle on macOS/Windows.
    #[cfg(not(target_os = "linux"))]
    #[test]
    fn test_next_backend_cycle_non_linux() {
        // Json → Mock → Host → Json (Luwen is launch-only, never cycled into).
        assert!(matches!(next_backend(BackendType::Json), BackendType::Mock));
        assert!(matches!(
            next_backend(BackendType::Luwen),
            BackendType::Mock
        ));
        assert!(matches!(next_backend(BackendType::Mock), BackendType::Host));
        assert!(matches!(next_backend(BackendType::Host), BackendType::Json));
        // Auto resolves into the cycle at JSON (no Hybrid off-Linux).
        assert!(matches!(next_backend(BackendType::Auto), BackendType::Json));

        // The cycle must return to its start within a bounded number of steps
        // (i.e. it terminates / has no dead end).
        let mut seen = std::collections::HashSet::new();
        let mut cur = BackendType::Json;
        for _ in 0..16 {
            cur = next_backend(cur);
            seen.insert(format!("{:?}", cur));
        }
        for expected in ["Json", "Mock", "Host"] {
            assert!(seen.contains(expected), "cycle should visit {expected}");
        }
    }

    #[test]
    fn test_mock_backend_always_works() {
        let config = BackendConfig::default();

        // Create a mock Cli instance for testing
        let cli = Cli {
            backend: BackendType::Mock,
            mock: Some(0),
            json: false,
            host: false,
            remote: None,
            serve: None,
            tt_smi_path: std::path::PathBuf::from("tt-smi"),
            interval: 100,
            devices: None,
            verbose: false,
            quiet: false,
            mock_devices: 3,
            max_errors: 10,
            timeout: 5000,
            visualize: false,
            workload: false,
            print: false,
            mode: None,
            rotate: false,
            profile: crate::config::AnimationProfile::Normal,
            bench: false,
            throttle: false,
            idle_on_blur: false,
        };

        let result = create_backend(BackendType::Mock, config, &cli);
        assert!(result.is_ok());
    }
}
