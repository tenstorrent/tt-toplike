// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Animation configuration: profiles and user-tunable thresholds.

use serde::Deserialize;
use std::path::PathBuf;

/// Named animation sensitivity presets.
///
/// Controls how responsive all visualizations are to hardware activity changes.
/// Applies universally to Memory Castle, Starfield, Memory Flow, and chip portrait particles.
#[derive(Debug, Clone, Copy, PartialEq, clap::ValueEnum)]
pub enum AnimationProfile {
    /// Only extreme spikes trigger visible activity (5× power increase needed)
    AnomaliesOnly,
    /// Gentle response; light workloads appear nearly still
    Relaxed,
    /// Balanced default — mirrors observed idle vs. load contrast
    Normal,
    /// Heightened — moderate workloads produce vigorous animation
    Hyper,
    /// Maximum sensitivity; every fluctuation is visible
    Paranoid,
}

impl Default for AnimationProfile {
    fn default() -> Self {
        Self::Normal
    }
}

/// Resolved animation configuration applied to all viz objects.
#[derive(Debug, Clone)]
pub struct AnimConfig {
    /// Multiplier applied to every `AdaptiveBaseline::power_change()` result.
    /// 1.0 = Normal; 0.2 = AnomaliesOnly; 5.0 = Paranoid.
    pub sensitivity: f32,

    /// Scale factor for particle caps (multiplied into base counts).
    /// 1.0 = Normal; 3.0 = Paranoid.
    pub max_particles_scale: f32,
}

impl AnimConfig {
    /// Build a config from a named preset.
    pub fn from_profile(profile: AnimationProfile) -> Self {
        match profile {
            AnimationProfile::AnomaliesOnly => Self {
                sensitivity: 0.2,
                max_particles_scale: 0.5,
            },
            AnimationProfile::Relaxed => Self {
                sensitivity: 0.5,
                max_particles_scale: 0.75,
            },
            AnimationProfile::Normal => Self {
                sensitivity: 1.0,
                max_particles_scale: 1.0,
            },
            AnimationProfile::Hyper => Self {
                sensitivity: 2.0,
                max_particles_scale: 2.0,
            },
            AnimationProfile::Paranoid => Self {
                sensitivity: 5.0,
                max_particles_scale: 3.0,
            },
        }
    }

    /// Memory Castle max_particles for this config (base: 600).
    pub fn castle_max_particles(&self) -> usize {
        ((600.0 * self.max_particles_scale) as usize).max(20)
    }

    /// Memory Flow max_particles for this config (base: 200).
    pub fn flow_max_particles(&self) -> usize {
        ((200.0 * self.max_particles_scale) as usize).max(10)
    }

    /// Portrait max_particles per device for this config (base: 32).
    pub fn portrait_max_particles(&self) -> usize {
        ((32.0 * self.max_particles_scale) as usize).max(4)
    }

    /// Apply user-provided TOML overrides on top of profile defaults.
    pub fn merge(mut self, overrides: AnimConfigOverrides) -> Self {
        if let Some(v) = overrides.sensitivity {
            self.sensitivity = v;
        }
        if let Some(v) = overrides.max_particles_scale {
            self.max_particles_scale = v;
        }
        self
    }
}

/// Partial overrides from `~/.config/tt-toplike/config.toml`.
/// All fields optional — missing = keep profile default.
#[derive(Debug, Default, Deserialize)]
pub struct AnimConfigOverrides {
    pub sensitivity: Option<f32>,
    pub max_particles_scale: Option<f32>,

    /// Equivalent of `--tt-smi-reset-behavior` (`ignore`, `inform`, `dazzle`
    /// or `demo`, any letter case). The CLI flag wins when both are set. This
    /// struct holds it because it is the only config file this project has.
    pub tt_smi_reset_behavior: Option<String>,
}

/// Load config overrides from `~/.config/tt-toplike/config.toml` (see
/// [`load_config`]). Every problem in the file is logged; a file with a
/// syntax error contributes nothing.
pub fn load_config_overrides() -> AnimConfigOverrides {
    let loaded = load_config();
    for p in &loaded.problems {
        log::warn!("config: {p}");
    }
    loaded.overrides
}

/// One problem found in a config file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigProblem {
    pub file: PathBuf,
    /// The 1-based line, when the parser gives a position.
    pub line: Option<usize>,
    pub message: String,
}

impl std::fmt::Display for ConfigProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.line {
            Some(l) => write!(f, "{}:{}: {}", self.file.display(), l, self.message),
            None => write!(f, "{}: {}", self.file.display(), self.message),
        }
    }
}

/// Everything read from the config files.
#[derive(Debug, Default)]
pub struct LoadedConfig {
    pub overrides: AnimConfigOverrides,
    /// The arcade's settings. The default layout when no file sets one, and
    /// also when any `[arcade]` setting has a problem.
    pub arcade: crate::animation::arcade_layout::ArcadeConfig,
    /// The backdrop of each view (`[backdrop]`, and `arcade.cloudfield`).
    pub backdrop: crate::animation::backdrop::BackdropConfig,
    /// Every problem, in line order. A file that cannot be read gives one
    /// problem with no line.
    pub problems: Vec<ConfigProblem>,
    /// Notes that are not problems: a deprecated key that still works. They
    /// are shown, and `--check-config` still passes.
    pub notes: Vec<ConfigProblem>,
    /// The files that were read (none, or the shared config file).
    pub files: Vec<PathBuf>,
    /// The file's named arcade layouts, in the order `}` steps through
    /// them: `config` (the `[[arcade.row]]` tables) when the file has them
    /// and `[arcade]` has no problem, then each `[[arcade.layouts]]` with no
    /// problem, in file order. The presets follow them in the cycle
    /// (`arcade_presets::LayoutCycle`); they are not listed here.
    pub layouts: Vec<crate::animation::arcade_presets::NamedLayout>,
    /// The layout to start with: `[arcade] layout = "name"`, or the
    /// `--arcade-layout` flag through [`choose_layout`]. Always a known
    /// name; `None` starts with the first layout of the cycle.
    pub start_layout: Option<String>,
    /// The `[saga]` settings: whether the quest view's story runs, whose
    /// names it uses, and how long a season lasts. A bad value keeps that
    /// key's default.
    pub saga: SagaSettings,
    /// The `[viz]` settings: which plugin views to enable and each one's
    /// own table. Default (nothing enabled) when the file has no `[viz]`.
    pub viz: VizSettings,
}

/// The `[viz]` section: `enable = ["id", ...]` lists the plugin views to
/// add to the `v` cycle on top of those enabled by default, and each
/// `[viz.<id>]` table is kept whole and handed to that viz's `configure`
/// at first use.
///
/// The ids are checked for shape only. The registry is not populated when
/// the config is parsed (YAML viz files are scanned afterwards), so whether
/// an id names a real viz is asked later with [`VizSettings::unknown_enabled`].
#[derive(Debug, Clone, Default)]
pub struct VizSettings {
    /// Ids from `enable`, in file order. Later files replace the list.
    pub enable: Vec<String>,
    /// `[viz.<id>]` tables by id, merged key-by-key across files.
    pub per_viz: std::collections::BTreeMap<String, toml_edit::Table>,
}

impl VizSettings {
    /// The ids in `enable` that `known` rejects. The caller (the TUI's viz
    /// host) passes the registry lookup after the YAML scan and raises a
    /// config problem for each one; this crate cannot ask the registry
    /// itself because it compiles without the `tui` feature.
    pub fn unknown_enabled(&self, known: &dyn Fn(&str) -> bool) -> Vec<String> {
        self.enable
            .iter()
            .filter(|id| !known(id))
            .cloned()
            .collect()
    }
}

/// True when `id` is kebab-case: `[a-z0-9]+(-[a-z0-9]+)*`.
///
/// This duplicates `crate::viz::registry::is_valid_id` on purpose:
/// `config.rs` compiles without the `tui` feature, where `crate::viz` does
/// not exist. The test `config_and_registry_agree_on_what_a_viz_id_is`
/// (tui form) keeps the two copies in step on a table of ids.
fn is_valid_viz_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        })
}

/// Which names the saga's chronicle uses (`[saga] names`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SagaNames {
    /// Real model and process names (the default).
    #[default]
    Real,
    /// Names drawn from the lexicon in place of every real name, for a
    /// shared screen.
    Generated,
}

impl SagaNames {
    /// The word the config file uses.
    pub fn name(self) -> &'static str {
        match self {
            SagaNames::Real => "real",
            SagaNames::Generated => "generated",
        }
    }
}

/// The shortest season `[saga] season_minutes` accepts.
pub const SAGA_SEASON_MIN: u32 = 1;
/// The longest season `[saga] season_minutes` accepts: one day.
pub const SAGA_SEASON_MAX: u32 = 1440;
/// The season when the file sets none (spec 3.5: 12,000 ticks at 10 Hz).
pub const SAGA_SEASON_DEFAULT: u32 = 20;
/// The bus header tag that records a saga season in ticks (final review
/// finding I4). A host writes it when it opens its bus, a replay host
/// carries the recorded log's value forward, and every saga reader takes
/// its season from it. See `crate::saga::config`.
pub const SAGA_SEASON_TAG: &str = "saga.season_ticks";

/// The `[saga]` section. The root crate turns it into the saga core's own
/// config with `crate::saga::saga_config`, which converts the minutes into
/// ticks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SagaSettings {
    /// `enabled`: the TUI runs the saga's driver thread from start-up
    /// (ledger ruling R24). `false` starts no thread at all.
    pub enabled: bool,
    /// `names`: `"real"` or `"generated"`.
    pub names: SagaNames,
    /// `season_minutes`: quiet minutes that end a world,
    /// [`SAGA_SEASON_MIN`] to [`SAGA_SEASON_MAX`].
    pub season_minutes: u32,
}

impl Default for SagaSettings {
    fn default() -> Self {
        SagaSettings {
            enabled: true,
            names: SagaNames::Real,
            season_minutes: SAGA_SEASON_DEFAULT,
        }
    }
}

/// The status line's text for `problems`: the first problem, and how many
/// more there are. `None` with no problem. The full list is in the log and
/// in `--check-config`.
pub fn problem_summary(problems: &[ConfigProblem]) -> Option<String> {
    let first = problems.first()?;
    let more = problems.len() - 1;
    Some(if more == 0 {
        format!("config: {first}")
    } else {
        format!("config: {first} (and {more} more; run --check-config)")
    })
}

/// The status line's message at start-up: the first problem as an error,
/// else the first note, else `None`.
pub fn status_message(loaded: &LoadedConfig) -> Option<(String, bool)> {
    if let Some(m) = problem_summary(&loaded.problems) {
        return Some((m, true));
    }
    loaded
        .notes
        .first()
        .map(|n| (format!("config: {n}"), false))
}

/// Write the `--check-config` report for `loaded` to `out` and return the
/// exit code: 0 with no problem, 1 with any. The report names the files
/// read, the arcade layout in use, the backdrops, the `[saga]` settings, and
/// every problem as `file:line: message`.
pub fn write_check_report(loaded: &LoadedConfig, out: &mut dyn std::io::Write) -> i32 {
    let _ = (|| -> std::io::Result<()> {
        // A file that exists but cannot be read is a problem, printed
        // below; only no file at all is "no config file found".
        if loaded.files.is_empty() && loaded.problems.is_empty() {
            writeln!(out, "no config file found (looked for: {})", {
                config_files()
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            })?;
        }
        for f in &loaded.files {
            writeln!(out, "read {}", f.display())?;
        }
        for p in &loaded.problems {
            writeln!(out, "{p}")?;
        }
        for n in &loaded.notes {
            writeln!(out, "note: {n}")?;
        }
        // The layout the arcade starts with and its rows. The arcade
        // carries no backdrop.
        let cycle = crate::animation::arcade_presets::LayoutCycle::new(
            &loaded.layouts,
            loaded.start_layout.as_deref(),
        );
        let start = cycle.current();
        let n = start.rows.len();
        writeln!(
            out,
            "arcade starts with {}: {n} row{}",
            start.name,
            if n == 1 { "" } else { "s" },
        )?;
        // Every layout `{` and `}` step through, with its rows.
        writeln!(
            out,
            "layouts, in the order }} steps through them (* starts):"
        )?;
        for (n, l) in cycle.layouts().iter().enumerate() {
            let mark = if n == cycle.index() { '*' } else { ' ' };
            if l.description.is_empty() {
                writeln!(out, "{mark} {}", l.name)?;
            } else {
                writeln!(out, "{mark} {}  {}", l.name, l.description)?;
            }
            for (i, r) in l.rows.iter().enumerate() {
                let panels: Vec<String> = r
                    .panels
                    .iter()
                    .map(|p| format!("{} x{}", p.view.key(), p.width))
                    .collect();
                writeln!(
                    out,
                    "    row {} (height {}): {}",
                    i + 1,
                    r.height,
                    panels.join(", ")
                )?;
            }
        }
        // The backdrops: the default, each view set, and the views that
        // have one of their own.
        {
            use crate::animation::backdrop::{builtin, choice_name, VIEWS};
            let b = &loaded.backdrop;
            let mut line = format!("backdrop: default {}", choice_name(b.default.flatten()));
            if !b.views.is_empty() {
                let set: Vec<String> = b
                    .views
                    .iter()
                    .map(|(v, c)| format!("{v} {}", choice_name(*c)))
                    .collect();
                line.push_str(&format!("; {}", set.join(", ")));
            }
            writeln!(out, "{line}")?;
            let own: Vec<String> = VIEWS
                .iter()
                .filter_map(|v| builtin(v).map(|k| format!("{v} {}", k.name())))
                .collect();
            writeln!(out, "  built in: {} (unless set above)", own.join(", "))?;
        }
        // The saga: on or off, its names and its season.
        let s = &loaded.saga;
        if s.enabled {
            writeln!(
                out,
                "saga: on, names {}, season {} minute{}",
                s.names.name(),
                s.season_minutes,
                if s.season_minutes == 1 { "" } else { "s" }
            )?;
        } else {
            writeln!(out, "saga: off ([saga] enabled = false)")?;
        }
        // The plugin views: silent unless `[viz] enable` names some.
        if !loaded.viz.enable.is_empty() {
            writeln!(out, "viz: enabled {}", loaded.viz.enable.join(", "))?;
        }
        match loaded.problems.len() {
            0 => writeln!(out, "ok")?,
            n => writeln!(
                out,
                "{n} problem{}; the TUI uses the default for each one",
                if n == 1 { "" } else { "s" }
            )?,
        }
        Ok(())
    })();
    i32::from(!loaded.problems.is_empty())
}

