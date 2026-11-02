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

/// Load config overrides from `~/.config/tt-toplike/config.toml` and
/// `./.toplikerc` (see [`load_config`]). Every problem in the files is
/// logged; a file with a syntax error contributes nothing.
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
    /// Every problem in every file, in file order.
    pub problems: Vec<ConfigProblem>,
    /// The files that exist and were read, in the order they apply.
    pub files: Vec<PathBuf>,
}

/// The name of the per-directory override file.
pub const LOCAL_CONFIG: &str = ".toplikerc";

/// The config files, in the order they apply: the shared
/// `~/.config/tt-toplike/config.toml`, then `./.toplikerc` in the working
/// directory. A key set in both takes the value from `.toplikerc`.
pub fn config_files() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = config_path().into_iter().collect();
    out.push(PathBuf::from(LOCAL_CONFIG));
    out
}

/// Read the config files that exist (see [`config_files`]).
pub fn load_config() -> LoadedConfig {
    let mut sources = Vec::new();
    let mut problems = Vec::new();
    for path in config_files() {
        match std::fs::read_to_string(&path) {
            Ok(text) => sources.push((path, text)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => problems.push(ConfigProblem {
                file: path,
                line: None,
                message: format!("cannot be read: {e}"),
            }),
        }
    }
    let mut loaded = load_from(&sources);
    problems.append(&mut loaded.problems);
    loaded.problems = problems;
    loaded
}

/// The keys allowed at the top of a config file.
const TOP_KEYS: [&str; 4] = [
    "sensitivity",
    "max_particles_scale",
    "tt_smi_reset_behavior",
    "arcade",
];

/// Merge config `sources` (each a path and its text), later ones winning per
/// key, and collect every problem. Pure: no file is read. See
/// `docs/arcade-layout.md` for the `[arcade]` schema and the messages.
pub fn load_from(sources: &[(PathBuf, String)]) -> LoadedConfig {
    use crate::animation::arcade_layout::ArcadeConfig;
    let mut out = LoadedConfig::default();
    let mut cloudfield: Option<bool> = None;
    let mut rows: Option<Vec<crate::animation::arcade_layout::RowSpec>> = None;
    let mut arcade_bad = false;
    for (path, text) in sources {
        out.files.push(path.clone());
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
                    format!("TOML syntax error: {}", e.message().trim()),
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
                if o.tt_smi_reset_behavior.is_some() {
                    out.overrides.tt_smi_reset_behavior = o.tt_smi_reset_behavior;
                }
            }
            Err(e) => report(e.span(), e.message().trim().to_string()),
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
            }
            if let Some(r) = parsed.1 {
                rows = Some(r);
            }
        }
    }
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

fn config_path() -> Option<PathBuf> {
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
    fn toplikerc_wins_per_key_over_the_shared_file() {
        let shared = "sensitivity = 2.0\nmax_particles_scale = 3.0\n[arcade]\ncloudfield = true\n";
        let local = "sensitivity = 0.5\n[[arcade.row]]\npanels = [{ view = \"die3d\" }]\n";
        let l = load(&[("config.toml", shared), (".toplikerc", local)]);
        assert!(l.problems.is_empty(), "{:?}", l.problems);
        assert_eq!(l.overrides.sensitivity, Some(0.5), "the local file wins");
        assert_eq!(
            l.overrides.max_particles_scale,
            Some(3.0),
            "a key only shared stays"
        );
        assert!(
            l.arcade.cloudfield,
            "cloudfield set only in the shared file stays"
        );
        assert_eq!(
            l.arcade.rows.len(),
            1,
            "the local rows replace the shared rows"
        );
        assert_eq!(l.files.len(), 2);
    }

    #[test]
    fn a_problem_names_the_file_it_is_in() {
        let p = problems(&[
            ("config.toml", "[arcade]\ncloudfield = true\n"),
            (".toplikerc", "[arcade]\nnope = 1\n"),
        ]);
        assert_eq!(
            p,
            vec![".toplikerc:2: unknown key `arcade.nope` (expected cloudfield or row)"]
        );
    }
}
