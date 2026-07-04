// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Remote-box discovery for the `--remote` UX.
//!
//! tt-station's `tt` CLI advertises boxes over mDNS (`_tenstorrent._tcp`) and
//! lists them with `tt --json discover`, which prints a JSON array of records:
//!
//! ```json
//! [{"name":"qb2-lab","host":"127.0.0.1","ctrl_port":8899,
//!   "chips":"4xBH","status":"serving:llama3","apiver":1}]
//! ```
//!
//! This module turns that list into a `HOST:PORT` we can hand to
//! [`WsBackend::from_host_port`](crate::backend::ws::WsBackend::from_host_port),
//! plus the small plumbing the `/remote` TUI picker needs.
//!
//! ## Layers
//!
//! * [`parse_discovered_boxes`] / [`resolve_remote_target`] — **pure** (no I/O):
//!   they take the discover JSON as a string, so they are trivially unit-tested
//!   by injecting JSON. `resolve_remote_target` is the Part-1 name-resolution
//!   contract: passthrough a `HOST:PORT`, resolve a bare name against the list,
//!   or return a clear error.
//! * [`run_tt_discover`] / [`resolve_remote_spec`] — the **impure** wiring that
//!   actually shells out to `tt`. Kept out of the unit tests on purpose.
//!
//! ## Strictly additive
//!
//! Nothing here is reachable except via the explicit `--remote <name>` flag or
//! the explicit `/remote` slash command. It never touches auto-detect or Tab.

use serde::Deserialize;

/// One box as reported by `tt --json discover`.
///
/// Deserialized leniently: unknown fields are ignored and `apiver` defaults, so
/// a newer/older `tt` that adds or drops fields still parses. `status` and
/// `chips` are kept as the display strings `tt` emits (`"idle"` /
/// `"serving:<model>"`, `"4xBH"`), which is all the picker needs to show.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct DiscoveredBox {
    /// Advertised box name (the token `--remote <name>` matches against).
    pub name: String,
    /// Hostname or IP to connect to.
    pub host: String,
    /// Control port the tt-station-agentd telemetry WebSocket listens on.
    pub ctrl_port: u16,
    /// Chip summary string, e.g. `"4xBH"`. Optional for forward-compat.
    #[serde(default)]
    pub chips: String,
    /// Serving status string: `"idle"` or `"serving:<model>"`.
    #[serde(default)]
    pub status: String,
    /// Advertised API version. Optional for forward-compat.
    #[serde(default)]
    pub apiver: u8,
}

impl DiscoveredBox {
    /// `HOST:PORT` connect target for this box.
    pub fn host_port(&self) -> String {
        format!("{}:{}", self.host, self.ctrl_port)
    }

    /// One-line picker summary: `name · host:port · chips · status`.
    pub fn summary_line(&self) -> String {
        format!(
            "{} · {} · {} · {}",
            self.name,
            self.host_port(),
            if self.chips.is_empty() {
                "?"
            } else {
                &self.chips
            },
            if self.status.is_empty() {
                "?"
            } else {
                &self.status
            },
        )
    }
}

/// Parse the JSON array printed by `tt --json discover`.
///
/// Pure — takes the JSON as a string so it can be unit-tested without shelling
/// out. Returns a clear error string on malformed JSON.
pub fn parse_discovered_boxes(discover_json: &str) -> Result<Vec<DiscoveredBox>, String> {
    serde_json::from_str::<Vec<DiscoveredBox>>(discover_json.trim())
        .map_err(|e| format!("could not parse `tt --json discover` output: {}", e))
}

/// True when `spec` already carries an explicit numeric port, so we should use
/// it verbatim rather than treating it as a discoverable box name.
///
/// * bracketed IPv6 with a port — `[::1]:8765` → true, `[::1]` → false
/// * `host:port` / `1.2.3.4:port` where the trailing component parses as a port
/// * a bare token with no `:` — `qb2-lab`, `myhost`, `192.168.1.5` → false
///
/// A bare token (false) is what triggers discovery-by-name; anything with an
/// explicit port bypasses discovery entirely (the fast, offline path).
pub fn looks_like_host_port(spec: &str) -> bool {
    let s = spec.trim();
    if let Some(rest) = s.strip_prefix('[') {
        // Bracketed IPv6: only a `]:PORT` suffix counts as host:port.
        return matches!(rest.split_once("]:"), Some((_, p)) if p.trim().parse::<u16>().is_ok());
    }
    match s.rsplit_once(':') {
        Some((host, port)) => !host.is_empty() && port.trim().parse::<u16>().is_ok(),
        None => false,
    }
}

