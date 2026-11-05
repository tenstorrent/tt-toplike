// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! The rules a parsed [`VizDef`] must meet, and the limits on its size.
//!
//! Parsing (`mod.rs`) already rejects what serde can see: unknown keys,
//! unknown widgets, unknown colours, wrong types. This file checks the
//! rules serde cannot express: the version, the id's shape, counts, ranges
//! and non-empty names. The limits keep a hostile or careless file from
//! making the TUI draw thousands of widgets.

use super::schema::VizDef;
use super::LoadError;

/// Largest file `parse` accepts, in bytes (256 KiB). Checked before parsing.
pub const MAX_FILE_BYTES: usize = 256 * 1024;
/// Most widgets (cells) in one definition, over all rows.
pub const MAX_WIDGETS: usize = 64;
/// Most rows in one definition.
pub const MAX_ROWS: usize = 16;
/// Smallest and largest cell `weight`.
pub const MIN_WEIGHT: u16 = 1;
pub const MAX_WEIGHT: u16 = 100;
/// Longest id, in characters.
pub const MAX_ID_LEN: usize = 48;
/// The only format version that exists.
pub const SUPPORTED_VERSION: u32 = 1;

/// Is `id` kebab-case: lowercase letters and digits in groups joined by
/// single hyphens, starting with a letter, at most [`MAX_ID_LEN`] long?
/// Matches `^[a-z][a-z0-9]*(-[a-z0-9]+)*$` in the JSON Schema.
pub fn check_id(id: &str) -> bool {
    if id.is_empty() || id.len() > MAX_ID_LEN {
        return false;
    }
    let first_ok = id.chars().next().is_some_and(|c| c.is_ascii_lowercase());
    // Every hyphen-separated group must be non-empty (no leading, trailing
    // or doubled hyphen) and made of lowercase letters and digits.
    let groups_ok = id.split('-').all(|g| {
        !g.is_empty()
            && g.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    });
    first_ok && groups_ok
}

/// Check a parsed definition. The error carries no file name (the caller
/// knows the path; see [`LoadError::with_path`]) and no line (the parsed
/// tree has none).
pub fn validate(def: &VizDef) -> Result<(), LoadError> {
    let err = |m: String| Err(LoadError::new(m));

    if def.version != SUPPORTED_VERSION {
        return err(format!(
            "unsupported version {} (this build reads version {SUPPORTED_VERSION})",
            def.version
        ));
    }
    if !check_id(&def.id) {
        return err(format!(
            "bad id `{}`: use lowercase letters, digits and single hyphens, starting with a letter, at most {MAX_ID_LEN} characters",
            def.id
        ));
    }
    if def.title.trim().is_empty() {
        return err("title is empty".into());
    }
    if let Some((w, h)) = def.min_size {
        if w == 0 || h == 0 {
            return err(format!("min_size {w} x {h} must be at least 1 x 1"));
        }
    }
    if def.layout.is_empty() {
        return err("layout has no rows".into());
    }
    if def.layout.len() > MAX_ROWS {
        return err(format!(
            "too many rows: {} (the limit is {MAX_ROWS})",
            def.layout.len()
        ));
    }
    let widgets: usize = def.layout.iter().map(|r| r.cells.len()).sum();
    if widgets > MAX_WIDGETS {
        return err(format!(
            "too many widgets: {widgets} (the limit is {MAX_WIDGETS})"
        ));
    }
    for (ri, row) in def.layout.iter().enumerate() {
        if row.cells.is_empty() {
            return err(format!("row {} has no cells", ri + 1));
        }
        for (ci, cell) in row.cells.iter().enumerate() {
            let at = format!("row {}, cell {}", ri + 1, ci + 1);
            if !(MIN_WEIGHT..=MAX_WEIGHT).contains(&cell.weight) {
                return err(format!(
                    "{at}: weight {} is out of range ({MIN_WEIGHT} to {MAX_WEIGHT})",
                    cell.weight
                ));
            }
            let body = cell.widget.body();
            if body.signal.trim().is_empty() {
                return err(format!("{at}: signal is empty"));
            }
            if let Some(max) = body.max {
                if !max.is_finite() || max <= 0.0 {
                    return err(format!("{at}: max {max} must be a number above 0"));
                }
            }
            if body.thresholds.iter().any(|t| !t.at.is_finite()) {
                return err(format!("{at}: a threshold `at` is not a finite number"));
            }
        }
    }
    Ok(())
}
