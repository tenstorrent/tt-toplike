// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Turn inference-server log lines into lifecycle events, and fold events (plus
//! the readiness probe) into a `LifecycleState`. Pure classifier + reducer;
//! the log *source* lives alongside as a trait (see `LogSource`).

/// Coarse lifecycle phase surfaced on the Insights card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Starting,
    Loading,
    Ready,
    Serving,
    Error,
}

/// A single classified log observation. Trail: add variants (e.g.
/// `Tokens { per_sec }`, `DiffusionStep { i, n }`) for richer telemetry later.
#[derive(Debug, Clone, PartialEq)]
pub enum LifecycleEvent {
    Loading { progress: Option<f32>, detail: String },
    Ready,
    Serving,
    Error(String),
}

/// Placeholder for future serving telemetry (tokens/sec, latency, …). Always
/// `None` on `LifecycleState` in v1; the field + type reserve the card slot.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServingMetrics {}

/// Best-effort, version-tolerant. Returns `None` for lines we don't recognize
/// (state then holds). Case-insensitive; ordered most- to least-specific.
pub fn classify_log_line(line: &str) -> Option<LifecycleEvent> {
    let l = line.to_lowercase();

    // Errors first.
    let trimmed = l.trim_start();
    if l.contains("traceback (most recent call last)")
        || l.contains("failed to load")
        || trimmed.starts_with("error")
        || trimmed.starts_with("fatal")
        || l.contains(" error:")
        || l.contains(" fatal")
    {
        return Some(LifecycleEvent::Error(line.trim().to_string()));
    }
    // Readiness markers (uvicorn/vLLM/tt-metal).
    if l.contains("application startup complete")
        || l.contains("uvicorn running on")
        || l.contains("model ready")
        || l.contains("compilation complete")
    {
        return Some(LifecycleEvent::Ready);
    }
    // Serving: a successful inference request (not a health poll).
    if (l.contains("/v1/") || l.contains("/generate"))
        && !l.contains("/v1/models")
        && (l.contains(" 200") || l.contains(" 201"))
    {
        return Some(LifecycleEvent::Serving);
    }
    // Loading, optionally with a percentage.
    if l.contains("loading") || l.contains("downloading") || l.contains("compiling") {
        let progress = parse_percent(&l);
        return Some(LifecycleEvent::Loading {
            progress,
            detail: line.trim().to_string(),
        });
    }
    None
}

/// Extract the first `NN%` as a 0..1 fraction, if present. Char-boundary-safe
/// (digits/'.' are ASCII, so the computed start is always a valid boundary).
fn parse_percent(l: &str) -> Option<f32> {
    let idx = l.find('%')?;
    let prefix = &l[..idx];
    let trailing: usize = prefix
        .chars()
        .rev()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .map(|c| c.len_utf8())
        .sum();
    prefix[prefix.len() - trailing..]
        .parse::<f32>()
        .ok()
        .map(|p| (p / 100.0).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_known_log_markers() {
        assert!(matches!(
            classify_log_line("Loading weights: 42%"),
            Some(LifecycleEvent::Loading { progress: Some(p), .. }) if (p - 0.42).abs() < 1e-3
        ));
        assert!(matches!(
            classify_log_line("INFO: Application startup complete."),
            Some(LifecycleEvent::Ready)
        ));
        assert!(matches!(
            classify_log_line("Traceback (most recent call last):"),
            Some(LifecycleEvent::Error(_))
        ));
        assert!(matches!(
            classify_log_line(r#"172.17.0.1 - "POST /v1/images/generations HTTP/1.1" 200"#),
            Some(LifecycleEvent::Serving)
        ));
        // Health-check spam and unknown lines are ignored.
        assert!(classify_log_line(r#""GET /health HTTP/1.0" 405 Method Not Allowed"#).is_none());
        assert!(classify_log_line("some unrelated chatter").is_none());

        // Non-ASCII abutting a percentage must NOT panic. (Deviates from the
        // literal task example "下载 weights 42%", which never reaches
        // parse_percent because it lacks a loading/downloading/compiling
        // keyword; this string keeps the "loading" keyword while still
        // having non-ASCII text directly abutting the digits, reproducing
        // Finding 1's byte-slicing panic through classify_log_line.)
        assert!(matches!(
            classify_log_line("loading 下载42%"),
            Some(LifecycleEvent::Loading { progress: Some(_), .. })
        ));
        // Line-initial error marker is classified.
        assert!(matches!(classify_log_line("Error: connection refused"), Some(LifecycleEvent::Error(_))));
        // "non-fatal" must NOT be classified as Error.
        assert!(classify_log_line("this is a non-fatal warning").is_none() ||
            !matches!(classify_log_line("this is a non-fatal warning"), Some(LifecycleEvent::Error(_))));
    }
}
