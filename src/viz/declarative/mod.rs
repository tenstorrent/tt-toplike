// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Declarative viz: a view described by a YAML or JSON file.
//!
//! This module is the data side: the types ([`schema`]), the rules and
//! limits ([`validate`]) and [`parse`]. Drawing a [`VizDef`] and finding
//! files on disk are separate steps that build on this one.
//!
//! The steps for a file are: `parse` (size check, then YAML or JSON by file
//! extension, same types for both), then `validate`. Both return a
//! [`LoadError`] whose one-line `Display` names the file and, where the
//! parser knows it, the line, so the host can log it as-is.

pub mod schema;
pub mod validate;

#[cfg(test)]
mod schema_tests;

use std::fmt;
use std::path::{Path, PathBuf};

pub use schema::{
    CellDef, ColorName, EachChip, Height, KindName, RowDef, Threshold, VizDef, WidgetBody,
    WidgetDef,
};
pub use validate::validate;

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
            message: message.into(),
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
            message: message.into(),
        }
    }
}

impl fmt::Display for LoadError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let has_path = !self.path.as_os_str().is_empty();
        if has_path {
            write!(f, "{}", self.path.display())?;
            if let Some(line) = self.line {
                write!(f, ":{line}")?;
            }
            f.write_str(": ")?;
        }
        f.write_str(&self.message)
    }
}

impl std::error::Error for LoadError {}

/// Drop the trailing ` at line N column M` that serde_yaml_ng and
/// serde_json add to their messages; the line goes in the prefix instead.
fn strip_location(msg: &str) -> String {
    match msg.rfind(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg.to_string(),
    }
}

/// Parse `text`, read from `path`, into a [`VizDef`].
///
/// The format follows the extension: `.json` is JSON, `.yaml` and `.yml`
/// are YAML. Any other extension is an error. Text over
/// [`validate::MAX_FILE_BYTES`] is refused before any parser sees it. The
/// result is not yet checked against the rules: call [`validate`] next.
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
        Some("yaml") | Some("yml") => serde_yaml_ng::from_str(text).map_err(|e| {
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