/// Resolve a requested remote target against a discover-JSON payload.
///
/// This is the pure heart of Part 1 (`--remote <name>`):
///
/// * If `requested` is already a `HOST:PORT` (see [`looks_like_host_port`]),
///   it is returned unchanged — discovery is not even consulted.
/// * Otherwise `requested` is treated as a box **name** and matched against the
///   parsed list; a match yields `host:ctrl_port`.
/// * No match (or malformed JSON) returns a clear, human-readable error that
///   lists the names that *were* discovered, to help catch typos.
///
/// It never performs I/O, so tests inject the JSON directly.
pub fn resolve_remote_target(discover_json: &str, requested: &str) -> Result<String, String> {
    let requested = requested.trim();
    if requested.is_empty() {
        return Err("no remote box requested".to_string());
    }
    // Explicit address → passthrough, no discovery needed.
    if looks_like_host_port(requested) {
        return Ok(requested.to_string());
    }

    let boxes = parse_discovered_boxes(discover_json)?;
    if let Some(b) = boxes.iter().find(|b| b.name == requested) {
        return Ok(b.host_port());
    }

    let names: Vec<&str> = boxes.iter().map(|b| b.name.as_str()).collect();
    if names.is_empty() {
        Err(format!("no box named '{}' (none discovered)", requested))
    } else {
        Err(format!(
            "no box named '{}' (discovered: {})",
            requested,
            names.join(", ")
        ))
    }
}

/// The `tt` binary to invoke — overridable via `TT_BIN`, default `tt` (on PATH).
pub fn tt_bin() -> String {
    std::env::var("TT_BIN")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "tt".to_string())
}

