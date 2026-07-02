// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Log breadcrumb for the inference-server panel. v2 derives lifecycle from
//! resource/process probes (see state.rs), not log parsing — the server log is
//! silent during the long load phase. The only log use is surfacing the last
//! human-meaningful line, filtering out liveness/health-poll spam.

/// Last log line that isn't a health/liveness poll, if any.
pub fn last_non_health_line(lines: &[String]) -> Option<String> {
    lines
        .iter()
        .rev()
        .find(|l| {
            let low = l.to_lowercase();
            !(low.contains("/tt-liveness") || low.contains("/health") || low.contains("/v1/models"))
        })
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn breadcrumb_skips_health_poll_spam() {
        let lines = vec![
            "INFO: compiling kernels for z-image".to_string(),
            r#"172.17.0.1 - "GET /tt-liveness HTTP/1.1" 405 Method Not Allowed"#.to_string(),
            r#"172.17.0.1 - "GET /health HTTP/1.0" 405"#.to_string(),
        ];
        assert_eq!(last_non_health_line(&lines).as_deref(), Some("INFO: compiling kernels for z-image"));
        assert_eq!(last_non_health_line(&[]), None);
    }
}
