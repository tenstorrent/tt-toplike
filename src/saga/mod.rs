//! Saga, the quest view, on the root crate's side.
//!
//! The story itself is told by the pure core in the `tt_toplike_saga` crate:
//! it turns an ordered stream of bus records into a world, a hero and a
//! chronicle, and the same stream always gives the same saga (spec 3.5).
//! This module holds what needs the root crate:
//!
//! * [`config`] turns the `[saga]` config section into the core's
//!   [`tt_toplike_saga::SagaConfig`] ([`saga_config`]).
//! * [`driver`] runs the core on a background thread fed from the bus and
//!   publishes its state for the view ([`SagaDriver`]).
//! * [`start_for_tui`] starts the TUI's driver at start-up (ledger ruling
//!   R24), and [`start`] holds the decision it makes.
//! * [`digest`] is `--bus-inspect DIR --saga-digest`: the saga a recorded
//!   log tells, as one number, to check the determinism contract.

pub mod config;
pub mod digest;
pub mod driver;

pub use config::saga_config;
pub use driver::{label_for, Published, SagaDriver};

use crate::config::{LoadedConfig, SagaSettings};
use std::path::PathBuf;
use tt_toplike_bus::Bus;

/// Where a driver reads its records.
pub enum SagaSource {
    /// A bus this process hosts.
    Local(Bus),
    /// The bus of another process's host, served on this socket path.
    Attached(PathBuf),
}

/// Starts a driver for `source` with the `[saga]` settings `s`, or none
/// when `[saga] enabled = false` (no thread is started then).
///
/// `interval_ms` is the time between backend sweeps for a local bus; an
/// attached driver counts 10 ticks a second, because a host's snapshot does
/// not say its interval. A driver that cannot start is logged and `None`:
/// the TUI runs without a saga.
pub fn start(s: &SagaSettings, source: SagaSource, interval_ms: u64) -> Option<SagaDriver> {
    if !s.enabled {
        return None;
    }
    let started = match source {
        SagaSource::Local(bus) => {
            let interval = u32::try_from(interval_ms).unwrap_or(u32::MAX);
            SagaDriver::spawn(bus, config::from_settings(s, Some(interval)))
        }
        #[cfg(unix)]
        SagaSource::Attached(path) => tt_toplike_bus::SocketClient::connect(&path)
            .and_then(|c| SagaDriver::spawn_remote(c, &path, config::from_settings(s, None))),
        #[cfg(not(unix))]
        SagaSource::Attached(path) => Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            format!("{}: bus sockets need a Unix system", path.display()),
        )),
    };
    match started {
        Ok(d) => Some(d),
        Err(e) => {
            log::warn!("saga: the story cannot start: {e}");
            None
        }
    }
}

/// The TUI's driver: on the TUI's own bus when it hosts one, on the host's
/// socket when it is attached. `interval_ms` is `--interval`. See [`start`].
pub fn start_for_tui(
    loaded: &LoadedConfig,
    bus: &crate::bus::tui_bus::TuiBus,
    interval_ms: u64,
) -> Option<SagaDriver> {
    let source = match (bus.host(), bus.attached_path()) {
        (Some(h), _) => SagaSource::Local(h.bus().clone()),
        (None, Some(p)) => SagaSource::Attached(p.to_path_buf()),
        (None, None) => return None,
    };
    start(&loaded.saga, source, interval_ms)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tt_toplike_bus::BusConfig;

    fn bus() -> Bus {
        Bus::open(BusConfig::default()).unwrap()
    }

    /// `[saga] enabled = false` starts no driver, so no thread; the default
    /// starts one (ledger ruling R24).
    #[test]
    fn disabling_the_saga_starts_no_thread() {
        let off = SagaSettings {
            enabled: false,
            ..SagaSettings::default()
        };
        assert!(start(&off, SagaSource::Local(bus()), 100).is_none());
        assert!(start(
            &off,
            SagaSource::Attached(PathBuf::from("/nonexistent/s.sock")),
            100
        )
        .is_none());
        let on = start(&SagaSettings::default(), SagaSource::Local(bus()), 100);
        let d = on.expect("the default settings start a driver");
        d.stop();
    }

    /// An attached TUI whose host is gone runs without a saga.
    #[cfg(unix)]
    #[test]
    fn an_attached_saga_with_no_host_is_none() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("gone.sock");
        assert!(start(&SagaSettings::default(), SagaSource::Attached(path), 100).is_none());
    }
}
