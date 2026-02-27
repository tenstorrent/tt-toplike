//! Backend Factory - Dynamic backend creation and switching
//!
//! This module provides functionality to create and switch between different
//! telemetry backends at runtime, enabling live backend comparison.

use crate::backend::{BackendConfig, TelemetryBackend};
use crate::cli::{BackendType, Cli};
use crate::error::{BackendError, BackendResult};
use crate::backend::mock::MockBackend;
use crate::backend::json::JSONBackend;

#[cfg(target_os = "linux")]
use crate::backend::sysfs::SysfsBackend;

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
            let mut backend = MockBackend::with_config(cli.mock_devices, config);
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
        #[cfg(target_os = "linux")]
        BackendType::Sysfs => {
            let mut backend = SysfsBackend::with_config(config);
            backend.init()?;
            Ok(Box::new(backend))
        }
        #[cfg(not(target_os = "linux"))]
        BackendType::Sysfs => Err(BackendError::Initialization(
            "Sysfs backend only available on Linux".to_string(),
        )),
    }
}

/// Auto-detect backend (tries backends in order until one succeeds)
///
/// SAFE MODE: Never tries Luwen backend (invasive, requires PCI BAR0 access)
/// Order: Sysfs (hwmon) → JSON (tt-smi) → Mock
/// Use --backend luwen explicitly if you need direct hardware access
fn create_auto_backend(config: BackendConfig, cli: &Cli) -> BackendResult<Box<dyn TelemetryBackend>> {
    log::info!("Auto-detecting backend (safe mode - skipping Luwen)...");

    // Try Sysfs backend first (hwmon sensors - SAFEST, non-invasive)
    #[cfg(target_os = "linux")]
    {
        log::info!("Trying Sysfs backend (hwmon sensors - safest, non-invasive)...");
        if let Ok(backend) = create_backend(BackendType::Sysfs, config.clone(), cli) {
            log::info!("Sysfs backend initialized successfully");
            return Ok(backend);
        }
        log::warn!("Sysfs backend failed, trying JSON backend");
    }

    // Try JSON backend (tt-smi subprocess - safe)
    log::info!("Trying JSON backend (tt-smi subprocess)...");
    if let Ok(backend) = create_backend(BackendType::Json, config.clone(), cli) {
        log::info!("JSON backend initialized successfully");
        return Ok(backend);
    }
    log::warn!("JSON backend failed, falling back to mock");

    // Fallback to mock backend (always succeeds)
    log::info!("No hardware backends available, using mock backend");
    log::info!("Tip: Use --backend luwen for direct hardware access (requires PCI permissions)");
    let mut backend = MockBackend::with_config(cli.mock_devices, config);
    backend.init()?;
    Ok(Box::new(backend))
}

/// Get the next backend in the cycle
///
/// Cycle order: Sysfs → JSON → Luwen → Mock → Sysfs
pub fn next_backend(current: BackendType) -> BackendType {
    match current {
        #[cfg(target_os = "linux")]
        BackendType::Sysfs => BackendType::Json,
        #[cfg(not(target_os = "linux"))]
        BackendType::Sysfs => BackendType::Json,

        BackendType::Json => BackendType::Luwen,

        #[cfg(feature = "luwen-backend")]
        BackendType::Luwen => BackendType::Mock,
        #[cfg(not(feature = "luwen-backend"))]
        BackendType::Luwen => BackendType::Mock,

        BackendType::Mock => {
            #[cfg(target_os = "linux")]
            return BackendType::Sysfs;
            #[cfg(not(target_os = "linux"))]
            return BackendType::Json;
        }

        BackendType::Auto => {
            #[cfg(target_os = "linux")]
            return BackendType::Sysfs;
            #[cfg(not(target_os = "linux"))]
            return BackendType::Json;
        }
    }
}

/// Try to create the next available backend, skipping unavailable ones
///
/// This function cycles through backends until it finds one that initializes successfully.
/// It tries up to 4 times (one full cycle) before giving up.
pub fn switch_to_next_backend(
    current: BackendType,
    config: BackendConfig,
    cli: &Cli,
) -> BackendResult<(Box<dyn TelemetryBackend>, BackendType)> {
    let mut attempts = 0;
    let mut next = next_backend(current);

    while attempts < 4 {
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
        // Test that cycling goes through all backends and loops back
        #[cfg(target_os = "linux")]
        {
            let start = BackendType::Sysfs;
            let b1 = next_backend(start);
            assert!(matches!(b1, BackendType::Json));

            let b2 = next_backend(b1);
            assert!(matches!(b2, BackendType::Luwen));

            let b3 = next_backend(b2);
            assert!(matches!(b3, BackendType::Mock));

            let b4 = next_backend(b3);
            assert!(matches!(b4, BackendType::Sysfs));
        }
    }

    #[test]
    fn test_mock_backend_always_works() {
        let config = BackendConfig::default();

        // Create a mock Cli instance for testing
        let cli = Cli {
            backend: BackendType::Mock,
            mock: true,
            json: false,
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
        };

        let result = create_backend(BackendType::Mock, config, &cli);
        assert!(result.is_ok());
    }
}
