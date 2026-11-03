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
//!
//! # The season in the log
//!
//! A season in ticks depends on two settings a log reader cannot see: the
//! box's `season_minutes` and `--interval`. So a host writes the season it
//! uses, in ticks, into its bus header as the tag
//! [`crate::config::SAGA_SEASON_TAG`] ([`write_season_tag`]), and every
//! reader takes the season from there ([`season_for_header`]): the local
//! driver, an attached driver (from the snapshot's tags), and
//! `--saga-digest`. A replay host copies the recorded log's tag. So a live
//! run, its replay and its digest agree whatever each reader's own config
//! says (final review finding I4).
//!
//! A log with no tag was recorded before the tag existed. Its readers use
//! [`default_season_ticks`]: `--saga-digest` always, and a replay bus
//! (header kind `Replay`) too, so the two agree. A live bus with no tag
//! (only tests open one) and an older host's snapshot use the caller's
//! config.

use std::collections::BTreeMap;

use crate::config::{LoadedConfig, SagaNames, SagaSettings, SAGA_SEASON_TAG};
use tt_toplike_bus::HeaderKind;
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

/// The season for a log that records none: the default
/// `season_minutes` at the default 10 Hz, 12,000 ticks.
pub fn default_season_ticks() -> u64 {
    season_ticks(SagaSettings::default().season_minutes, None)
}

/// Writes the season a host with `[saga]` settings `s` and a sweep every
/// `interval_ms` uses into `tags`, the header tags of the bus it is about
/// to open (see "The season in the log").
pub fn write_season_tag(tags: &mut BTreeMap<String, String>, s: &SagaSettings, interval_ms: u64) {
    let interval = u32::try_from(interval_ms).unwrap_or(u32::MAX);
    let ticks = season_ticks(s.season_minutes, Some(interval));
    tags.insert(SAGA_SEASON_TAG.to_string(), ticks.to_string());
}

/// The season `tags` record, or `None` when the tag is absent or is not a
/// whole number above zero.
pub fn season_from_tags(tags: &BTreeMap<String, String>) -> Option<u64> {
    tags.get(SAGA_SEASON_TAG)
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&t| t > 0)
}

/// The season a saga reading a bus with header `kind` and `tags` uses: the
/// recorded season; else, for a replay, [`default_season_ticks`]; else
/// `fallback` (the caller's config). See "The season in the log".
pub fn season_for_header(kind: HeaderKind, tags: &BTreeMap<String, String>, fallback: u64) -> u64 {
    match season_from_tags(tags) {
        Some(t) => t,
        None if kind == HeaderKind::Replay => default_season_ticks(),
        None => fallback,
    }
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

    /// The season tag holds the ticks this host's settings give, and a
    /// reader takes it over its own config (final review finding I4).
    #[test]
    fn the_season_tag_round_trips_and_wins_over_the_callers_config() {
        let s = SagaSettings {
            season_minutes: 5,
            ..SagaSettings::default()
        };
        let mut tags = BTreeMap::new();
        write_season_tag(&mut tags, &s, 200);
        assert_eq!(tags[SAGA_SEASON_TAG], "1500");
        assert_eq!(season_from_tags(&tags), Some(1_500));
        for kind in [HeaderKind::Live, HeaderKind::Replay] {
            assert_eq!(season_for_header(kind, &tags, 77), 1_500);
        }
        // No tag: a live bus uses the caller's config, a replay the default.
        let none = BTreeMap::new();
        assert_eq!(season_for_header(HeaderKind::Live, &none, 77), 77);
        assert_eq!(season_for_header(HeaderKind::Replay, &none, 77), 12_000);
        assert_eq!(default_season_ticks(), 12_000);
        // A tag that is not a whole number above zero counts as none.
        for bad in ["0", "-3", "1.5", "", "lots"] {
            let tags = BTreeMap::from([(SAGA_SEASON_TAG.to_string(), bad.to_string())]);
            assert_eq!(season_from_tags(&tags), None, "{bad:?}");
        }
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
