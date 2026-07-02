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
    if l.contains("traceback (most recent call last)")
        || l.contains("failed to load")
        || l.contains("fatal")
        || l.contains(" error:")
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

/// Extract the first `NN%` as a 0..1 fraction, if present.
fn parse_percent(l: &str) -> Option<f32> {
    let idx = l.find('%')?;
    let start = l[..idx].rfind(|c: char| !c.is_ascii_digit() && c != '.').map_or(0, |p| p + 1);
    l[start..idx].parse::<f32>().ok().map(|p| (p / 100.0).clamp(0.0, 1.0))
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
    }
}
