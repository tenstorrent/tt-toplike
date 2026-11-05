// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Declarative viz: a view described by a YAML or JSON file.
//!
//! This module is the data side: the types ([`schema`]), the rules and
//! limits ([`validate`]) and [`parse`]. Drawing a [`VizDef`] and finding
//! files on disk are separate steps that build on this one.
//!
//! Callers use [`load`]: size check, [`parse`] (YAML or JSON by file
//! extension, same types for both), [`validate()`], and the file name added
//! to any error. `parse` and `validate` are public for tests and tools, but
//! calling them directly leaves out the step that puts the file name on a
//! validation error, so later code should not. A [`LoadError`] has a
//! one-line `Display` that names the file and, where the parser knows it,
//! the line, so the host can log it as-is.

pub mod loader;
pub mod schema;
pub mod validate;
pub mod viz;
pub mod widgets;

#[cfg(test)]
mod example_tests;
#[cfg(test)]
mod loader_tests;
#[cfg(test)]
mod schema_tests;
#[cfg(test)]
mod widgets_tests;

use std::fmt;
use std::path::{Path, PathBuf};

pub use schema::{
    CellDef, ColorName, EachChip, Height, KindName, RowDef, Threshold, VizDef, WidgetBody,
    WidgetDef,
};
pub use validate::validate;
pub use viz::DeclarativeViz;

/// Why a definition could not be loaded.
///
/// `Display` is one line: `path:line: message`, with the path and line left
/// out when unknown (`validate` has neither until the caller adds the path).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadError {
    /// The file, or empty when not yet known.
    pub path: PathBuf,
    /// 1-based line in the file, when the parser reported one.
    pub line: Option<usize>,
    /// What is wrong, without the file name.
    pub message: String,
}

impl LoadError {
    /// An error with only a message.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            path: PathBuf::new(),
            line: None,
            message: one_line(&message.into()),
        }
    }

    /// The same error, attributed to `path` (used for `validate` errors,
    /// which do not know the file).
    pub fn with_path(mut self, path: &Path) -> Self {
        self.path = path.to_path_buf();
        self
    }

    fn at(path: &Path, line: Option<usize>, message: impl Into<String>) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
            message: one_line(&message.into()),
        }
    }
}

/// Make user-supplied text safe to print on one log line: control characters
/// (newline, tab, ESC and the rest) are dropped. A file can put its own text
/// into an error message (an unknown widget name, a bad id), and an ESC
/// there could drive the terminal the log is shown on.
fn one_line(s: &str) -> String {
    s.chars().filter(|c| !c.is_control()).collect()
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let has_path = !self.path.as_os_str().is_empty();
        if has_path {
            write!(f, "{}", one_line(&self.path.display().to_string()))?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
            f.write_str(": ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for LoadError {}

/// Load a definition from `text` read from `path`: [`parse`], then
/// [`validate()`], with the file name put on whichever error comes. This is
/// the one function the loader and the host call.
///
/// Nothing here panics or loops on hostile text: the size limit is checked
/// first, YAML and JSON nesting is capped by the parsers, and YAML alias
/// expansion has a repeat limit; tests in `schema_tests.rs` pin each.
pub fn load(path: &Path, text: &str) -> Result<VizDef, LoadError> {
    let def = parse(path, text)?;
    validate(&def).map_err(|e| e.with_path(path))?;
    Ok(def)
}

/// Drop the trailing ` at line N column M` that serde_yaml_ng and
/// serde_json add to their messages; the line goes in the prefix instead.
fn strip_location(msg: &str) -> String {
    match msg.rfind(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg.to_string(),
    }
}

/// Does this YAML tree hold a tagged value anywhere?
fn has_tag(v: &serde_yaml_ng::Value) -> bool {
    use serde_yaml_ng::Value as V;
    match v {
        V::Tagged(_) => true,
        V::Sequence(s) => s.iter().any(has_tag),
        V::Mapping(m) => m.iter().any(|(k, v)| has_tag(k) || has_tag(v)),
        _ => false,
    }
}

/// Parse `text`, read from `path`, into a [`VizDef`].
///
/// The format follows the extension: `.json` is JSON, `.yaml` and `.yml`
/// are YAML. Any other extension is an error. Text over
/// [`validate::MAX_FILE_BYTES`] is refused before any parser sees it. The
/// result is not yet checked against the rules. Prefer [`load`], which
/// also validates and names the file on every error.
pub fn parse(path: &Path, text: &str) -> Result<VizDef, LoadError> {
    if text.len() > validate::MAX_FILE_BYTES {
        return Err(LoadError::at(
            path,
            None,
            format!(
                "file too large: {} bytes (the limit is {})",
                text.len(),
                validate::MAX_FILE_BYTES
            ),
        ));
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("json") => serde_json::from_str(text).map_err(|e| {
            let line = (e.line() > 0).then(|| e.line());
            LoadError::at(path, line, strip_location(&e.to_string()))
        }),
        Some("yaml") | Some("yml") => {
            // serde_yaml_ng skips a tag on a plain value (`version: !evil 1`
            // loads as 1). A definition has no use for tags, so refuse them
            // rather than let a file carry a hidden one. Reading into a
            // generic Value first exposes tags; the typed read below then
            // gives the precise error and line for everything else.
            if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(text) {
                if has_tag(&v) {
                    return Err(LoadError::at(
                        path,
                        None,
                        "YAML tags (`!name`) are not allowed",
                    ));
                }
            }
            serde_yaml_ng::from_str(text)
        }
        .map_err(|e: serde_yaml_ng::Error| {
            let line = e.location().map(|l| l.line());
            LoadError::at(path, line, strip_location(&e.to_string()))
        }),
        _ => Err(LoadError::at(
            path,
            None,
            "unknown file type (expected .yaml, .yml or .json)",
        )),
    }
}