/// The largest config file read, in bytes. A larger file is a problem.
pub const MAX_CONFIG_BYTES: u64 = 256 * 1024;

/// The config files: the shared `~/.config/tt-toplike/config.toml`, or
/// nothing when the platform has no config directory. There is no
/// per-directory file, so a repo you check out cannot change tt-toplike.
pub fn config_files() -> Vec<PathBuf> {
    config_path().into_iter().collect()
}

/// Read the shared config file (see [`config_files`]).
///
/// The TUI calls this once, in `run_tui`, before the terminal enters raw
/// mode, so a problem here never leaves a blank raw-mode screen.
pub fn load_config() -> LoadedConfig {
    load_config_file(config_path().as_deref())
}

/// Read the config file at `path` (none with `None`) and check it. A file
/// that cannot be read, or that [`read_config_file`] refuses, is one
/// problem, and every setting keeps its default.
pub fn load_config_file(path: Option<&std::path::Path>) -> LoadedConfig {
    let Some(path) = path else {
        return load_from(&[]);
    };
    match read_config_file(path) {
        Ok(Some(text)) => load_from(&[(path.to_path_buf(), text)]),
        Ok(None) => load_from(&[]),
        Err(message) => {
            let mut loaded = load_from(&[]);
            loaded.problems.push(ConfigProblem {
                file: path.to_path_buf(),
                line: None,
                message,
            });
            loaded
        }
    }
}

/// The text of the config file at `path`. `Ok(None)` when there is no file.
/// `Err` holds the problem's message.
///
/// Only a regular file of at most [`MAX_CONFIG_BYTES`] is read. A symlink is
/// followed and the file it points to is checked, so a symlink to a regular
/// file works. A directory, FIFO, socket or device is never opened: a FIFO
/// or a terminal would block the read, and `/dev/zero` would never end it.
/// The type is checked again on the open file, in case the path changed in
/// between, and the read stops one byte past the cap.
pub fn read_config_file(path: &std::path::Path) -> Result<Option<String>, String> {
    use std::io::Read;
    let unreadable = |e: std::io::Error| format!("cannot be read: {e}");
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // A symlink whose target is missing is still a file someone
            // put there.
            return match std::fs::symlink_metadata(path) {
                Ok(_) => Err("is a symlink to a file that does not exist".into()),
                Err(_) => Ok(None),
            };
        }
        Err(e) => return Err(unreadable(e)),
    };
    check_config_meta(&meta)?;
    let mut opts = std::fs::OpenOptions::new();
    opts.read(true);
    // Should a FIFO take the file's place after the check above, opening it
    // without O_NONBLOCK would wait for a writer.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.custom_flags(libc::O_NONBLOCK);
    }
    let file = opts.open(path).map_err(unreadable)?;
    check_config_meta(&file.metadata().map_err(unreadable)?)?;
    let mut bytes = Vec::new();
    file.take(MAX_CONFIG_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(unreadable)?;
    if bytes.len() as u64 > MAX_CONFIG_BYTES {
        return Err(too_large(bytes.len() as u64));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|_| "is not UTF-8 text".into())
}

/// The problem with a config file's metadata: not a regular file, or over
/// the size cap.
fn check_config_meta(meta: &std::fs::Metadata) -> Result<(), String> {
    let t = meta.file_type();
    if !t.is_file() {
        let kind = if t.is_dir() {
            "a directory"
        } else {
            special_file_kind(&t)
        };
        return Err(format!("is {kind}; the config must be a regular file"));
    }
    if meta.len() > MAX_CONFIG_BYTES {
        return Err(too_large(meta.len()));
    }
    Ok(())
}

/// What kind of file `t` is, for a file that is neither a regular file nor
/// a directory.
#[cfg(unix)]
fn special_file_kind(t: &std::fs::FileType) -> &'static str {
    use std::os::unix::fs::FileTypeExt;
    if t.is_fifo() {
        "a FIFO"
    } else if t.is_char_device() {
        "a character device"
    } else if t.is_block_device() {
        "a block device"
    } else if t.is_socket() {
        "a socket"
    } else {
        "a special file"
    }
}

#[cfg(not(unix))]
fn special_file_kind(_: &std::fs::FileType) -> &'static str {
    "a special file"
}

/// The message for a config file of `len` bytes, over the cap.
fn too_large(len: u64) -> String {
    format!(
        "is larger than {} KiB ({len} bytes); it was not read",
        MAX_CONFIG_BYTES / 1024
    )
}

/// A parser message on one line: toml_edit's messages can span several.
fn one_line(message: &str) -> String {
    message
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("; ")
}

/// The keys allowed at the top of a config file.
const TOP_KEYS: [&str; 7] = [
    "sensitivity",
    "max_particles_scale",
    "tt_smi_reset_behavior",
    "arcade",
    "backdrop",
    "saga",
    "viz",
];

/// Merge config `sources` (each a path and its text), later ones winning per
/// key, and collect every problem. Pure: no file is read. The TUI passes
/// one source at most, the shared config file. See
/// `docs/arcade-layout.md` for the `[arcade]` schema and the messages.
pub fn load_from(sources: &[(PathBuf, String)]) -> LoadedConfig {
    use crate::animation::arcade_layout::ArcadeConfig;
    let mut out = LoadedConfig::default();
    let mut cloudfield: Option<bool> = None;
    // Where `arcade.cloudfield` was set, for its deprecation note.
    let mut cloudfield_at: Option<(PathBuf, Option<usize>)> = None;
    let mut backdrop = crate::animation::backdrop::BackdropConfig::default();
    let mut rows: Option<Vec<crate::animation::arcade_layout::RowSpec>> = None;
    let mut arcade_bad = false;
    let mut layouts: Option<Vec<crate::animation::arcade_presets::NamedLayout>> = None;
    let mut start: Option<String> = None;
    for (path, text) in sources {
        out.files.push(path.clone());
        let first_problem = out.problems.len();
        let mut report = |span: Option<std::ops::Range<usize>>, message: String| {
            out.problems.push(ConfigProblem {
                file: path.clone(),
                line: span.map(|s| line_of(text, s.start)),
                message,
            });
        };
        let doc = match toml_edit::ImDocument::parse(text.as_str()) {
            Ok(d) => d,
            Err(e) => {
                report(
                    e.span(),
                    format!("TOML syntax error: {}", one_line(e.message())),
                );
                continue;
            }
        };
        let top = doc.as_table();
        for (k, _) in top.iter() {
            if !TOP_KEYS.contains(&k) {
                let span = top.key(k).and_then(|key| key.span());
                report(span, format!("unknown key `{k}`"));
            }
        }
        // The existing keys, through serde as before.
        match toml::from_str::<AnimConfigOverrides>(text) {
            Ok(o) => {
                if o.sensitivity.is_some() {
                    out.overrides.sensitivity = o.sensitivity;
                }
                if o.max_particles_scale.is_some() {
                    out.overrides.max_particles_scale = o.max_particles_scale;
                }
                if let Some(v) = o.tt_smi_reset_behavior {
                    // An unknown value is a problem here, so --check-config
                    // reports it, and the TUI uses the default.
                    use clap::ValueEnum;
                    if crate::cli::ResetBehavior::from_str(v.trim(), true).is_ok() {
                        out.overrides.tt_smi_reset_behavior = Some(v);
                    } else {
                        let span = top.get("tt_smi_reset_behavior").and_then(|i| i.span());
                        report(
                            span,
                            format!(
                                "unknown tt_smi_reset_behavior {v:?} (expected ignore, inform, dazzle or demo); the TUI uses inform"
                            ),
                        );
                    }
                }
            }
            Err(e) => report(e.span(), one_line(e.message())),
        }
        if let Some(item) = top.get("arcade") {
            let mut arcade_report = |span: Option<std::ops::Range<usize>>, message: String| {
                out.problems.push(ConfigProblem {
                    file: path.clone(),
                    line: span.map(|s| line_of(text, s.start)),
                    message,
                });
            };
            let parsed = parse_arcade(item, &mut arcade_report);
            // The old rule holds for everything in `[arcade]` but the named
            // layouts and the start name: a problem there gives the whole
            // default arcade. A problem in one `[[arcade.layouts]]` drops
            // only that layout (it is left out of `parsed.layouts`).
            if parsed.core_bad {
                arcade_bad = true;
            }
            for (span, message) in &parsed.notes {
                out.notes.push(ConfigProblem {
                    file: path.clone(),
                    line: span.clone().map(|s| line_of(text, s.start)),
                    message: message.clone(),
                });
            }
            if let Some(c) = parsed.cloudfield {
                cloudfield = Some(c);
                let line = item
                    .as_table_like()
                    .and_then(|t| t.get("cloudfield"))
                    .and_then(|v| v.span())
                    .map(|sp| line_of(text, sp.start));
                cloudfield_at = Some((path.clone(), line));
            }
            let has_rows = parsed.rows.is_some();
            if let Some(r) = parsed.rows {
                rows = Some(r);
            }
            if let Some(l) = parsed.layouts {
                layouts = Some(l);
            }
            if let Some((name, span)) = parsed.start {
                // Checked against the layouts this file gives.
                let mut file = Vec::new();
                if has_rows && !arcade_bad {
                    file.push(config_layout(Vec::new()));
                }
                file.extend(layouts.iter().flatten().cloned());
                let cycle = crate::animation::arcade_presets::LayoutCycle::new(&file, None);
                if cycle.find(&name).is_some() {
                    start = Some(name);
                } else {
                    start = None;
                    arcade_report(
                        span,
                        format!(
                            "{}; the arcade starts with `{}`",
                            cycle.unknown(&name),
                            cycle.current().name
                        ),
                    );
                }
            }
        }
        if let Some(item) = top.get("backdrop") {
            let mut backdrop_report = |span: Option<std::ops::Range<usize>>, message: String| {
                out.problems.push(ConfigProblem {
                    file: path.clone(),
                    line: span.map(|s| line_of(text, s.start)),
                    message,
                });
            };
            let mut backdrop_notes = Vec::new();
            parse_backdrop(
                item,
                &mut backdrop,
                &mut backdrop_report,
                &mut backdrop_notes,
            );
            for (span, message) in backdrop_notes {
                out.notes.push(ConfigProblem {
                    file: path.clone(),
                    line: span.map(|s| line_of(text, s.start)),
                    message,
                });
            }
            // A default the views without a backdrop of their own refuse is
            // ignored there; say so once.
            if let Some(k) = backdrop.default.flatten() {
                use crate::animation::backdrop::{builtin, refusal, takers, VIEWS};
                let line = item
                    .as_table_like()
                    .and_then(|t| t.get("default"))
                    .and_then(|v| v.span())
                    .map(|sp| line_of(text, sp.start));
                let open: Vec<&str> = VIEWS
                    .iter()
                    .copied()
                    .filter(|v| builtin(v).is_none() && !backdrop.views.iter().any(|(x, _)| x == v))
                    .collect();
                let refused: Vec<&str> = open
                    .iter()
                    .copied()
                    .filter(|v| refusal(v, k).is_some())
                    .collect();
                let message = if refused.len() == open.len() {
                    format!(
                        "backdrop default {} is ignored: {}, and each draws its own",
                        k.name(),
                        takers()
                    )
                } else {
                    format!(
                        "backdrop default {} is ignored by {}: no backdrop on these views",
                        k.name(),
                        refused.join(", ")
                    )
                };
                if !refused.is_empty() {
                    out.notes.push(ConfigProblem {
                        file: path.clone(),
                        line,
                        message,
                    });
                }
            }
        }
        if let Some(item) = top.get("saga") {
            let mut saga_report = |span: Option<std::ops::Range<usize>>, message: String| {
                out.problems.push(ConfigProblem {
                    file: path.clone(),
                    line: span.map(|s| line_of(text, s.start)),
                    message,
                });
            };
            parse_saga(item, &mut out.saga, &mut saga_report);
        }
        if let Some(item) = top.get("viz") {
            let mut viz_report = |span: Option<std::ops::Range<usize>>, message: String| {
                out.problems.push(ConfigProblem {
                    file: path.clone(),
                    line: span.map(|s| line_of(text, s.start)),
                    message,
                });
            };
            parse_viz(item, &mut out.viz, &mut viz_report);
        }
        // This file's problems in line order.
        out.problems[first_problem..].sort_by_key(|p| p.line.unwrap_or(0));
    }
    // `arcade.cloudfield` is still read, and ignored with a note: the
    // arcade carries no backdrop.
    if let (Some(_), Some((file, line))) = (cloudfield.filter(|_| !arcade_bad), cloudfield_at) {
        out.notes.push(ConfigProblem {
            file,
            line,
            message: format!(
                "`arcade.cloudfield` is ignored: {}",
                crate::animation::arcade_layout::NO_BACKDROP_NOTE
            ),
        });
    }
    out.backdrop = backdrop;
    let mut arcade = ArcadeConfig::default();
    if !arcade_bad {
        if let Some(r) = rows {
            out.layouts.push(config_layout(r.clone()));
            arcade.rows = r;
        }
    }
    out.arcade = arcade;
    out.layouts.extend(layouts.unwrap_or_default());
    // A start name that was valid for its file is still valid here, unless
    // a later problem took the `config` layout away.
    out.start_layout = start.filter(|n| {
        crate::animation::arcade_presets::LayoutCycle::new(&out.layouts, None)
            .find(n)
            .is_some()
    });
    out
}

