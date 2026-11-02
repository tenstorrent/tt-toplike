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
/// read, the arcade layout in use, and every problem as `file:line: message`.
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
        let a = &loaded.arcade;
        let default = a.rows == crate::animation::arcade_layout::default_rows();
        writeln!(
            out,
            "arcade: {} layout, {} row{}, cloudfield {}",
            if default { "default" } else { "configured" },
            a.rows.len(),
            if a.rows.len() == 1 { "" } else { "s" },
            if a.cloudfield { "on" } else { "off" }
        )?;
        for (i, r) in a.rows.iter().enumerate() {
            let panels: Vec<String> = r
                .panels
                .iter()
                .map(|p| format!("{} x{}", p.view.key(), p.width))
                .collect();
            writeln!(
                out,
                "  row {} (height {}): {}",
                i + 1,
                r.height,
                panels.join(", ")
            )?;
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
const TOP_KEYS: [&str; 5] = [
    "sensitivity",
    "max_particles_scale",
    "tt_smi_reset_behavior",
    "arcade",
    "backdrop",
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
            let before = out.problems.len();
            let mut arcade_report = |span: Option<std::ops::Range<usize>>, message: String| {
                out.problems.push(ConfigProblem {
                    file: path.clone(),
                    line: span.map(|s| line_of(text, s.start)),
                    message,
                });
            };
            let parsed = parse_arcade(item, &mut arcade_report);
            if out.problems.len() > before {
                arcade_bad = true;
            }
            if let Some(c) = parsed.0 {
                cloudfield = Some(c);
                let line = item
                    .as_table_like()
                    .and_then(|t| t.get("cloudfield"))
                    .and_then(|v| v.span())
                    .map(|sp| line_of(text, sp.start));
                cloudfield_at = Some((path.clone(), line));
            }
            if let Some(r) = parsed.1 {
                rows = Some(r);
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
            parse_backdrop(item, &mut backdrop, &mut backdrop_report);
        }
        // This file's problems in line order.
        out.problems[first_problem..].sort_by_key(|p| p.line.unwrap_or(0));
    }
    // `arcade.cloudfield` still works: `true` is `[backdrop.views] arcade =
    // "scene"`, `false` is off, and a `[backdrop.views] arcade` entry wins.
    if let (Some(on), Some((file, line))) = (cloudfield.filter(|_| !arcade_bad), cloudfield_at) {
        if !backdrop.views.iter().any(|(v, _)| *v == "arcade") {
            backdrop.views.push((
                "arcade",
                on.then_some(crate::animation::backdrop::BackdropKind::Scene),
            ));
        }
        out.notes.push(ConfigProblem {
            file,
            line,
            message: "`arcade.cloudfield` is deprecated; write [backdrop.views] arcade = \"scene\""
                .into(),
        });
    }
    out.backdrop = backdrop;
    let mut arcade = ArcadeConfig::default();
    if !arcade_bad {
        if let Some(c) = cloudfield {
            arcade.cloudfield = c;
        }
        if let Some(r) = rows {
            arcade.rows = r;
        }
    }
    out.arcade = arcade;
    out
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

/// The `[arcade]` table: its `cloudfield` and its rows, each `None` when not
/// set or not valid. Every problem goes to `report`.
fn parse_arcade(
    item: &toml_edit::Item,
    report: &mut Report<'_>,
) -> (
    Option<bool>,
    Option<Vec<crate::animation::arcade_layout::RowSpec>>,
) {
    use crate::animation::arcade_layout::{PanelSpec, RowSpec, View};
    let Some(table) = item.as_table_like() else {
        report(item.span(), "`arcade` must be a table: [arcade]".into());
        return (None, None);
    };
    for (k, _) in table.iter() {
        if !["cloudfield", "row"].contains(&k) {
            let span = table.get_key_value(k).and_then(|(key, _)| key.span());
            report(
                span,
                format!("unknown key `arcade.{k}` (expected cloudfield or row)"),
            );
        }
    }
    let cloudfield = match table.get("cloudfield") {
        None => None,
        Some(v) => match v.as_bool() {
            Some(b) => Some(b),
            None => {
                report(v.span(), "`arcade.cloudfield` must be true or false".into());
                None
            }
        },
    };
    let Some(row_item) = table.get("row") else {
        return (cloudfield, None);
    };
    let row_tables: Vec<(&dyn toml_edit::TableLike, Option<std::ops::Range<usize>>)> =
        match row_item {
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
                            None => report(e.span(), "each `arcade.row` must be a table".into()),
                        }
                    }
                    v
                }
                None => {
                    report(
                        row_item.span(),
                        "`arcade.row` must be a list of rows: [[arcade.row]]".into(),
                    );
                    return (cloudfield, None);
                }
            },
        };
    if row_tables.is_empty() {
        report(row_item.span(), "`arcade.row` has no rows".into());
        return (cloudfield, None);
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
    (cloudfield, Some(rows))
}

