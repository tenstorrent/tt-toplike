// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Pure parsers for TT inference-server probe output: `docker stats`, the
//! `/tt-liveness` HTTP probe, `env`/`printenv` dumps, and `ps` snapshots.
//!
//! These functions never touch the network or the filesystem themselves —
//! they only interpret text that some other layer already captured. That
//! keeps them trivially unit-testable and panic-free on untrusted input
//! (raw docker/HTTP/process output), since a malformed line should degrade
//! to `None`/a conservative `Readiness` rather than crash the monitor.

/// Readiness ladder from a liveness probe.
#[derive(Debug, Clone, PartialEq)]
pub enum Readiness {
    Down,                              // connection refused / no response
    NotReady,                          // up but model not loaded (e.g. 405 "Model is not ready")
    Ready { runner: Option<String> },  // 200; runner = runner_in_use if present
}

/// Parse `{{.CPUPerc}}|{{.MemUsage}}` → (cpu%, rss bytes). MemUsage is "USED / LIMIT".
pub fn parse_docker_stats(line: &str) -> Option<(f32, u64)> {
    let (cpu_s, mem_s) = line.split_once('|')?;
    let cpu = cpu_s.trim().trim_end_matches('%').parse::<f32>().ok()?;
    let used = mem_s.split('/').next()?.trim();
    Some((cpu, parse_size_bytes(used)?))
}

/// "39.96GiB" / "812916KiB" / "700MiB" → bytes.
fn parse_size_bytes(s: &str) -> Option<u64> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_alphabetic())?;
    let (num, unit) = s.split_at(split);
    let v = num.trim().parse::<f64>().ok()?;
    let mult = match unit.trim().to_lowercase().as_str() {
        "b" => 1.0,
        "kib" | "kb" => 1024.0,
        "mib" | "mb" => 1024.0 * 1024.0,
        "gib" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "tib" | "tb" => 1024.0f64.powi(4),
        _ => return None,
    };
    Some((v * mult) as u64)
}

/// Interpret an HTTP status + body from the `/tt-liveness` probe as a `Readiness`.
pub fn parse_liveness(status: u16, body: &str) -> Readiness {
    match status {
        0 => Readiness::Down,
        200 => {
            // pull runner_in_use if present (tolerant substring parse, no serde dep needed)
            let runner = body
                .split_once("\"runner_in_use\"")
                .and_then(|(_, rest)| rest.split_once(':'))
                .and_then(|(_, rest)| rest.split('"').nth(1))
                .map(|s| s.to_string());
            Readiness::Ready { runner }
        }
        _ => Readiness::NotReady, // 405 "Model is not ready", 503, etc.
    }
}

/// Extract `KEY=VALUE` from `env`/`printenv`-style output.
pub fn parse_env_var(env_output: &str, key: &str) -> Option<String> {
    env_output.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k == key).then(|| v.to_string())
    })
}

/// Count non-empty lines (kernel-artifact count, FD count).
pub fn count_lines(s: &str) -> usize {
    s.lines().filter(|l| !l.trim().is_empty()).count()
}

/// First data row of `ps -eo pcpu,rss,comm --sort=-pcpu` → (comm, cpu%, rss bytes).
/// rss is KiB in ps output. Skips the header line.
pub fn top_process(ps_output: &str) -> Option<(String, f32, u64)> {
    // Skip the header row, then parse the first data row. (A plain `for` loop
    // here trips clippy::never_loop, since every path either returns or
    // short-circuits via `?` on the first iteration — there's never a second.)
    let line = ps_output.lines().nth(1)?;
    let mut it = line.split_whitespace();
    let cpu = it.next()?.parse::<f32>().ok()?;
    let rss_kib = it.next()?.parse::<u64>().ok()?;
    let comm = it.next()?.to_string();
    Some((comm, cpu, rss_kib * 1024))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_docker_stats_line() {
        // `docker stats --no-stream --format '{{.CPUPerc}}|{{.MemUsage}}'`
        let (cpu, rss) = parse_docker_stats("102.17%|39.96GiB / 249.3GiB").unwrap();
        assert!((cpu - 102.17).abs() < 0.01);
        assert_eq!(rss, (39.96 * 1024.0 * 1024.0 * 1024.0) as u64);
    }
    #[test]
    fn liveness_ladder() {
        assert!(matches!(parse_liveness(0, ""), Readiness::Down));
        assert!(matches!(parse_liveness(405, r#"{"detail":"Model is not ready"}"#), Readiness::NotReady));
        match parse_liveness(200, r#"{"runner_in_use":"tt-z-image-turbo"}"#) {
            Readiness::Ready { runner } => assert_eq!(runner.as_deref(), Some("tt-z-image-turbo")),
            _ => panic!("expected Ready"),
        }
    }
    #[test]
    fn parses_env_and_counts() {
        assert_eq!(parse_env_var("HOME=/root\nTT_METAL_HOME=/x/tt-metal\n", "TT_METAL_HOME").as_deref(), Some("/x/tt-metal"));
        assert_eq!(count_lines("a\nb\nc\n"), 3);
        assert_eq!(count_lines(""), 0);
    }
    #[test]
    fn top_process_from_ps() {
        // `ps -eo pcpu,rss,comm --sort=-pcpu` (rss in KiB)
        let out = "%CPU   RSS COMMAND\n33.7 9043136 python3\n 5.2 815612 python3\n";
        let (name, cpu, rss) = top_process(out).unwrap();
        assert_eq!(name, "python3");
        assert!((cpu - 33.7).abs() < 0.01);
        assert_eq!(rss, 9043136 * 1024);
    }
}