/// The `[[arcade.row]]` tables as the layout named `config`.
fn config_layout(
    rows: Vec<crate::animation::arcade_layout::RowSpec>,
) -> crate::animation::arcade_presets::NamedLayout {
    crate::animation::arcade_presets::NamedLayout {
        name: crate::animation::arcade_presets::CONFIG_LAYOUT.into(),
        description: "the [[arcade.row]] tables in config.toml".into(),
        rows,
    }
}

/// Start with the layout `name` from `--arcade-layout`, which wins over
/// `[arcade] layout`. An unknown name is a problem (with `--arcade-layout`
/// in place of a file) and the arcade starts with the first layout of the
/// cycle. `None` leaves `loaded` as it is.
pub fn choose_layout(loaded: &mut LoadedConfig, name: Option<&str>) {
    let Some(name) = name else {
        return;
    };
    let cycle = crate::animation::arcade_presets::LayoutCycle::new(&loaded.layouts, None);
    if cycle.find(name).is_some() {
        loaded.start_layout = Some(name.to_string());
    } else {
        loaded.start_layout = None;
        loaded.problems.push(ConfigProblem {
            file: PathBuf::from("--arcade-layout"),
            line: None,
            message: format!(
                "{}; the arcade starts with `{}`",
                cycle.unknown(name),
                cycle.current().name
            ),
        });
    }
}

/// The 1-based line of byte `offset` in `text`.
fn line_of(text: &str, offset: usize) -> usize {
    text.as_bytes()[..offset.min(text.len())]
        .iter()
        .filter(|&&b| b == b'\n')
        .count()
        + 1
}

/// A problem's place and message.
type Report<'a> = dyn FnMut(Option<std::ops::Range<usize>>, String) + 'a;

/// What the `[arcade]` table holds. Each part is `None` when not set or
/// not valid.
#[derive(Default)]
struct ArcadeParsed {
    cloudfield: Option<bool>,
    /// The `[[arcade.row]]` tables.
    rows: Option<Vec<crate::animation::arcade_layout::RowSpec>>,
    /// The `[[arcade.layouts]]` with no problem, in file order; `Some` when
    /// the key is there.
    layouts: Option<Vec<crate::animation::arcade_presets::NamedLayout>>,
    /// `layout = "name"` and where it is. `load_from` checks the name
    /// against the layouts afterwards.
    start: Option<(String, Option<std::ops::Range<usize>>)>,
    /// A problem outside the named layouts and the start name.
    core_bad: bool,
    /// Deprecation notes from the kept layouts (`cloudfield` in a layout).
    notes: Vec<(Option<std::ops::Range<usize>>, String)>,
}

/// The `[arcade]` table. Every problem goes to `report`.
fn parse_arcade(item: &toml_edit::Item, report: &mut Report<'_>) -> ArcadeParsed {
    let mut out = ArcadeParsed::default();
    // Problems are collected here first, so the core ones can be told from
    // the per-layout ones, then all go to `report` at the end.
    let mut probs: Vec<(Option<std::ops::Range<usize>>, String)> = Vec::new();
    out.core_bad = parse_arcade_into(item, &mut out, &mut probs);
    for (span, m) in probs {
        report(span, m);
    }
    out
}

/// The body of [`parse_arcade`]. Returns whether there is a core problem.
fn parse_arcade_into(
    item: &toml_edit::Item,
    out: &mut ArcadeParsed,
    probs: &mut Vec<(Option<std::ops::Range<usize>>, String)>,
) -> bool {
    let Some(table) = item.as_table_like() else {
        probs.push((item.span(), "`arcade` must be a table: [arcade]".into()));
        return true;
    };
    let mut core = 0usize;
    for (k, _) in table.iter() {
        if !["cloudfield", "row", "layout", "layouts"].contains(&k) {
            let span = table.get_key_value(k).and_then(|(key, _)| key.span());
            probs.push((
                span,
                format!("unknown key `arcade.{k}` (expected cloudfield, row, layout or layouts)"),
            ));
            core += 1;
        }
    }
    // `cloudfield` is read with any value and ignored with a note: the
    // arcade carries no backdrop.
    out.cloudfield = table.get("cloudfield").map(|v| v.as_bool().unwrap_or(true));
    if let Some(row_item) = table.get("row") {
        let before = probs.len();
        out.rows = parse_rows(row_item, "arcade.row", &mut |s, m| probs.push((s, m)));
        core += probs.len() - before;
    }
    // The start choice. `[[arcade.layout]]` (singular) is a likely slip for
    // `[[arcade.layouts]]`, so it gets its own message.
    if let Some(v) = table.get("layout") {
        match v.as_str() {
            Some(name) => out.start = Some((name.to_string(), v.span())),
            None if v.is_array_of_tables() => probs.push((
                v.as_array_of_tables()
                    .and_then(|a| a.iter().next())
                    .and_then(|t| t.span())
                    .or(v.span()),
                "`arcade.layout` names the layout to start with, such as layout = \"deck\"; write your own layouts as [[arcade.layouts]]".into(),
            )),
            None => probs.push((
                v.span(),
                "`arcade.layout` must be a layout name, such as layout = \"deck\"".into(),
            )),
        }
    }
    if let Some(v) = table.get("layouts") {
        out.layouts = Some(parse_layouts(v, probs, &mut out.notes));
    }
    core > 0
}

/// The `[[arcade.layouts]]` tables that have no problem, in file order.
/// Every problem goes to `probs`; a layout with one is left out.
fn parse_layouts(
    item: &toml_edit::Item,
    probs: &mut Vec<(Option<std::ops::Range<usize>>, String)>,
    notes: &mut Vec<(Option<std::ops::Range<usize>>, String)>,
) -> Vec<crate::animation::arcade_presets::NamedLayout> {
    use crate::animation::arcade_presets::{
        is_reserved, name_problem, preset, NamedLayout, CONFIG_LAYOUT, MAX_DESCRIPTION,
    };
    let Some(tables) = tables_of(item) else {
        probs.push((
            item.span(),
            "`arcade.layouts` must be a list of layouts: [[arcade.layouts]]".into(),
        ));
        return Vec::new();
    };
    let mut out = Vec::new();
    // Every name given, valid or not, so a second one is reported.
    let mut names: Vec<String> = Vec::new();
    for (i, (t, span)) in tables.into_iter().enumerate() {
        let n = i + 1;
        let before = probs.len();
        for (k, _) in t.iter() {
            if !["name", "description", "row", "backdrop", "cloudfield"].contains(&k) {
                let ks = t.get_key_value(k).and_then(|(key, _)| key.span());
                probs.push((
                    ks,
                    format!(
                        "unknown key `{k}` in layout {n} (expected name, description, backdrop or row)"
                    ),
                ));
            }
        }
        let name = match t.get("name") {
            None => {
                probs.push((span.clone(), format!("layout {n} has no `name`")));
                None
            }
            Some(v) => match v.as_str() {
                None => {
                    probs.push((v.span(), format!("layout {n}: `name` must be a string")));
                    None
                }
                Some(name) => {
                    let problem = if let Some(p) = name_problem(name) {
                        Some(p)
                    } else if name.eq_ignore_ascii_case(CONFIG_LAYOUT) {
                        Some("is kept for the [[arcade.row]] tables".to_string())
                    } else if is_reserved(name) {
                        Some("is a word of /arcade layout; choose another".to_string())
                    } else if preset(name).is_some() {
                        Some("is a built-in layout; choose another".to_string())
                    } else if names.iter().any(|m| m.eq_ignore_ascii_case(name)) {
                        Some("is used twice; each layout needs its own name".to_string())
                    } else {
                        None
                    };
                    match problem {
                        Some(p) => {
                            probs.push((
                                v.span(),
                                format!(
                                    "layout {n}: the name `{}` {p}",
                                    crate::animation::arcade_presets::shown(name)
                                ),
                            ));
                            None
                        }
                        None => Some(name.to_string()),
                    }
                }
            },
        };
        // Messages about the rest name the layout when its name is good.
        let ctx = match &name {
            Some(name) => format!("layout `{name}`"),
            None => format!("layout {n}"),
        };
        let description = match t.get("description") {
            None => String::new(),
            Some(v) => match v.as_str() {
                None => {
                    probs.push((v.span(), format!("{ctx}: `description` must be a string")));
                    String::new()
                }
                Some(d) => {
                    if d.chars().count() > MAX_DESCRIPTION {
                        probs.push((
                            v.span(),
                            format!(
                                "{ctx}: the description is longer than {MAX_DESCRIPTION} characters"
                            ),
                        ));
                    } else if d.chars().any(|c| c.is_control() || is_format_char(c)) {
                        probs.push((
                            v.span(),
                            format!("{ctx}: the description has a control character"),
                        ));
                    }
                    d.to_string()
                }
            },
        };
        // `backdrop` and the older `cloudfield` are still read, and ignored
        // with a note: the arcade carries no backdrop.
        let mut cloudfield_note = None;
        for key in ["backdrop", "cloudfield"] {
            if let Some((k, _)) = t.get_key_value(key) {
                cloudfield_note.get_or_insert((
                    k.span(),
                    format!(
                        "{ctx}: `{key}` is ignored: {}",
                        crate::animation::arcade_layout::NO_BACKDROP_NOTE
                    ),
                ));
            }
        }
        let rows = match t.get("row") {
            None => {
                probs.push((
                    span.clone(),
                    format!("{ctx} has no rows: give it at least one [[arcade.layouts.row]]"),
                ));
                None
            }
            Some(r) => parse_rows(r, "arcade.layouts.row", &mut |s, m| {
                probs.push((s, format!("{ctx}: {m}")))
            }),
        };
        if probs.len() == before {
            if let (Some(name), Some(rows)) = (name, rows) {
                let name_kept = name.clone();
                out.push(NamedLayout {
                    name,
                    description,
                    rows,
                });
                notes.extend(cloudfield_note);
                // Only a kept layout takes its name, so a broken layout
                // cannot make a good one with the same name a duplicate.
                names.push(name_kept);
            }
        }
    }
    out
}

/// Whether `c` is an invisible format character (Unicode category Cf),
/// such as a bidirectional override, which can make a description show
/// text in another order than it is written.
fn is_format_char(c: char) -> bool {
    matches!(
        c,
        '\u{00AD}'
            | '\u{061C}'
            | '\u{180E}'
            | '\u{200B}'..='\u{200F}'
            | '\u{202A}'..='\u{202E}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206F}'
            | '\u{FEFF}'
            | '\u{FFF9}'..='\u{FFFB}'
    )
}

/// A table and where it is.
type Spanned<'a> = (&'a dyn toml_edit::TableLike, Option<std::ops::Range<usize>>);

/// The tables of an array of tables (`[[x]]`) or of an array of inline
/// tables (`x = [{ ... }]`). `None` when `item` is neither kind of array.
/// An array entry that is not a table is left out; [`parse_rows`] reports it.
fn tables_of(item: &toml_edit::Item) -> Option<Vec<Spanned<'_>>> {
    match item {
        toml_edit::Item::ArrayOfTables(a) => Some(
            a.iter()
                .map(|t| (t as &dyn toml_edit::TableLike, t.span()))
                .collect(),
        ),
        _ => item.as_array().map(|arr| {
            arr.iter()
                .filter_map(|e| {
                    e.as_inline_table()
                        .map(|t| (t as &dyn toml_edit::TableLike, e.span()))
                })
                .collect()
        }),
    }
}