/// The `[backdrop]` table: `default` and `[backdrop.views]`, into `cfg`. A
/// bad entry is reported and left out, so it keeps its default; the other
/// entries are used.
fn parse_backdrop(
    item: &toml_edit::Item,
    cfg: &mut crate::animation::backdrop::BackdropConfig,
    report: &mut Report<'_>,
) {
    use crate::animation::backdrop::{
        canonical_view, parse_choice, refusal, Choice, CHOICES, VIEWS,
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
        assert!(!l.arcade.cloudfield);
        let l = load(&[("c.toml", "sensitivity = 2.0")]);
        assert_eq!(l.arcade.rows, default_rows());
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.overrides.sensitivity, Some(2.0));
    }

    #[test]
    fn the_example_from_the_brief_loads() {
        let l = load(&[("c.toml", BRIEF)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert!(l.arcade.cloudfield);
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
                "c.toml:3: unknown key `arcade.clouds` (expected cloudfield or row)",
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
    fn a_bad_cloudfield_is_reported() {
        let p = problems(&[("c.toml", "[arcade]\ncloudfield = \"yes\"\n")]);
        assert_eq!(
            p,
            vec!["c.toml:2: `arcade.cloudfield` must be true or false"]
        );
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
        assert!(out.contains("arcade: default layout"), "{out}");
        let l = load(&[("c.toml", "[arcade]\ncloudfield = true\n")]);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.ends_with("ok\n") && out.contains("cloudfield on"),
            "{out}"
        );
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
            vec!["/x/config.toml:2: unknown key `arcade.nope` (expected cloudfield or row)"]
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

    const SCENE: crate::animation::backdrop::Choice = Some(BackdropKind::Scene);
    const SKY: crate::animation::backdrop::Choice = Some(BackdropKind::Sky);
    const CLOUDS: crate::animation::backdrop::Choice = Some(BackdropKind::Clouds);

    #[test]
    fn the_backdrop_table_from_the_brief_loads() {
        let text = "[backdrop]\ndefault = \"off\"\n[backdrop.views]\narcade = \"scene\"\ncastle = \"clouds\"\n";
        let l = load(&[("c.toml", text)]);
        assert!(
            l.problems.is_empty() && l.notes.is_empty(),
            "{:?}",
            l.problems
        );
        assert_eq!(l.backdrop.default, Some(None));
        assert_eq!(
            l.backdrop.views,
            vec![("arcade", SCENE), ("castle", CLOUDS)]
        );
        assert_eq!(l.backdrop.choice("arcade"), SCENE);
        assert_eq!(
            l.backdrop.choice("training"),
            SKY,
            "the view's own backdrop stays"
        );
        // No file: every view keeps its own look.
        let none = load(&[]);
        assert_eq!(none.backdrop, Default::default());
        assert_eq!(none.backdrop.choice("die3d"), CLOUDS);
    }

    #[test]
    fn view_names_take_their_other_spellings() {
        let text = "[backdrop.views]\nwaffler = \"off\"\n\"~\" = \"sky\"\n";
        let l = load(&[("c.toml", text)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.backdrop.views, vec![("die3d", None), ("hivemind", SKY)]);
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
                "c.toml:7: starfield refuses sky: its stars are Tensix cores, and this backdrop draws stars too",
                "c.toml:9: view `die3d` is set twice in [backdrop.views]",
            ]
        );
        // Each bad entry keeps the default; the good ones are used.
        assert_eq!(l.backdrop.default, None);
        assert_eq!(l.backdrop.views, vec![("die3d", None), ("flow", CLOUDS)]);
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
    fn cloudfield_still_turns_on_the_arcade_scene_with_a_note() {
        let l = load(&[("c.toml", "[arcade]\ncloudfield = true\n")]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.backdrop.choice("arcade"), SCENE);
        let n: Vec<String> = l.notes.iter().map(|p| p.to_string()).collect();
        assert_eq!(
            n,
            vec!["c.toml:2: `arcade.cloudfield` is deprecated; write [backdrop.views] arcade = \"scene\""]
        );
        // `[backdrop.views] arcade` wins over it.
        let text = "[arcade]\ncloudfield = true\n[backdrop.views]\narcade = \"clouds\"\n";
        let l = load(&[("c.toml", text)]);
        assert_eq!(l.backdrop.choice("arcade"), CLOUDS);
        assert_eq!(l.notes.len(), 1);
        // `false` is the default: the arcade has no backdrop.
        let l = load(&[("c.toml", "[arcade]\ncloudfield = false\n")]);
        assert_eq!(l.backdrop.choice("arcade"), None);
    }

    #[test]
    fn check_config_reports_the_backdrops_and_a_note_does_not_fail_it() {
        let l = load(&[(
            "c.toml",
            "[arcade]\ncloudfield = true\n[backdrop.views]\ncastle = \"sky\"\n",
        )]);
        let mut out = Vec::new();
        assert_eq!(write_check_report(&l, &mut out), 0);
        let out = String::from_utf8(out).unwrap();
        assert!(
            out.contains("note: c.toml:2: `arcade.cloudfield` is deprecated"),
            "{out}"
        );
        assert!(
            out.contains("backdrop: default off; castle sky, arcade scene"),
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
                "config: c.toml:2: `arcade.cloudfield` is deprecated; write [backdrop.views] arcade = \"scene\"".to_string(),
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
}