/// Shell out to `tt --json discover` and return its raw stdout.
///
/// Impure (spawns a process); deliberately excluded from unit tests. Surfaces a
/// clear error when `tt` is missing from PATH or exits non-zero.
pub fn run_tt_discover() -> Result<String, String> {
    let bin = tt_bin();
    let output = std::process::Command::new(&bin)
        .args(["--json", "discover"])
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                format!(
                    "`{}` not found on PATH — install tt-station's `tt` CLI or set TT_BIN",
                    bin
                )
            } else {
                format!("failed to run `{} --json discover`: {}", bin, e)
            }
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!(
            "`{} --json discover` failed: {}",
            bin,
            stderr.trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Convenience for the `/remote` picker: run discovery and parse it in one step.
pub fn discover_boxes() -> Result<Vec<DiscoveredBox>, String> {
    let json = run_tt_discover()?;
    parse_discovered_boxes(&json)
}

/// Resolve a `--remote <spec>` to a concrete `HOST:PORT`, shelling out to `tt`
/// only when needed (Part 1 CLI wiring).
///
/// * `HOST:PORT` / `[IPv6]:PORT` → returned verbatim, no `tt` call.
/// * A bare token → resolved as a box **name** via `tt --json discover`.
///   To keep `--remote <bare-host>` working **exactly as it did before this
///   feature existed**, if discovery is unavailable (`tt` missing/errored) or
///   the name matches no box, we fall back to using the token as a bare host
///   (the WsBackend then defaults the port). The reason is logged. The strict
///   "return an error" contract lives in the pure [`resolve_remote_target`]
///   (and is what the TUI `/remote <name>` path surfaces to the user).
pub fn resolve_remote_spec(spec: &str) -> Result<String, String> {
    if looks_like_host_port(spec) {
        return Ok(spec.to_string());
    }
    match run_tt_discover() {
        Ok(json) => match resolve_remote_target(&json, spec) {
            Ok(target) => Ok(target),
            Err(e) => {
                log::warn!("--remote '{}': {}; falling back to bare host", spec, e);
                Ok(spec.to_string())
            }
        },
        Err(e) => {
            log::warn!(
                "--remote '{}': discovery unavailable ({}); using as bare host",
                spec,
                e
            );
            Ok(spec.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A realistic two-box `tt --json discover` payload.
    const DISCOVER_JSON: &str = r#"[
        {"name":"qb2-lab","host":"192.168.1.42","ctrl_port":8899,"chips":"4xBH","status":"idle","apiver":1},
        {"name":"qb-serving","host":"10.0.0.7","ctrl_port":8765,"chips":"8xWH","status":"serving:llama3","apiver":1}
    ]"#;

    // ── looks_like_host_port ────────────────────────────────────────────────

    #[test]
    fn host_port_detection() {
        assert!(looks_like_host_port("192.168.1.42:8765"));
        assert!(looks_like_host_port("myhost:8000"));
        assert!(looks_like_host_port("[::1]:8765"));
        // Bare tokens (names / bare hosts / bare IPs) are NOT host:port.
        assert!(!looks_like_host_port("qb2-lab"));
        assert!(!looks_like_host_port("myhost"));
        assert!(!looks_like_host_port("192.168.1.5"));
        assert!(!looks_like_host_port("[::1]"));
        // Non-numeric trailing component is not a port.
        assert!(!looks_like_host_port("host:notaport"));
    }

    // ── resolve_remote_target: match ────────────────────────────────────────

    #[test]
    fn resolves_name_to_host_port() {
        assert_eq!(
            resolve_remote_target(DISCOVER_JSON, "qb2-lab").unwrap(),
            "192.168.1.42:8899"
        );
        assert_eq!(
            resolve_remote_target(DISCOVER_JSON, "qb-serving").unwrap(),
            "10.0.0.7:8765"
        );
    }

    // ── resolve_remote_target: host:port passthrough (no discovery) ──────────

    #[test]
    fn host_port_passes_through_even_with_empty_json() {
        // An explicit address is returned verbatim; JSON is never consulted, so
        // even an empty/garbage payload is fine on the passthrough path.
        assert_eq!(
            resolve_remote_target("", "192.168.1.42:8765").unwrap(),
            "192.168.1.42:8765"
        );
        assert_eq!(
            resolve_remote_target("not json", "[::1]:9000").unwrap(),
            "[::1]:9000"
        );
    }

    // ── resolve_remote_target: no match ─────────────────────────────────────

    #[test]
    fn no_match_returns_clear_error_listing_names() {
        let err = resolve_remote_target(DISCOVER_JSON, "nope").unwrap_err();
        assert!(err.contains("nope"), "error should name the request: {err}");
        assert!(
            err.contains("qb2-lab"),
            "error should list discovered: {err}"
        );
        assert!(
            err.contains("qb-serving"),
            "error should list discovered: {err}"
        );
    }

    #[test]
    fn no_boxes_returns_clear_error() {
        let err = resolve_remote_target("[]", "qb2-lab").unwrap_err();
        assert!(err.contains("qb2-lab"));
        assert!(err.contains("none discovered"), "got: {err}");
    }

    // ── resolve_remote_target: malformed JSON ───────────────────────────────

    #[test]
    fn malformed_json_for_a_name_is_an_error() {
        // A name (not host:port) forces a parse; malformed JSON must error.
        let err = resolve_remote_target("{ this is not valid", "qb2-lab").unwrap_err();
        assert!(err.contains("parse"), "got: {err}");
    }

    #[test]
    fn empty_request_is_an_error() {
        assert!(resolve_remote_target(DISCOVER_JSON, "   ").is_err());
    }

    // ── parsing + display helpers ───────────────────────────────────────────

    #[test]
    fn parses_boxes_and_formats_summary() {
        let boxes = parse_discovered_boxes(DISCOVER_JSON).unwrap();
        assert_eq!(boxes.len(), 2);
        assert_eq!(boxes[0].name, "qb2-lab");
        assert_eq!(boxes[0].host_port(), "192.168.1.42:8899");
        let line = boxes[1].summary_line();
        assert!(line.contains("qb-serving"));
        assert!(line.contains("10.0.0.7:8765"));
        assert!(line.contains("8xWH"));
        assert!(line.contains("serving:llama3"));
    }

    #[test]
    fn lenient_parse_tolerates_missing_optional_fields() {
        // Only the required fields present; chips/status/apiver default.
        let json = r#"[{"name":"minimal","host":"h","ctrl_port":1234}]"#;
        let boxes = parse_discovered_boxes(json).unwrap();
        assert_eq!(boxes[0].host_port(), "h:1234");
        assert_eq!(boxes[0].chips, "");
        // summary_line renders placeholders rather than blanks.
        assert!(boxes[0].summary_line().contains("?"));
    }

    #[test]
    fn tt_bin_defaults_and_env_override() {
        // NOTE: reads/writes the process-global TT_BIN env var. Kept in one test
        // to avoid cross-test races on the shared environment.
        std::env::remove_var("TT_BIN");
        assert_eq!(tt_bin(), "tt");
        std::env::set_var("TT_BIN", "/opt/tt/bin/tt");
        assert_eq!(tt_bin(), "/opt/tt/bin/tt");
        // Blank is ignored → default.
        std::env::set_var("TT_BIN", "   ");
        assert_eq!(tt_bin(), "tt");
        std::env::remove_var("TT_BIN");
    }
}