/// The rows of a `[[<key>]]` list (`arcade.row`, or `arcade.layouts.row`
/// inside a layout). `None` when the list is not a list or has no rows.
/// Every problem goes to `report`; a row or panel with one is left out.
fn parse_rows(
    row_item: &toml_edit::Item,
    key: &str,
    report: &mut Report<'_>,
) -> Option<Vec<crate::animation::arcade_layout::RowSpec>> {
    use crate::animation::arcade_layout::{PanelSpec, RowSpec, View};
    let row_tables: Vec<Spanned<'_>> = match row_item {
        toml_edit::Item::ArrayOfTables(a) => a
            .iter()
            .map(|t| (t as &dyn toml_edit::TableLike, t.span()))
            .collect(),
        _ => match row_item.as_array() {
            Some(arr) => {
                let mut v = Vec::new();
                for e in arr.iter() {
                    match e.as_inline_table() {
                        Some(t) => v.push((t as &dyn toml_edit::TableLike, e.span())),
                        None => report(e.span(), format!("each `{key}` must be a table")),
                    }
                }
                v
            }
            None => {
                report(
                    row_item.span(),
                    format!("`{key}` must be a list of rows: [[{key}]]"),
                );
                return None;
            }
        },
    };
    if row_tables.is_empty() {
        report(row_item.span(), format!("`{key}` has no rows"));
        return None;
    }
    let mut rows = Vec::new();
    let mut seen: Vec<View> = Vec::new();
    for (ri, (row, row_span)) in row_tables.into_iter().enumerate() {
        let n = ri + 1;
        for (k, _) in row.iter() {
            if !["height", "panels"].contains(&k) {
                let span = row.get_key_value(k).and_then(|(key, _)| key.span());
                report(
                    span,
                    format!("unknown key `{k}` in row {n} (expected height or panels)"),
                );
            }
        }
        let height = weight(row.get("height"), &format!("row {n} height"), report);
        let mut panels = Vec::new();
        let items: Vec<(&dyn toml_edit::TableLike, Option<std::ops::Range<usize>>)> = match row
            .get("panels")
        {
            None => Vec::new(),
            Some(toml_edit::Item::ArrayOfTables(a)) => a
                .iter()
                .map(|t| (t as &dyn toml_edit::TableLike, t.span()))
                .collect(),
            Some(p) => match p.as_array() {
                Some(arr) => {
                    let mut v = Vec::new();
                    for e in arr.iter() {
                        match e.as_inline_table() {
                                Some(t) => v.push((t as &dyn toml_edit::TableLike, e.span())),
                                None => report(
                                    e.span(),
                                    format!(
                                        "row {n}: each panel must be a table such as {{ view = \"castle\" }}"
                                    ),
                                ),
                            }
                    }
                    v
                }
                None => {
                    report(
                        p.span(),
                        format!("row {n}: `panels` must be a list of panels"),
                    );
                    Vec::new()
                }
            },
        };
        if items.is_empty() {
            report(
                row.get("panels").and_then(|p| p.span()).or(row_span),
                format!("row {n} is empty: give it at least one panel"),
            );
        }
        for (pi, (panel, panel_span)) in items.into_iter().enumerate() {
            let m = pi + 1;
            for (k, _) in panel.iter() {
                if !["view", "width"].contains(&k) {
                    let span = panel.get_key_value(k).and_then(|(key, _)| key.span());
                    report(
                        span,
                        format!("unknown key `{k}` in row {n} panel {m} (expected view or width)"),
                    );
                }
            }
            let width = weight(
                panel.get("width"),
                &format!("row {n} panel {m} width"),
                report,
            );
            let view = match panel.get("view") {
                None => {
                    report(panel_span, format!("row {n} panel {m} has no `view`"));
                    None
                }
                Some(v) => match v.as_str() {
                    None => {
                        report(
                            v.span(),
                            format!("row {n} panel {m}: `view` must be a string"),
                        );
                        None
                    }
                    Some(name) => match View::from_key(name) {
                        Some(view) => {
                            if seen.contains(&view) {
                                report(
                                    v.span(),
                                    format!(
                                        "view `{}` appears twice; each view can be shown once",
                                        view.key()
                                    ),
                                );
                            }
                            seen.push(view);
                            Some(view)
                        }
                        None => {
                            report(
                                v.span(),
                                format!(
                                    "unknown view `{name}` (expected one of {})",
                                    View::ALL.map(|v| v.key()).join(", ")
                                ),
                            );
                            None
                        }
                    },
                },
            };
            if let (Some(view), Some(width)) = (view, width) {
                panels.push(PanelSpec { view, width });
            }
        }
        if let Some(height) = height {
            rows.push(RowSpec { height, panels });
        }
    }
    Some(rows)
}

/// The `[backdrop]` table: `default` and `[backdrop.views]`, into `cfg`. A
/// bad entry is reported and left out, so it keeps its default; the other
/// entries are used.
///
/// An entry for a view that takes no backdrop is a note in `notes` and is
/// ignored.
fn parse_backdrop(
    item: &toml_edit::Item,
    cfg: &mut crate::animation::backdrop::BackdropConfig,
    report: &mut Report<'_>,
    notes: &mut Vec<(Option<std::ops::Range<usize>>, String)>,
) {
    use crate::animation::backdrop::{
        canonical_view, parse_choice, refusal, takes_backdrop, Choice, CHOICES, NO_BACKDROP, VIEWS,
    };
    let Some(table) = item.as_table_like() else {
        report(item.span(), "`backdrop` must be a table: [backdrop]".into());
        return;
    };
    // A string value that names a choice, or a report saying why not.
    let choice = |v: &toml_edit::Item, what: &str, report: &mut Report<'_>| -> Option<Choice> {
        match v.as_str() {
            None => {
                report(v.span(), format!("`{what}` must be {CHOICES}"));
                None
            }
            Some(s) => {
                let c = parse_choice(s);
                if c.is_none() {
                    report(v.span(), format!("`{what}` must be {CHOICES}, got {s:?}"));
                }
                c
            }
        }
    };
    for (k, v) in table.iter() {
        match k {
            "default" => {
                if let Some(c) = choice(v, "backdrop.default", report) {
                    cfg.default = Some(c);
                }
            }
            "views" => {
                let Some(views) = v.as_table_like() else {
                    report(
                        v.span(),
                        "`backdrop.views` must be a table: [backdrop.views]".into(),
                    );
                    continue;
                };
                for (name, value) in views.iter() {
                    let key_span = views.get_key_value(name).and_then(|(key, _)| key.span());
                    let Some(view) = canonical_view(name) else {
                        report(
                            key_span,
                            format!(
                                "unknown view `{name}` in [backdrop.views] (expected one of {})",
                                VIEWS.join(", ")
                            ),
                        );
                        continue;
                    };
                    let Some(c) = choice(value, &format!("backdrop.views.{name}"), report) else {
                        continue;
                    };
                    if !takes_backdrop(view) {
                        notes.push((
                            key_span,
                            format!("[backdrop.views] {view} is ignored: {NO_BACKDROP}"),
                        ));
                        continue;
                    }
                    if let Some(why) = c.and_then(|kind| refusal(view, kind)) {
                        report(
                            value.span(),
                            format!(
                                "{view} refuses {}: {why}",
                                crate::animation::backdrop::choice_name(c)
                            ),
                        );
                        continue;
                    }
                    if cfg.views.iter().any(|(v, _)| *v == view) {
                        report(
                            key_span,
                            format!("view `{view}` is set twice in [backdrop.views]"),
                        );
                        continue;
                    }
                    cfg.views.push((view, c));
                }
            }
            _ => {
                let span = table.get_key_value(k).and_then(|(key, _)| key.span());
                report(
                    span,
                    format!("unknown key `backdrop.{k}` (expected default or views)"),
                );
            }
        }
    }
}

/// The `[saga]` table, read into `saga` key by key. A key that is set and
/// valid replaces the value already there (a later file wins); a key with a
/// problem keeps it. Every problem goes to `report`.
fn parse_saga(item: &toml_edit::Item, saga: &mut SagaSettings, report: &mut Report<'_>) {
    let Some(table) = item.as_table_like() else {
        report(item.span(), "`saga` must be a table: [saga]".into());
        return;
    };
    for (k, _) in table.iter() {
        if !["enabled", "names", "season_minutes"].contains(&k) {
            let span = table.get_key_value(k).and_then(|(key, _)| key.span());
            report(
                span,
                format!("unknown key `saga.{k}` (expected enabled, names or season_minutes)"),
            );
        }
    }
    if let Some(v) = table.get("enabled") {
        match v.as_bool() {
            Some(b) => saga.enabled = b,
            None => report(
                v.span(),
                "`saga.enabled` must be true or false; the saga stays on".into(),
            ),
        }
    }
    // A bad `names` value hides names. A person who set the key meant to
    // choose, and the usual reason to choose is to keep real names off a
    // shared screen, so a typo such as "generate" must not show them
    // (final review finding M1). Every other bad key keeps its default.
    if let Some(v) = table.get("names") {
        match v.as_str() {
            Some("real") => saga.names = SagaNames::Real,
            Some("generated") => saga.names = SagaNames::Generated,
            Some(other) => {
                saga.names = SagaNames::Generated;
                report(
                    v.span(),
                    format!(
                        "unknown saga.names {other:?} (expected \"real\" or \"generated\"); the saga uses generated names"
                    ),
                )
            }
            None => {
                saga.names = SagaNames::Generated;
                report(
                    v.span(),
                    "`saga.names` must be \"real\" or \"generated\"; the saga uses generated names"
                        .into(),
                )
            }
        }
    }
    if let Some(v) = table.get("season_minutes") {
        let range = SAGA_SEASON_MIN..=SAGA_SEASON_MAX;
        match v.as_integer() {
            Some(n) if (i64::from(SAGA_SEASON_MIN)..=i64::from(SAGA_SEASON_MAX)).contains(&n) => {
                // In range, so the cast cannot truncate.
                saga.season_minutes = n as u32;
            }
            _ => report(
                v.span(),
                format!(
                    "`saga.season_minutes` must be a whole number from {} to {}; the saga uses {SAGA_SEASON_DEFAULT}",
                    range.start(),
                    range.end()
                ),
            ),
        }
    }
}

/// The `[viz]` table, read into `viz`. Structural problems only: `enable`
/// must be an array of kebab-case id strings, every other key must be a
/// `[viz.<id>]` table with a valid id. A bad entry is skipped and the rest
/// kept. Unknown-but-well-formed ids are not a problem here (the registry
/// is empty at parse time); see [`VizSettings::unknown_enabled`].
///
/// A valid `enable` replaces the list from an earlier file; a `[viz.<id>]`
/// table is merged into the earlier file's table key by key.
fn parse_viz(item: &toml_edit::Item, viz: &mut VizSettings, report: &mut Report<'_>) {
    let Some(table) = item.as_table_like() else {
        report(item.span(), "`viz` must be a table: [viz]".into());
        return;
    };
    for (k, v) in table.iter() {
        let key_span = table.get_key_value(k).and_then(|(key, _)| key.span());
        if k == "enable" {
            let Some(arr) = v.as_array() else {
                report(
                    v.span(),
                    "`viz.enable` must be an array of viz ids: enable = [\"example-pulse\"]".into(),
                );
                continue;
            };
            let mut ids = Vec::new();
            for e in arr.iter() {
                match e.as_str() {
                    Some(id) if is_valid_viz_id(id) => ids.push(id.to_string()),
                    Some(id) => report(
                        e.span(),
                        format!(
                            "`viz.enable` entry {id:?} is not a viz id (lower-case letters, digits and single hyphens, such as \"example-pulse\"); it is skipped"
                        ),
                    ),
                    None => report(
                        e.span(),
                        "`viz.enable` entries must be strings (viz ids); this one is skipped"
                            .into(),
                    ),
                }
            }
            viz.enable = ids;
        } else if let Some(t) = v.as_table_like() {
            if !is_valid_viz_id(k) {
                report(
                    key_span,
                    format!("`[viz.{k}]` is not a viz id (lower-case letters, digits and single hyphens); the table is ignored"),
                );
                continue;
            }
            let dst = viz.per_viz.entry(k.to_string()).or_default();
            for (tk, tv) in t.iter() {
                dst.insert(tk, tv.clone());
            }
        } else {
            report(
                key_span,
                format!("unknown key `viz.{k}` (expected enable, or a [viz.<id>] table)"),
            );
        }
    }
}

/// A height or width weight: a whole number of at least 1, 1 when absent.
fn weight(item: Option<&toml_edit::Item>, what: &str, report: &mut Report<'_>) -> Option<u32> {
    let Some(item) = item else {
        return Some(1);
    };
    match item.as_integer() {
        Some(n) if (1..=1000).contains(&n) => Some(n as u32),
        Some(n) => {
            report(
                item.span(),
                format!("{what} must be a whole number from 1 to 1000, got {n}"),
            );
            None
        }
        None => {
            report(
                item.span(),
                format!("{what} must be a whole number from 1 to 1000"),
            );
            None
        }
    }
}

/// Parses a `tt_smi_reset_behavior` config value. Case-insensitive; an
/// unknown value logs a warning and returns `None` so the caller falls back
/// to its default.
pub fn parse_reset_behavior(text: &str) -> Option<crate::cli::ResetBehavior> {
    use clap::ValueEnum;
    let parsed = crate::cli::ResetBehavior::from_str(text.trim(), true).ok();
    if parsed.is_none() {
        log::warn!(
            "config: unknown tt_smi_reset_behavior {text:?} (expected ignore, inform, dazzle or demo); using inform"
        );
    }
    parsed
}

