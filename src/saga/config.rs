//! The `[saga]` config section as the saga core's own config.
//!
//! `crate::config` reads and checks `[saga]` (`enabled`, `names`,
//! `season_minutes`) into [`SagaSettings`]. This module turns those settings
//! into a [`tt_toplike_saga::SagaConfig`]. It is a free function because the
//! orphan rule forbids an inherent method on a type from another crate
//! (ledger ruling R8).
//!
//! # Minutes to ticks
//!
//! The core counts time in ticks: one tick per backend sweep (spec 3.5). A
//! season of `m` minutes is `m * 60 * 1000 / interval_ms` ticks, where
//! `interval_ms` is the time between sweeps: the interval the log's header
//! records when the caller knows it, else 100 ms (10 Hz, the backend's
//! default). The product saturates, so no input overflows, and the result is
//! at least one tick.

use crate::config::{LoadedConfig, SagaNames, SagaSettings};
use tt_toplike_saga::chronicle::NameMode;
use tt_toplike_saga::{Label, SagaConfig};

/// The sweep interval used when the caller knows none: 100 ms, 10 Hz.
pub const DEFAULT_INTERVAL_MS: u32 = 100;

/// The saga core's config for `cfg`'s `[saga]` section.
///
/// `interval_ms` is the time between backend sweeps, in milliseconds: the
/// interval the log's header records, when known. `None` or `Some(0)` uses
/// [`DEFAULT_INTERVAL_MS`]. See the module comment for the conversion.
///
/// The label is [`Label::Live`]. The driver replaces it with the label the
/// bus header calls for (see `crate::saga::driver::label_for`), so the
/// caller never has to set it.
pub fn saga_config(cfg: &LoadedConfig, interval_ms: Option<u32>) -> SagaConfig {
    from_settings(&cfg.saga, interval_ms)
}

/// [`saga_config`] for the `[saga]` settings alone.
pub fn from_settings(s: &SagaSettings, interval_ms: Option<u32>) -> SagaConfig {
    SagaConfig {
        season_ticks: season_ticks(s.season_minutes, interval_ms),
        names: match s.names {
            SagaNames::Real => NameMode::Real,
            SagaNames::Generated => NameMode::Generated,
        },
        label: Label::Live,
    }
}

/// Ticks in a season of `minutes` at one sweep every `interval_ms`
/// milliseconds (see the module comment). Never 0.
pub fn season_ticks(minutes: u32, interval_ms: Option<u32>) -> u64 {
    let interval = match interval_ms {
        Some(0) | None => DEFAULT_INTERVAL_MS,
        Some(ms) => ms,
    };
    let ms = u64::from(minutes).saturating_mul(60 * 1000);
    (ms / u64::from(interval)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{load_from, SAGA_SEASON_MAX};
    use std::path::PathBuf;

    fn loaded(text: &str) -> LoadedConfig {
        load_from(&[(PathBuf::from("c.toml"), text.to_string())])
    }

    /// No `[saga]` section: 20 minutes at 10 Hz is 12,000 ticks (spec
    /// 3.5), real names, the live label.
    #[test]
    fn the_defaults_give_a_twenty_minute_season_at_ten_hertz() {
        let c = saga_config(&loaded(""), None);
        assert_eq!(c.season_ticks, 12_000);
        assert_eq!(c.names, NameMode::Real);
        assert_eq!(c.label, Label::Live);
    }

    #[test]
    fn minutes_become_ticks_with_the_recorded_interval() {
        let cfg = loaded("[saga]\nseason_minutes = 3\n");
        assert_eq!(saga_config(&cfg, None).season_ticks, 1_800);
        assert_eq!(saga_config(&cfg, Some(100)).season_ticks, 1_800);
        assert_eq!(saga_config(&cfg, Some(50)).season_ticks, 3_600);
        assert_eq!(saga_config(&cfg, Some(1000)).season_ticks, 180);
        assert_eq!(saga_config(&cfg, Some(7)).season_ticks, 25_714);
        // An interval of 0 is not a rate: the default is used.
        assert_eq!(saga_config(&cfg, Some(0)).season_ticks, 1_800);
    }

    #[test]
    fn the_conversion_saturates_and_never_gives_zero() {
        assert_eq!(season_ticks(1, Some(u32::MAX)), 1);
        assert_eq!(
            season_ticks(u32::MAX, Some(1)),
            u64::from(u32::MAX) * 60_000
        );
        assert_eq!(season_ticks(SAGA_SEASON_MAX, Some(1)), 86_400_000);
    }

    #[test]
    fn names_follow_the_section() {
        let c = saga_config(&loaded("[saga]\nnames = \"generated\"\n"), None);
        assert_eq!(c.names, NameMode::Generated);
        let c = saga_config(&loaded("[saga]\nnames = \"real\"\n"), None);
        assert_eq!(c.names, NameMode::Real);
        // A bad value keeps the default, real names.
        let c = saga_config(&loaded("[saga]\nnames = \"nope\"\n"), None);
        assert_eq!(c.names, NameMode::Real);
    }
}