/// The shared config file, `~/.config/tt-toplike/config.toml` (the
/// platform's config directory).
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("tt-toplike").join("config.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normal_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        assert!((cfg.sensitivity - 1.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_paranoid_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        assert!((cfg.sensitivity - 5.0).abs() < f32::EPSILON);
    }

    #[test]
    fn test_anomalies_only_sensitivity() {
        let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
        assert!((cfg.sensitivity - 0.2).abs() < f32::EPSILON);
    }

    #[test]
    fn test_max_particles_normal() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        // Normal: 600 castle, 200 flow
        assert_eq!(cfg.castle_max_particles(), 600);
        assert_eq!(cfg.flow_max_particles(), 200);
    }

    #[test]
    fn test_max_particles_paranoid() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        // 3× scale → 1800 castle, 600 flow
        assert_eq!(cfg.castle_max_particles(), 1800);
        assert_eq!(cfg.flow_max_particles(), 600);
    }

    #[test]
    fn test_toml_override_sensitivity() {
        let toml = r#"sensitivity = 2.5"#;
        let overrides: AnimConfigOverrides = toml::from_str(toml).unwrap();
        let base = AnimConfig::from_profile(AnimationProfile::Normal);
        let merged = base.merge(overrides);
        assert!((merged.sensitivity - 2.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_portrait_max_particles_normal() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Normal);
        // 32 * 1.0 = 32
        assert_eq!(cfg.portrait_max_particles(), 32);
    }

    #[test]
    fn test_portrait_max_particles_paranoid() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Paranoid);
        // 32 * 3.0 = 96
        assert_eq!(cfg.portrait_max_particles(), 96);
    }

    #[test]
    fn test_toml_override_max_particles_scale() {
        let toml = r#"max_particles_scale = 1.5"#;
        let overrides: AnimConfigOverrides = toml::from_str(toml).unwrap();
        let base = AnimConfig::from_profile(AnimationProfile::Normal);
        let merged = base.merge(overrides);
        // 600 * 1.5 = 900
        assert_eq!(merged.castle_max_particles(), 900);
        assert!((merged.max_particles_scale - 1.5).abs() < f32::EPSILON);
    }

    #[test]
    fn test_anomalies_only_castle_particles() {
        let cfg = AnimConfig::from_profile(AnimationProfile::AnomaliesOnly);
        // 600 * 0.5 = 300
        assert_eq!(cfg.castle_max_particles(), 300);
    }

    #[test]
    fn test_hyper_flow_particles() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Hyper);
        // 200 * 2.0 = 400
        assert_eq!(cfg.flow_max_particles(), 400);
    }

    #[test]
    fn test_relaxed_portrait_particles() {
        let cfg = AnimConfig::from_profile(AnimationProfile::Relaxed);
        // 32 * 0.75 = 24
        assert_eq!(cfg.portrait_max_particles(), 24);
    }

    #[test]
    fn reset_behavior_names_parse_case_insensitively() {
        use crate::cli::ResetBehavior::*;
        for (txt, want) in [
            ("ignore", Ignore),
            ("inform", Inform),
            ("dazzle", Dazzle),
            ("demo", Demo),
            ("Dazzle", Dazzle),
            ("INFORM", Inform),
            ("  demo ", Demo),
        ] {
            assert_eq!(parse_reset_behavior(txt), Some(want), "{txt:?}");
        }
    }

    #[test]
    fn reset_behavior_unknown_name_is_none() {
        assert_eq!(parse_reset_behavior("sparkle"), None);
        assert_eq!(parse_reset_behavior(""), None);
    }

    #[test]
    fn reset_behavior_config_key_loads_from_toml() {
        let o: AnimConfigOverrides = toml::from_str(r#"tt_smi_reset_behavior = "Dazzle""#).unwrap();
        assert_eq!(o.tt_smi_reset_behavior.as_deref(), Some("Dazzle"));
        // The removed key is ignored and does not break the file.
        let o: AnimConfigOverrides = toml::from_str("reset_takeover = true").unwrap();
        assert_eq!(o.tt_smi_reset_behavior, None);
    }

    // ---- Loading and validation ------------------------------------------

    use crate::animation::arcade_layout::{default_rows, PanelSpec, View};

    fn load(files: &[(&str, &str)]) -> LoadedConfig {
        let sources: Vec<(PathBuf, String)> = files
            .iter()
            .map(|(p, t)| (PathBuf::from(p), t.to_string()))
            .collect();
        load_from(&sources)
    }

    /// The problems as `file:line: message` strings.
    fn problems(files: &[(&str, &str)]) -> Vec<String> {
        load(files).problems.iter().map(|p| p.to_string()).collect()
    }

    const BRIEF: &str = r#"
[arcade]
cloudfield = true

[[arcade.row]]
height = 2
panels = [{ view = "starfield" }]

[[arcade.row]]
height = 3
panels = [
  { view = "defrag", width = 3 },
  { view = "castle", width = 1 },
  { view = "waffler",  width = 2 },
]

[[arcade.row]]
height = 2
panels = [{ view = "flow" }, { view = "training" }, { view = "inference" }]
"#;

    #[test]
    fn no_file_and_no_arcade_section_give_the_default_layout() {
        let l = load(&[]);
        assert_eq!(l.arcade.rows, default_rows());
        assert!(l.notes.is_empty());
        let l = load(&[("c.toml", "sensitivity = 2.0")]);
        assert_eq!(l.arcade.rows, default_rows());
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.overrides.sensitivity, Some(2.0));
    }

    #[test]
    fn the_example_from_the_brief_loads() {
        let l = load(&[("c.toml", BRIEF)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert!(
            l.notes[0]
                .message
                .ends_with("the arcade carries no backdrop"),
            "{:?}",
            l.notes
        );
        assert_eq!(l.arcade.rows.len(), 3);
        assert_eq!(l.arcade.rows[1].height, 3);
        assert_eq!(
            l.arcade.rows[1].panels,
            vec![
                PanelSpec {
                    view: View::Defrag,
                    width: 3
                },
                PanelSpec {
                    view: View::Castle,
                    width: 1
                },
                PanelSpec {
                    view: View::Die3d,
                    width: 2
                },
            ]
        );
        assert_eq!(l.arcade.rows[2].panels[0].width, 1, "width defaults to 1");
    }

    #[test]
    fn rows_written_as_arrays_of_tables_load_too() {
        let text = "[[arcade.row]]\n[[arcade.row.panels]]\nview = \"castle\"\nwidth = 2\n";
        let l = load(&[("c.toml", text)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.arcade.rows[0].height, 1, "height defaults to 1");
        assert_eq!(l.arcade.rows[0].panels[0].width, 2);
    }

    #[test]
    fn an_unknown_view_is_reported_with_its_line() {
        let text = "[[arcade.row]]\npanels = [{ view = \"castel\" }]\n";
        let l = load(&[("c.toml", text)]);
        assert_eq!(l.arcade.rows, default_rows(), "a problem keeps the default");
        let p = problems(&[("c.toml", text)]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("c.toml:2: unknown view `castel`"), "{p:?}");
        assert!(p[0].contains("starfield, castle"), "{p:?}");
        assert!(
            p[0].ends_with("training, inference, saga)"),
            "the list names saga: {p:?}"
        );
    }

    /// `view = "saga"` loads in `[[arcade.row]]` and in a layout of its
    /// own, `--check-config` prints it by its key, and a second Saga panel
    /// is reported like any view shown twice.
    #[test]
    fn a_saga_panel_loads_and_check_config_names_it() {
        let text = "[[arcade.row]]\npanels = [{ view = \"saga\", width = 2 }, { view = \"castle\" }]\n\n[[arcade.layouts]]\nname = \"quest\"\n\n[[arcade.layouts.row]]\npanels = [{ view = \"Saga\" }]\n";
        let mut l = load(&[("c.toml", text)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(
            l.arcade.rows[0].panels[0],
            PanelSpec {
                view: View::Saga,
                width: 2
            }
        );
        choose_layout(&mut l, Some("quest"));
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("row 1 (height 1): saga x2, castle x1\n"),
            "{out}"
        );
        assert!(out.contains("\narcade starts with quest: 1 row\n"), "{out}");
        let twice = "[[arcade.row]]\npanels = [{ view = \"saga\" }, { view = \"saga\" }]\n";
        assert_eq!(
            problems(&[("c.toml", twice)]),
            vec!["c.toml:2: view `saga` appears twice; each view can be shown once"]
        );
    }

    #[test]
    fn zero_and_negative_weights_are_reported() {
        let text = "[[arcade.row]]\nheight = 0\npanels = [{ view = \"castle\", width = -2 }]\n";
        let p = problems(&[("c.toml", text)]);
        assert_eq!(p.len(), 2, "{p:?}");
        assert_eq!(
            p[0],
            "c.toml:2: row 1 height must be a whole number from 1 to 1000, got 0"
        );
        assert_eq!(
            p[1],
            "c.toml:3: row 1 panel 1 width must be a whole number from 1 to 1000, got -2"
        );
        let p = problems(&[(
            "c.toml",
            "[[arcade.row]]\nheight = 1.5\npanels = [{ view = \"flow\" }]",
        )]);
        assert_eq!(
            p,
            vec!["c.toml:2: row 1 height must be a whole number from 1 to 1000"]
        );
    }

    #[test]
    fn an_empty_row_is_reported() {
        let p = problems(&[("c.toml", "[[arcade.row]]\nheight = 2\npanels = []\n")]);
        assert_eq!(
            p,
            vec!["c.toml:3: row 1 is empty: give it at least one panel"]
        );
        let p = problems(&[("c.toml", "[[arcade.row]]\nheight = 2\n")]);
        assert_eq!(
            p,
            vec!["c.toml:1: row 1 is empty: give it at least one panel"]
        );
    }

    #[test]
    fn unknown_keys_are_reported_at_every_level() {
        let text = "colour = 1\n[arcade]\nclouds = true\n[[arcade.row]]\nheigth = 2\npanels = [{ view = \"flow\", wide = 2 }]\n";
        let p = problems(&[("c.toml", text)]);
        assert_eq!(
            p,
            vec![
                "c.toml:1: unknown key `colour`",
                "c.toml:3: unknown key `arcade.clouds` (expected cloudfield, row, layout or layouts)",
                "c.toml:5: unknown key `heigth` in row 1 (expected height or panels)",
                "c.toml:6: unknown key `wide` in row 1 panel 1 (expected view or width)",
            ]
        );
    }

    #[test]
    fn a_syntax_error_is_reported_with_its_line_and_keeps_every_default() {
        let text = "sensitivity = 2.0\n[arcade\ncloudfield = true\n";
        let l = load(&[("c.toml", text)]);
        assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        assert_eq!(l.problems[0].line, Some(2));
        assert!(
            l.problems[0].message.starts_with("TOML syntax error"),
            "{:?}",
            l.problems
        );
        assert_eq!(l.overrides.sensitivity, None, "a broken file gives nothing");
        assert_eq!(l.arcade.rows, default_rows());
    }

    #[test]
    fn a_wrong_type_for_an_existing_key_is_reported() {
        let p = problems(&[("c.toml", "sensitivity = \"high\"\n")]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].starts_with("c.toml:1: "), "{p:?}");
    }

    #[test]
    fn a_view_shown_twice_is_reported() {
        let text = "[[arcade.row]]\npanels = [{ view = \"flow\" }, { view = \"flow\" }]\n";
        let p = problems(&[("c.toml", text)]);
        assert_eq!(
            p,
            vec!["c.toml:2: view `flow` appears twice; each view can be shown once"]
        );
    }

    #[test]
    fn any_cloudfield_value_is_a_note() {
        for v in ["true", "false", "\"yes\""] {
            let l = load(&[("c.toml", &format!("[arcade]\ncloudfield = {v}\n"))]);
            assert!(l.problems.is_empty(), "{v}: {:?}", l.problems);
            let n: Vec<String> = l.notes.iter().map(|p| p.to_string()).collect();
            assert_eq!(
                n,
                vec!["c.toml:2: `arcade.cloudfield` is ignored: the arcade carries no backdrop"],
                "{v}"
            );
        }
    }

    #[test]
    fn every_problem_in_a_file_is_reported_not_only_the_first() {
        let text = "[[arcade.row]]\nheight = 0\npanels = [{ view = \"nope\" }]\n[[arcade.row]]\npanels = []\n";
        assert_eq!(problems(&[("c.toml", text)]).len(), 3);
    }

    #[test]
    fn check_config_prints_every_problem_and_exits_non_zero() {
        let text = "[[arcade.row]]\nheight = 0\npanels = [{ view = \"nope\" }]\n";
        let l = load(&[("c.toml", text)]);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 1);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("c.toml:2: row 1 height"), "{out}");
        assert!(out.contains("c.toml:3: unknown view `nope`"), "{out}");
        assert!(
            out.contains("\narcade starts with classic: 3 rows\n"),
            "{out}"
        );
        let l = load(&[("c.toml", "[arcade]\ncloudfield = true\n")]);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.ends_with("ok\n") && out.contains("\narcade starts with classic: 3 rows\n"),
            "{out}"
        );
    }

    /// The report's arcade line is the layout the arcade starts with and
    /// its rows. The arcade carries no backdrop, so no setting gives it one,
    /// and no line describes only the `[[arcade.row]]` tables or names the
    /// cloudfield (re-review M1).
    #[test]
    fn check_config_says_how_the_arcade_starts() {
        let report = |l: &LoadedConfig| {
            let mut out = Vec::new();
            write_check_report(l, &mut out);
            String::from_utf8(out).unwrap()
        };
        let mine = "[arcade]\nlayout = \"mine\"\n[[arcade.layouts]]\nname = \"mine\"\nbackdrop = \"clouds\"\n[[arcade.layouts.row]]\npanels = [{ view = \"castle\" }]\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n";
        for (text, want) in [
            (mine, "arcade starts with mine: 2 rows"),
            (
                "[arcade]\ncloudfield = true\n[backdrop.views]\narcade = \"sky\"\n",
                "arcade starts with classic: 3 rows",
            ),
            (
                "[backdrop]\ndefault = \"clouds\"\n",
                "arcade starts with classic: 3 rows",
            ),
            (
                "[[arcade.row]]\npanels = [{ view = \"castle\" }]\n",
                "arcade starts with config: 1 row",
            ),
        ] {
            let mut l = load(&[("c.toml", text)]);
            choose_layout(&mut l, None);
            let out = report(&l);
            assert!(out.contains(&format!("\n{want}\n")), "{text}\n{out}");
            assert!(!out.contains("\narcade: "), "{text}\n{out}");
            assert!(!out.contains("cloudfield o"), "{text}\n{out}");
            assert!(!out.contains("when selected"), "{text}\n{out}");
        }
    }

    #[test]
    fn the_status_line_names_the_first_problem_and_counts_the_rest() {
        let l = load(&[("c.toml", "a = 1\nb = 2\n")]);
        assert_eq!(
            problem_summary(&l.problems).unwrap(),
            "config: c.toml:1: unknown key `a` (and 1 more; run --check-config)"
        );
        assert_eq!(problem_summary(&[]), None);
    }

    #[test]
    fn a_problem_names_the_file_it_is_in() {
        let p = problems(&[("/x/config.toml", "[arcade]\nnope = 1\n")]);
        assert_eq!(
            p,
            vec!["/x/config.toml:2: unknown key `arcade.nope` (expected cloudfield, row, layout or layouts)"]
        );
    }

    // ---- `[viz]` ----

    /// `[viz] enable` is a list of ids, kept in file order.
    #[test]
    fn viz_enable_list_parses() {
        let l = load(&[(
            "c.toml",
            "[viz]\nenable = [\"example-pulse\", \"thermal-board\"]\n",
        )]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.viz.enable, vec!["example-pulse", "thermal-board"]);
    }

    /// The registry is empty when the config is parsed (YAML files are
    /// scanned later), so a well-formed id nobody has registered is no
    /// problem here. `unknown_enabled` finds it once the scan is done.
    #[test]
    fn viz_enable_unknown_id_is_a_config_problem_not_a_failure() {
        let l = load(&[("c.toml", "[viz]\nenable = [\"example-pulse\", \"nope\"]\n")]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(
            l.viz.unknown_enabled(&|id| id == "example-pulse"),
            vec!["nope"]
        );
        assert!(l.viz.unknown_enabled(&|_| true).is_empty());
    }

    /// A `[viz.<id>]` table is kept whole for `Viz::configure`.
    #[test]
    fn viz_per_id_table_is_kept() {
        let l = load(&[(
            "c.toml",
            "[viz]\nenable = [\"a\"]\n\n[viz.example-pulse]\nrate = 3\ncolor = \"red\"\n",
        )]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        let t = &l.viz.per_viz["example-pulse"];
        assert_eq!(t["rate"].as_integer(), Some(3));
        assert_eq!(t["color"].as_str(), Some("red"));
        assert_eq!(l.viz.per_viz.len(), 1);
    }

    /// Structural problems each get a line and leave the rest of `[viz]`
    /// usable.
    #[test]
    fn bad_viz_values_are_reported_with_their_lines() {
        let text = "[viz]\nenable = [\"ok\", \"Bad_Id\", 3]\nbogus = 1\n\n[viz.Not_Valid]\nx = 1\n";
        let p = problems(&[("c.toml", text)]);
        assert_eq!(p.len(), 4, "{p:?}");
        assert!(
            p[0].starts_with("c.toml:2:") && p[0].contains("Bad_Id"),
            "{p:?}"
        );
        assert!(
            p[1].starts_with("c.toml:2:") && p[1].contains("string"),
            "{p:?}"
        );
        assert!(
            p[2].starts_with("c.toml:3:") && p[2].contains("viz.bogus"),
            "{p:?}"
        );
        assert!(
            p[3].starts_with("c.toml:5:") && p[3].contains("Not_Valid"),
            "{p:?}"
        );
        let l = load(&[("c.toml", text)]);
        assert_eq!(l.viz.enable, vec!["ok"]);
        assert!(l.viz.per_viz.is_empty());

        let p = problems(&[("c.toml", "[viz]\nenable = \"x\"\n")]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].contains("viz.enable") && p[0].contains("array"),
            "{p:?}"
        );
        let p = problems(&[("c.toml", "viz = 3\n")]);
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("`viz` must be a table"), "{p:?}");
    }

    /// Existing config files have no `[viz]` section and must behave as
    /// before: default settings, no problem, nothing in the summary.
    #[test]
    fn a_config_with_no_viz_section_is_unchanged() {
        let text = "[arcade]\ncloudfield = true\n";
        let l = load(&[("c.toml", text)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert!(l.viz.enable.is_empty());
        assert!(l.viz.per_viz.is_empty());
        let mut out = Vec::new();
        write_check_report(&l, &mut out);
        assert!(!String::from_utf8(out).unwrap().contains("viz"));
    }

    #[test]
    fn the_summary_names_enabled_viz() {
        let l = load(&[(
            "c.toml",
            "[viz]\nenable = [\"example-pulse\", \"thermal-board\"]\n",
        )]);
        let mut out = Vec::new();
        write_check_report(&l, &mut out);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("viz: enabled example-pulse, thermal-board\n"),
            "{out}"
        );
    }

    /// config.rs holds its own copy of the id rule (it compiles without the
    /// `tui` feature, where `crate::viz` does not exist). The copies must agree.
    #[cfg(feature = "tui")]
    #[test]
    fn config_and_registry_agree_on_what_a_viz_id_is() {
        for id in [
            "a",
            "example-pulse",
            "a1-b2",
            "9",
            "",
            "-",
            "a-",
            "-a",
            "a--b",
            "A",
            "a_b",
            "a b",
            "é",
            "a-B",
            "thermal-board-2",
        ] {
            assert_eq!(
                is_valid_viz_id(id),
                crate::viz::registry::is_valid_id(id),
                "disagree on {id:?}"
            );
        }
    }

    /// With no `[saga]` section the saga runs with real names and a
    /// 20-minute season (ledger ruling R24: on by default).
    #[test]
    fn no_saga_section_gives_the_saga_defaults() {
        let l = load(&[]);
        assert_eq!(
            l.saga,
            SagaSettings {
                enabled: true,
                names: SagaNames::Real,
                season_minutes: 20,
            }
        );
        let l = load(&[("c.toml", "[arcade]\ncloudfield = true\n")]);
        assert_eq!(l.saga, SagaSettings::default());
    }

    #[test]
    fn a_saga_section_sets_each_key() {
        let l = load(&[(
            "c.toml",
            "[saga]\nenabled = false\nnames = \"generated\"\nseason_minutes = 1440\n",
        )]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(
            l.saga,
            SagaSettings {
                enabled: false,
                names: SagaNames::Generated,
                season_minutes: 1440,
            }
        );
        let l = load(&[("c.toml", "[saga]\nnames = \"real\"\nseason_minutes = 1\n")]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.saga.names, SagaNames::Real);
        assert_eq!(l.saga.season_minutes, 1);
    }

    /// Every bad `[saga]` value is a problem with its line, and that key
    /// keeps its default while the good keys still apply. A bad `names` is
    /// the exception: it hides names (see `parse_saga`).
    #[test]
    fn bad_saga_values_are_reported_with_their_lines_and_keep_the_defaults() {
        let text =
            "[saga]\nenabled = \"yes\"\nnames = \"secret\"\nseason_minutes = 0\nhost = true\n";
        let l = load(&[("c.toml", text)]);
        let p: Vec<String> = l.problems.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            p,
            vec![
                "c.toml:2: `saga.enabled` must be true or false; the saga stays on",
                "c.toml:3: unknown saga.names \"secret\" (expected \"real\" or \"generated\"); the saga uses generated names",
                "c.toml:4: `saga.season_minutes` must be a whole number from 1 to 1440; the saga uses 20",
                "c.toml:5: unknown key `saga.host` (expected enabled, names or season_minutes)",
            ]
        );
        assert_eq!(
            l.saga,
            SagaSettings {
                names: SagaNames::Generated,
                ..SagaSettings::default()
            }
        );
        for bad in ["1441", "-5", "2.5", "\"20\""] {
            let l = load(&[("c.toml", &format!("[saga]\nseason_minutes = {bad}\n"))]);
            assert_eq!(l.problems.len(), 1, "{bad}: {:?}", l.problems);
            assert_eq!(l.problems[0].line, Some(2), "{bad}");
            assert_eq!(l.saga.season_minutes, 20, "{bad}");
        }
        let l = load(&[("c.toml", "[saga]\nnames = 3\nseason_minutes = 5\n")]);
        assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        assert_eq!(
            l.saga.names,
            SagaNames::Generated,
            "a bad names hides names"
        );
        assert_eq!(l.saga.season_minutes, 5, "a good key still applies");
        let p = problems(&[("c.toml", "saga = 1\n")]);
        assert_eq!(p, vec!["c.toml:1: `saga` must be a table: [saga]"]);
    }

    #[test]
    fn check_config_lists_the_saga_settings() {
        let report = |text: &str| {
            let l = load(&[("c.toml", text)]);
            let mut out = Vec::new();
            let code = write_check_report(&l, &mut out);
            (code, String::from_utf8(out).unwrap())
        };
        let (code, out) = report("");
        assert_eq!(code, 0);
        assert!(
            out.contains("\nsaga: on, names real, season 20 minutes\n"),
            "{out}"
        );
        let (_, out) = report("[saga]\nnames = \"generated\"\nseason_minutes = 1\n");
        assert!(
            out.contains("\nsaga: on, names generated, season 1 minute\n"),
            "{out}"
        );
        let (_, out) = report("[saga]\nenabled = false\n");
        assert!(
            out.contains("\nsaga: off ([saga] enabled = false)\n"),
            "{out}"
        );
        let (code, out) = report("[saga]\nseason_minutes = 0\n");
        assert_eq!(code, 1);
        assert!(
            out.contains("c.toml:2: `saga.season_minutes` must be a whole number"),
            "{out}"
        );
    }

    #[test]
    fn a_bad_tt_smi_reset_behavior_is_a_problem() {
        let text = "sensitivity = 2.0\ntt_smi_reset_behavior = \"bogus\"\n";
        let l = load(&[("c.toml", text)]);
        let p: Vec<String> = l.problems.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            p,
            vec![
                "c.toml:2: unknown tt_smi_reset_behavior \"bogus\" (expected ignore, inform, dazzle or demo); the TUI uses inform"
            ]
        );
        assert_eq!(
            l.overrides.tt_smi_reset_behavior, None,
            "the default is used"
        );
        assert_eq!(l.overrides.sensitivity, Some(2.0), "the other keys stay");
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 1, "--check-config fails");
        let l = load(&[("c.toml", "tt_smi_reset_behavior = \"Dazzle\"\n")]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.overrides.tt_smi_reset_behavior.as_deref(), Some("Dazzle"));
    }

    #[test]
    fn a_syntax_error_is_one_line() {
        // toml_edit's message for a bad table header spans two lines.
        let l = load(&[("c.toml", "[arcade\n")]);
        let m = &l.problems[0].message;
        assert!(!m.contains('\n'), "{m:?}");
        assert!(m.starts_with("TOML syntax error: "), "{m:?}");
        assert!(m.contains("; "), "the lines are joined: {m:?}");
    }

    // ---- Reading the file ------------------------------------------------

    /// A fresh directory under the system temp directory, removed on drop.
    struct TempDir(PathBuf);

    impl TempDir {
        fn new(tag: &str) -> TempDir {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static N: AtomicUsize = AtomicUsize::new(0);
            let p = std::env::temp_dir().join(format!(
                "tt-toplike-config-{tag}-{}-{}",
                std::process::id(),
                N.fetch_add(1, Ordering::Relaxed)
            ));
            let _ = std::fs::remove_dir_all(&p);
            std::fs::create_dir_all(&p).unwrap();
            TempDir(p)
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// [`read_config_file`] on a thread. Panics if it has not returned
    /// within ten seconds, so a read that blocks fails the test and never
    /// hangs it. The bound only catches a hang; nothing about speed is
    /// checked.
    fn read_or_hang(path: PathBuf) -> Result<Option<String>, String> {
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(read_config_file(&path));
        });
        rx.recv_timeout(std::time::Duration::from_secs(10))
            .expect("reading the config file blocked")
    }

    #[test]
    fn a_missing_file_is_no_file_and_a_regular_file_is_read() {
        let d = TempDir::new("plain");
        assert_eq!(read_or_hang(d.0.join("config.toml")), Ok(None));
        std::fs::write(d.0.join("config.toml"), "sensitivity = 2.0\n").unwrap();
        assert_eq!(
            read_or_hang(d.0.join("config.toml")),
            Ok(Some("sensitivity = 2.0\n".into()))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_a_regular_file_is_read() {
        let d = TempDir::new("link");
        std::fs::write(d.0.join("real.toml"), "sensitivity = 2.0\n").unwrap();
        std::os::unix::fs::symlink(d.0.join("real.toml"), d.0.join("config.toml")).unwrap();
        let l = load_config_file(Some(&d.0.join("config.toml")));
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.overrides.sensitivity, Some(2.0));
    }

    #[cfg(unix)]
    #[test]
    fn a_symlink_to_dev_zero_is_a_problem_and_is_not_read() {
        let d = TempDir::new("zero");
        let path = d.0.join("config.toml");
        std::os::unix::fs::symlink("/dev/zero", &path).unwrap();
        let e = read_or_hang(path.clone()).unwrap_err();
        assert_eq!(
            e,
            "is a character device; the config must be a regular file"
        );
        let l = load_config_file(Some(&path));
        assert_eq!(l.problems.len(), 1);
        assert!(l.files.is_empty());
        assert_eq!(
            l.arcade,
            crate::animation::arcade_layout::ArcadeConfig::default()
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_fifo_is_a_problem_and_is_never_opened() {
        let d = TempDir::new("fifo");
        let path = d.0.join("config.toml");
        let c = std::ffi::CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: `c` is a valid NUL-terminated path for the call's duration.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0, "mkfifo");
        // Opening a FIFO for reading blocks until a writer comes, so a
        // reader that opens it hangs here (and `read_or_hang` fails).
        let e = read_or_hang(path).unwrap_err();
        assert_eq!(e, "is a FIFO; the config must be a regular file");
    }

    #[test]
    fn a_directory_is_a_problem() {
        let d = TempDir::new("dir");
        std::fs::create_dir(d.0.join("config.toml")).unwrap();
        let e = read_or_hang(d.0.join("config.toml")).unwrap_err();
        assert_eq!(e, "is a directory; the config must be a regular file");
    }

    #[test]
    fn a_file_over_the_size_cap_is_a_problem() {
        let d = TempDir::new("big");
        let path = d.0.join("config.toml");
        let cap = MAX_CONFIG_BYTES as usize;
        std::fs::write(&path, "#".repeat(cap)).unwrap();
        assert!(
            read_or_hang(path.clone()).unwrap().is_some(),
            "the cap itself is read"
        );
        std::fs::write(&path, "#".repeat(cap + 1)).unwrap();
        let e = read_or_hang(path).unwrap_err();
        assert_eq!(
            e,
            format!(
                "is larger than 256 KiB ({} bytes); it was not read",
                cap + 1
            )
        );
    }

    #[test]
    fn a_file_that_is_not_utf8_is_a_problem() {
        let d = TempDir::new("utf8");
        std::fs::write(d.0.join("config.toml"), [0xff, 0xfe, b'\n']).unwrap();
        assert_eq!(
            read_or_hang(d.0.join("config.toml")).unwrap_err(),
            "is not UTF-8 text"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_file_is_reported_as_unreadable() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new("mode000");
        let path = d.0.join("config.toml");
        std::fs::write(&path, "sensitivity = 2.0\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000)).unwrap();
        if std::fs::read(&path).is_ok() {
            // Running as root: mode 000 does not stop a read.
            return;
        }
        let l = load_config_file(Some(&path));
        assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        assert!(
            l.problems[0].message.starts_with("cannot be read: "),
            "{:?}",
            l.problems
        );
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 1);
        let out = String::from_utf8(out).unwrap();
        assert!(!out.contains("no config file found"), "{out}");
        assert!(out.contains("cannot be read"), "{out}");
    }

    #[test]
    fn check_config_says_no_file_only_when_there_is_none() {
        let d = TempDir::new("none");
        let l = load_config_file(Some(&d.0.join("config.toml")));
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.starts_with("no config file found (looked for: "),
            "{out}"
        );
    }

    #[test]
    fn only_the_shared_file_is_a_config_file() {
        let files = config_files();
        assert!(files.len() <= 1, "{files:?}");
        for f in &files {
            assert!(f.ends_with("tt-toplike/config.toml"), "{f:?}");
        }
    }

    /// The child half of [`a_toplikerc_in_the_working_directory_is_ignored`]:
    /// run by that test in a directory holding a `.toplikerc`, with `HOME`
    /// and `XDG_CONFIG_HOME` in an empty directory. Does nothing otherwise.
    #[test]
    #[ignore = "run by a_toplikerc_in_the_working_directory_is_ignored"]
    fn toplikerc_probe() {
        if std::env::var_os("TT_TOPLIKE_TOPLIKERC_PROBE").is_none() {
            return;
        }
        assert!(
            std::path::Path::new(".toplikerc").exists(),
            "set up by the parent"
        );
        let l = load_config();
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert!(l.files.is_empty(), "{:?}", l.files);
        assert_eq!(l.overrides.sensitivity, None);
        assert_eq!(
            l.arcade,
            crate::animation::arcade_layout::ArcadeConfig::default()
        );
    }

    /// A `.toplikerc` in the working directory changes nothing: a repo you
    /// check out must not change tt-toplike. The working directory is
    /// process-wide, so this runs [`toplikerc_probe`] in a child process of
    /// this test binary, started in a directory with a `.toplikerc` that
    /// has a problem and a setting.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_toplikerc_in_the_working_directory_is_ignored() {
        let d = TempDir::new("cwd");
        let home = d.0.join("home");
        std::fs::create_dir(&home).unwrap();
        std::fs::write(
            d.0.join(".toplikerc"),
            "sensitivity = 9.0\nnope = 1\n[[arcade.row]]\npanels = [{ view = \"flow\" }]\n",
        )
        .unwrap();
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "config::tests::toplikerc_probe", "--ignored"])
            .current_dir(&d.0)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join(".config"))
            .env("TT_TOPLIKE_TOPLIKERC_PROBE", "1")
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success() && text.contains("1 passed"),
            "{text}{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // ---- [backdrop] -------------------------------------------------------

    use crate::animation::backdrop::BackdropKind;

    const SKY: crate::animation::backdrop::Choice = Some(BackdropKind::Sky);
    const CLOUDS: crate::animation::backdrop::Choice = Some(BackdropKind::Clouds);

    #[test]
    fn the_backdrop_table_from_the_brief_loads() {
        let text = "[backdrop]\ndefault = \"off\"\n[backdrop.views]\ntraining = \"clouds\"\ninference = \"off\"\n";
        let l = load(&[("c.toml", text)]);
        assert!(
            l.problems.is_empty() && l.notes.is_empty(),
            "{:?} {:?}",
            l.problems,
            l.notes
        );
        assert_eq!(l.backdrop.default, Some(None));
        assert_eq!(
            l.backdrop.views,
            vec![("training", CLOUDS), ("inference", None)]
        );
        assert_eq!(l.backdrop.choice("training"), CLOUDS);
        assert_eq!(l.backdrop.choice("inference"), None);
        assert_eq!(
            l.backdrop.choice("die3d"),
            CLOUDS,
            "the view's own backdrop stays"
        );
        // No file: every view keeps its own look.
        let none = load(&[]);
        assert_eq!(none.backdrop, Default::default());
        assert_eq!(none.backdrop.choice("die3d"), CLOUDS);
    }

    /// A `default` other than off is ignored with one note: every view
    /// without a backdrop of its own takes none. `--check-config` still
    /// passes (review S1, now for every such view).
    #[test]
    fn a_default_is_ignored_with_a_note() {
        for kind in ["scene", "sky", "clouds"] {
            let l = load(&[("c.toml", &format!("[backdrop]\ndefault = \"{kind}\"\n"))]);
            assert!(l.problems.is_empty(), "{:?}", l.problems);
            for v in ["starfield", "castle", "arcade", "grid"] {
                assert_eq!(l.backdrop.choice(v), None, "{kind} {v}");
            }
            assert_eq!(l.backdrop.choice("training"), SKY, "its own");
            let n: Vec<String> = l.notes.iter().map(|p| p.to_string()).collect();
            assert_eq!(
                n,
                vec![format!("c.toml:2: backdrop default {kind} is ignored: only inference, training and Wafer Waffler take a backdrop, and each draws its own")]
            );
            let mut out = Vec::new();
            assert_eq!(write_check_report(&l, &mut out), 0);
        }
        let l = load(&[("c.toml", "[backdrop]\ndefault = \"off\"\n")]);
        assert!(l.notes.is_empty(), "{:?}", l.notes);
    }

    #[test]
    fn view_names_take_their_other_spellings() {
        let text = "[backdrop.views]\nwaffler = \"off\"\n\"~\" = \"sky\"\n";
        let l = load(&[("c.toml", text)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.backdrop.views, vec![("die3d", None)]);
        let n: Vec<String> = l.notes.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            n,
            vec!["c.toml:3: [backdrop.views] hivemind is ignored: no backdrop on this view"]
        );
    }

    #[test]
    fn every_bad_backdrop_value_is_reported_with_its_line() {
        let text = "[backdrop]\ndefault = \"fog\"\nfast = true\n[backdrop.views]\ncastel = \"sky\"\ngrid = 3\nstarfield = \"sky\"\ndie3d = \"off\"\nwaffler = \"clouds\"\nflow = \"clouds\"\n";
        let l = load(&[("c.toml", text)]);
        let p: Vec<String> = l.problems.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            p,
            vec![
                "c.toml:2: `backdrop.default` must be off, scene, sky or clouds, got \"fog\"",
                "c.toml:3: unknown key `backdrop.fast` (expected default or views)",
                "c.toml:5: unknown view `castel` in [backdrop.views] (expected one of insights, grid, starfield, castle, flow, arcade, defrag, inference, hivemind, training, die3d)",
                "c.toml:6: `backdrop.views.grid` must be off, scene, sky or clouds",
                "c.toml:9: view `die3d` is set twice in [backdrop.views]",
            ]
        );
        // A view that takes no backdrop is a note, never a problem.
        let n: Vec<String> = l.notes.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            n,
            vec![
                "c.toml:7: [backdrop.views] starfield is ignored: no backdrop on this view",
                "c.toml:10: [backdrop.views] flow is ignored: no backdrop on this view",
            ]
        );
        // Each bad entry keeps the default; the good ones are used.
        assert_eq!(l.backdrop.default, None);
        assert_eq!(l.backdrop.views, vec![("die3d", None)]);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 1, "--check-config fails");
        let p = problems(&[("c.toml", "backdrop = \"sky\"\n")]);
        assert_eq!(p, vec!["c.toml:1: `backdrop` must be a table: [backdrop]"]);
        let p = problems(&[("c.toml", "[backdrop]\nviews = 2\n")]);
        assert_eq!(
            p,
            vec!["c.toml:2: `backdrop.views` must be a table: [backdrop.views]"]
        );
    }

    #[test]
    fn the_old_arcade_cloudfield_is_ignored_with_a_note() {
        for text in [
            "[arcade]\ncloudfield = true\n",
            "[arcade]\ncloudfield = false\n",
            "[arcade]\ncloudfield = true\n[backdrop.views]\narcade = \"clouds\"\n",
        ] {
            let l = load(&[("c.toml", text)]);
            assert!(l.problems.is_empty(), "{:?}", l.problems);
            assert_eq!(l.backdrop.choice("arcade"), None, "{text}");
            let n: Vec<String> = l.notes.iter().map(|p| p.to_string()).collect();
            assert!(
                n.contains(
                    &"c.toml:2: `arcade.cloudfield` is ignored: the arcade carries no backdrop"
                        .to_string()
                ),
                "{text}: {n:?}"
            );
        }
    }

    #[test]
    fn check_config_reports_the_backdrops_and_a_note_does_not_fail_it() {
        let l = load(&[(
            "c.toml",
            "[arcade]\ncloudfield = true\n[backdrop.views]\ncastle = \"sky\"\ntraining = \"clouds\"\n",
        )]);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains(
                "note: c.toml:2: `arcade.cloudfield` is ignored: the arcade carries no backdrop"
            ),
            "{out}"
        );
        assert!(
            out.contains(
                "note: c.toml:4: [backdrop.views] castle is ignored: no backdrop on this view"
            ),
            "{out}"
        );
        assert!(
            out.contains("backdrop: default off; training clouds\n"),
            "{out}"
        );
        assert!(
            out.contains("built in: inference scene, training sky, die3d clouds"),
            "{out}"
        );
        assert!(out.ends_with("ok\n"), "{out}");
    }

    #[test]
    fn the_status_line_shows_a_note_when_there_is_no_problem() {
        let l = load(&[("c.toml", "[arcade]\ncloudfield = true\n")]);
        assert_eq!(
            status_message(&l),
            Some((
                "config: c.toml:2: `arcade.cloudfield` is ignored: the arcade carries no backdrop"
                    .to_string(),
                false
            ))
        );
        let l = load(&[("c.toml", "a = 1\n[arcade]\ncloudfield = true\n")]);
        assert_eq!(
            status_message(&l).map(|m| m.1),
            Some(true),
            "a problem first"
        );
        assert_eq!(status_message(&load(&[])), None);
    }

    // ---- Named layouts ---------------------------------------------------

    use crate::animation::arcade_presets::{presets, LayoutCycle};

    const LAYOUTS: &str = r#"
[arcade]
layout = "tall"

[[arcade.layouts]]
name = "tall"
description = "the castle over the flow"

[[arcade.layouts.row]]
height = 3
panels = [{ view = "castle" }]

[[arcade.layouts.row]]
panels = [{ view = "flow" }]

[[arcade.layouts]]
name = "solo"

[[arcade.layouts.row]]
panels = [{ view = "starfield" }]
"#;

    fn layout_names(l: &LoadedConfig) -> Vec<String> {
        l.layouts.iter().map(|x| x.name.clone()).collect()
    }

    #[test]
    fn user_layouts_load_in_file_order_with_the_start_choice() {
        let l = load(&[("c.toml", LAYOUTS)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(layout_names(&l), vec!["tall", "solo"]);
        assert_eq!(l.layouts[0].description, "the castle over the flow");
        assert_eq!(l.layouts[1].description, "", "a description is optional");
        assert_eq!(l.layouts[0].rows.len(), 2);
        assert_eq!(l.layouts[0].rows[0].height, 3);
        assert_eq!(l.start_layout.as_deref(), Some("tall"));
        assert_eq!(
            l.arcade.rows,
            default_rows(),
            "no [[arcade.row]], default rows"
        );
        let l = load(&[(
            "c.toml",
            &LAYOUTS.replace("name = \"solo\"", "name = \"solo\"\ncloudfield = true"),
        )]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(
            layout_names(&l),
            vec!["tall", "solo"],
            "the old key keeps the layout"
        );
        let c = LayoutCycle::new(&l.layouts, l.start_layout.as_deref());
        assert_eq!(c.current().name, "tall");
        assert_eq!(c.len(), 2 + presets().len());
    }

    /// A layout's `backdrop` and the old `cloudfield` are still read, with
    /// any value, and ignored with one note: the arcade carries no backdrop.
    /// The layout is kept and `--check-config` still passes.
    #[test]
    fn a_layouts_backdrop_or_cloudfield_is_ignored_with_a_note() {
        let with = |extra: &str| {
            load(&[(
                "c.toml",
                &LAYOUTS.replace("name = \"solo\"", &format!("name = \"solo\"\n{extra}")),
            )])
        };
        for (text, key) in [
            ("backdrop = \"clouds\"", "backdrop"),
            ("backdrop = \"off\"", "backdrop"),
            ("backdrop = 3", "backdrop"),
            ("cloudfield = true", "cloudfield"),
            ("cloudfield = \"yes\"", "cloudfield"),
            ("cloudfield = true\nbackdrop = \"sky\"", "backdrop"),
        ] {
            let l = with(text);
            assert!(l.problems.is_empty(), "{text}: {:?}", l.problems);
            assert_eq!(layout_names(&l), vec!["tall", "solo"], "{text}");
            let n: Vec<String> = l.notes.iter().map(|p| p.message.clone()).collect();
            assert_eq!(
                n,
                vec![format!(
                    "layout `solo`: `{key}` is ignored: the arcade carries no backdrop"
                )],
                "{text}"
            );
            let mut l = l;
            choose_layout(&mut l, None);
            let mut out = Vec::new();
            assert_eq!(write_check_report(&l, &mut out), 0, "{text}");
            let out = String::from_utf8(out).unwrap();
            assert!(out.contains("layout `solo`: `"), "{out}");
            assert!(!out.contains("when selected"), "{out}");
        }
    }

    #[test]
    fn the_row_tables_are_the_config_layout_and_come_first() {
        let text = format!(
            "{BRIEF}{}",
            LAYOUTS.replace("[arcade]\nlayout = \"tall\"\n", "")
        );
        let l = load(&[("c.toml", &text)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(layout_names(&l), vec!["config", "tall", "solo"]);
        assert_eq!(l.layouts[0].rows, l.arcade.rows);
        let c = LayoutCycle::new(&l.layouts, l.start_layout.as_deref());
        assert_eq!(c.current().name, "config", "the config layout starts");
        // With no config file at all, classic starts.
        let l = load(&[]);
        assert!(l.layouts.is_empty() && l.start_layout.is_none());
        assert_eq!(LayoutCycle::new(&l.layouts, None).current().name, "classic");
    }

    #[test]
    fn a_problem_in_one_layout_drops_only_that_layout() {
        let text = LAYOUTS.replace(
            "panels = [{ view = \"starfield\" }]",
            "panels = [{ view = \"starfeld\" }]",
        );
        let l = load(&[("c.toml", &text)]);
        let p: Vec<String> = l.problems.iter().map(|p| p.to_string()).collect();
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(
            p[0].starts_with("c.toml:20: layout `solo`: unknown view `starfeld` (expected one of "),
            "{p:?}"
        );
        assert_eq!(
            layout_names(&l),
            vec!["tall"],
            "solo is dropped, tall stays"
        );
        assert_eq!(l.start_layout.as_deref(), Some("tall"));
        // A problem in a layout leaves the row tables be.
        let text = format!(
            "{BRIEF}{}",
            text.replace("[arcade]\nlayout = \"tall\"\n", "")
        );
        let l = load(&[("c.toml", &text)]);
        assert_eq!(layout_names(&l), vec!["config", "tall"]);
    }

    #[test]
    fn a_problem_in_the_row_tables_keeps_the_old_rule_and_the_layouts() {
        let text = format!(
            "[arcade]\ncloudfield = true\n[[arcade.row]]\npanels = [{{ view = \"castel\" }}]\n{}",
            LAYOUTS.replace("[arcade]\nlayout = \"tall\"\n", "")
        );
        let l = load(&[("c.toml", &text)]);
        assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        assert_eq!(l.arcade.rows, default_rows());
        assert!(
            l.notes.is_empty(),
            "the whole [arcade] is dropped, as before"
        );
        assert_eq!(layout_names(&l), vec!["tall", "solo"], "no config layout");
    }

    /// Each problem a user layout can have, with its message and line.
    #[test]
    fn every_layout_problem_is_reported_with_its_line() {
        let cases: &[(&str, &str)] = &[
            (
                "[[arcade.layouts]]\ndescription = \"x\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:1: layout 1 has no `name`",
            ),
            (
                "[[arcade.layouts]]\nname = \"\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:2: layout 1: the name `` is empty",
            ),
            (
                "[[arcade.layouts]]\nname = \"my layout\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:2: layout 1: the name `my layout` may hold only letters, digits, `-` and `_`",
            ),
            (
                "[[arcade.layouts]]\nname = 7\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:2: layout 1: `name` must be a string",
            ),
            (
                "[[arcade.layouts]]\nname = \"deck\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:2: layout 1: the name `deck` is a built-in layout; choose another",
            ),
            (
                "[[arcade.layouts]]\nname = \"Config\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:2: layout 1: the name `Config` is kept for the [[arcade.row]] tables",
            ),
            (
                "[[arcade.layouts]]\nname = \"next\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:2: layout 1: the name `next` is a word of /arcade layout; choose another",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n[[arcade.layouts]]\nname = \"A\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:6: layout 2: the name `A` is used twice; each layout needs its own name",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\n",
                "c.toml:1: layout `a` has no rows: give it at least one [[arcade.layouts.row]]",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\nrows = 2\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:3: unknown key `rows` in layout 1 (expected name, description, backdrop or row)",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\ndescription = 3\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:3: layout `a`: `description` must be a string",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\ndescription = \"bell\\u0007\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
                "c.toml:3: layout `a`: the description has a control character",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\n[[arcade.layouts.row]]\nheight = 0\npanels = [{ view = \"flow\" }]\n",
                "c.toml:4: layout `a`: row 1 height must be a whole number from 1 to 1000, got 0",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\n[[arcade.layouts.row]]\npanels = []\n",
                "c.toml:4: layout `a`: row 1 is empty: give it at least one panel",
            ),
            (
                "[[arcade.layouts]]\nname = \"a\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }, { view = \"flow\" }]\n",
                "c.toml:4: layout `a`: view `flow` appears twice; each view can be shown once",
            ),
            (
                "[arcade]\nlayouts = 3\n",
                "c.toml:2: `arcade.layouts` must be a list of layouts: [[arcade.layouts]]",
            ),
            (
                "[[arcade.layout]]\nname = \"a\"\n",
                "c.toml:1: `arcade.layout` names the layout to start with, such as layout = \"deck\"; write your own layouts as [[arcade.layouts]]",
            ),
            (
                "[arcade]\nlayout = \"nope\"\n",
                "c.toml:2: unknown arcade layout `nope` (expected one of classic, now-serving, defrag-wide, waffler, workload, deck, skyline, memory-lane); the arcade starts with `classic`",
            ),
        ];
        for (text, want) in cases {
            let p = problems(&[("c.toml", text)]);
            assert_eq!(p, vec![want.to_string()], "{text}");
        }
        // A long name and a long description.
        let long = format!(
            "[[arcade.layouts]]\nname = \"{}\"\ndescription = \"{}\"\n[[arcade.layouts.row]]\npanels = [{{ view = \"flow\" }}]\n",
            "n".repeat(33),
            "d".repeat(81)
        );
        let p = problems(&[("c.toml", &long)]);
        assert_eq!(
            p,
            vec![
                format!(
                    "c.toml:2: layout 1: the name `{}` is longer than 32 characters",
                    "n".repeat(33)
                ),
                "c.toml:3: layout 1: the description is longer than 80 characters".to_string(),
            ]
        );
    }

    /// A broken layout does not take its name from a good one after it, a
    /// typed name is echoed with its control characters escaped, and a
    /// description refuses invisible format characters (review M3, M9).
    #[test]
    fn names_are_echoed_safely_and_only_a_kept_layout_takes_its_name() {
        let text = "[[arcade.layouts]]\nname = \"a\"\n[[arcade.layouts.row]]\npanels = [{ view = \"castel\" }]\n[[arcade.layouts]]\nname = \"a\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n";
        let l = load(&[("c.toml", text)]);
        assert_eq!(l.problems.len(), 1, "{:?}", l.problems);
        assert_eq!(layout_names(&l), vec!["a"], "the good `a` is kept");
        let p = problems(&[(
            "c.toml",
            "[[arcade.layouts]]\nname = \"a\\u001b[2J\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
        )]);
        assert_eq!(p.len(), 1);
        assert!(
            !p[0].contains('\u{1b}') && p[0].contains("\\u{1b}"),
            "{p:?}"
        );
        let p = problems(&[("c.toml", "[arcade]\nlayout = \"x\\u001b[2J\"\n")]);
        assert!(!p[0].contains('\u{1b}'), "{p:?}");
        let p = problems(&[(
            "c.toml",
            "[[arcade.layouts]]\nname = \"a\"\ndescription = \"abc\\u202Edef\"\n[[arcade.layouts.row]]\npanels = [{ view = \"flow\" }]\n",
        )]);
        assert_eq!(
            p,
            vec!["c.toml:3: layout `a`: the description has a control character"]
        );
    }

    #[test]
    fn an_unknown_start_layout_falls_back_to_the_first() {
        let l = load(&[("c.toml", "[arcade]\nlayout = \"nope\"\ncloudfield = true\n")]);
        assert_eq!(l.start_layout, None);
        assert_eq!(
            l.notes.len(),
            1,
            "a bad start name is not a problem of [arcade]: {:?}",
            l.notes
        );
        let l = load(&[("c.toml", "[arcade]\nlayout = \"Skyline\"\n")]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.start_layout.as_deref(), Some("Skyline"));
        let l = load(&[("c.toml", "[arcade]\nlayout = 3\n")]);
        assert_eq!(
            l.problems[0].to_string(),
            "c.toml:2: `arcade.layout` must be a layout name, such as layout = \"deck\""
        );
    }

    #[test]
    fn the_command_line_layout_wins_and_an_unknown_one_is_a_problem() {
        let mut l = load(&[("c.toml", LAYOUTS)]);
        choose_layout(&mut l, Some("solo"));
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.start_layout.as_deref(), Some("solo"));
        choose_layout(&mut l, None);
        assert_eq!(
            l.start_layout.as_deref(),
            Some("solo"),
            "no flag changes nothing"
        );
        choose_layout(&mut l, Some("nope"));
        assert_eq!(l.start_layout, None, "the first layout starts");
        assert_eq!(
            l.problems.iter().map(|p| p.to_string()).collect::<Vec<_>>(),
            vec![format!(
                "--arcade-layout: unknown arcade layout `nope` (expected one of tall, solo, {}); the arcade starts with `tall`",
                presets().iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")
            )]
        );
    }

    #[test]
    fn check_config_lists_every_layout_and_its_panels() {
        let mut l = load(&[("c.toml", LAYOUTS)]);
        choose_layout(&mut l, None);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("\narcade starts with tall: 2 rows\n"), "{out}");
        let mut ns = load(&[]);
        choose_layout(&mut ns, Some("now-serving"));
        let mut o = Vec::new();
        write_check_report(&ns, &mut o);
        let o = String::from_utf8(o).unwrap();
        assert!(
            o.contains("\narcade starts with now-serving: 2 rows\n"),
            "{o}"
        );
        assert!(
            out.contains("layouts, in the order } steps through them (* starts):"),
            "{out}"
        );
        assert!(out.contains("* tall  the castle over the flow\n"), "{out}");
        assert!(out.contains("    row 1 (height 3): castle x1\n"), "{out}");
        assert!(out.contains("\n  solo\n"), "{out}");
        for p in presets() {
            assert!(
                out.contains(&format!("  {}  {}\n", p.name, p.description)),
                "{out}"
            );
        }
        assert!(
            out.contains("    row 2 (height 4): castle x1, defrag x1, die3d x1\n"),
            "{out}"
        );
        assert!(
            out.contains("now-serving  ")
                && out.contains("and saga\n    row 1 (height 3): inference x1\n    row 2 (height 4): die3d x1, saga x1\n")
                && !out.contains("when selected"),
            "{out}"
        );
        assert!(out.ends_with("ok\n"), "{out}");
    }

    /// Every TOML example in docs/arcade-layout.md parses with the real
    /// loader and has no problem.
    #[test]
    fn the_docs_examples_load_without_a_problem() {
        let doc = include_str!("../docs/arcade-layout.md");
        let mut blocks = Vec::new();
        let mut cur: Option<String> = None;
        for line in doc.lines() {
            match (&mut cur, line.trim_start()) {
                (None, "```toml") => cur = Some(String::new()),
                (Some(_), "```") => blocks.push(cur.take().unwrap()),
                (Some(b), _) => {
                    b.push_str(line);
                    b.push('\n');
                }
                _ => {}
            }
        }
        assert!(blocks.len() >= 2, "the doc has its examples");
        let mut with_layouts = 0;
        for b in &blocks {
            let l = load(&[("doc.toml", b)]);
            assert!(l.problems.is_empty(), "{b}\n{:?}", l.problems);
            if b.contains("[[arcade.layouts]]") {
                with_layouts += 1;
                assert!(!l.layouts.is_empty(), "{b}");
            }
        }
        assert!(with_layouts >= 1, "a user-layout example is there");
    }

    /// Every TOML example in docs/saga.md parses with the real loader, has
    /// no problem, and sets what it shows: a doc example that the loader
    /// ignored or refused would mislead the reader.
    #[test]
    fn the_saga_docs_examples_load_and_take_effect() {
        let doc = include_str!("../docs/saga.md");
        let mut blocks = Vec::new();
        let mut cur: Option<String> = None;
        for line in doc.lines() {
            match (&mut cur, line.trim_start()) {
                (None, "```toml") => cur = Some(String::new()),
                (Some(_), "```") => blocks.push(cur.take().unwrap()),
                (Some(b), _) => {
                    b.push_str(line);
                    b.push('\n');
                }
                _ => {}
            }
        }
        assert!(!blocks.is_empty(), "the doc has its [saga] example");
        for b in &blocks {
            let l = load(&[("doc.toml", b)]);
            assert!(l.problems.is_empty(), "{b}\n{:?}", l.problems);
            assert!(b.contains("[saga]"), "{b}");
            assert_ne!(
                l.saga,
                SagaSettings::default(),
                "the example changes something:\n{b}"
            );
        }
        // The example in the doc's config section, value by value.
        let l = load(&[("doc.toml", &blocks[0])]);
        assert!(l.saga.enabled);
        assert_eq!(l.saga.names, SagaNames::Generated);
        assert_eq!(l.saga.season_minutes, 30);
    }
}
