// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Detects a `tt-smi -r`/`--reset` invocation from the host process list and
//! classifies it as a full-box or subset reset. See
//! docs/superpowers/specs/2026-09-28-reset-takeover-design.md.

use std::collections::HashSet;

use crate::models::device::Device;

/// A detected `tt-smi -r` invocation, still in progress or just finished.
#[derive(Debug, Clone)]
pub struct ResetEvent {
    pub pid: i32,
    /// True if this targets every device currently known (omitted targets,
    /// literal `all`, or an explicit list that happens to enumerate every
    /// known device).
    pub is_full: bool,
    /// Number of chips actually targeted (== `total_devices` when `is_full`).
    pub chip_count: usize,
    /// Total devices known on this box right now (from the active backend).
    pub total_devices: usize,
    /// Resolved real device indices for the targeted chip(s). Equals
    /// `0..total_devices` when `is_full`. A target token that couldn't be
    /// resolved (unknown BDF, junk) is dropped from this list but still
    /// counted in `raw_targets`/`chip_count` — never guessed.
    pub device_indices: Vec<u8>,
    /// The original TARGETS tokens, for display/log text.
    pub raw_targets: Vec<String>,
}

/// Recognizes a `tt-smi -r [TARGETS...]` / `--reset [TARGETS...]` invocation
/// from a process's name + full cmdline, per the real `tt-smi --help`
/// grammar: `-r [TARGETS ...]` — omitted or literal `all` means every
/// device; otherwise whitespace- or comma-separated UMD logical IDs, PCI
/// BDFs (e.g. `0000:0a:00.0`), or `/dev/tenstorrent/<id>`. Returns `None`
/// for any other `tt-smi` invocation (`-s`, `-ls`, etc.) or a non-`tt-smi`
/// process. `devices` is the active backend's current device list, used to
/// know the true device count and to resolve each target token to a real
/// device index.
pub fn parse_reset_process(
    pid: i32,
    name: &str,
    cmdline: &str,
    devices: &[Device],
) -> Option<ResetEvent> {
    let base = name.rsplit('/').next().unwrap_or(name);
    let first_tok = cmdline.split_whitespace().next().unwrap_or("");
    let first_base = first_tok.rsplit('/').next().unwrap_or(first_tok);
    if base != "tt-smi" && first_base != "tt-smi" {
        return None;
    }

    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    let flag_idx = tokens.iter().position(|t| *t == "-r" || *t == "--reset")?;

    let mut raw_targets: Vec<String> = Vec::new();
    for tok in &tokens[flag_idx + 1..] {
        if tok.starts_with('-') {
            break;
        }
        for piece in tok.split(',') {
            if !piece.is_empty() {
                raw_targets.push(piece.to_string());
            }
        }
    }

    let total_devices = devices.len();
    let is_all_literal =
        raw_targets.is_empty() || raw_targets.iter().any(|t| t.eq_ignore_ascii_case("all"));

    let resolved_indices: Vec<u8> = if is_all_literal {
        (0..total_devices).filter_map(|i| u8::try_from(i).ok()).collect()
    } else {
        resolve_target_indices(&raw_targets, devices)
    };

    // Check if the resolved indices actually cover all devices (coverage-based, not count-based).
    // A duplicated target (e.g., `tt-smi -r 0 0` on a 2-chip box) should not count as full.
    let is_full = if is_all_literal {
        true
    } else if total_devices == 0 {
        false
    } else {
        // All distinct indices must be present in 0..total_devices for is_full to be true.
        let resolved_set: HashSet<_> = resolved_indices.iter().collect();
        (0..total_devices as u8).all(|i| resolved_set.contains(&i))
    };

    // When is_full, device_indices should be 0..total_devices; otherwise use resolved indices.
    let device_indices = if is_full {
        (0..total_devices).filter_map(|i| u8::try_from(i).ok()).collect()
    } else {
        resolved_indices
    };

    let chip_count = if is_all_literal {
        total_devices
    } else {
        raw_targets.len()
    };

    Some(ResetEvent {
        pid,
        is_full,
        chip_count,
        total_devices,
        device_indices,
        raw_targets,
    })
}

/// Resolve each raw TARGETS token to a real device index: a bare integer or
/// `/dev/tenstorrent/<n>` is taken as the UMD logical id directly; anything
/// else is matched against each device's real PCI bus id. Unresolvable
/// tokens are silently dropped — never guessed.
fn resolve_target_indices(targets: &[String], devices: &[Device]) -> Vec<u8> {
    let mut out = Vec::new();
    for t in targets {
        let idx = if let Some(rest) = t.strip_prefix("/dev/tenstorrent/") {
            rest.parse::<usize>().ok()
        } else if let Ok(n) = t.parse::<usize>() {
            Some(n)
        } else {
            devices.iter().find(|d| d.bus_id.eq_ignore_ascii_case(t)).map(|d| d.index)
        };
        if let Some(i) = idx {
            if let Ok(b) = u8::try_from(i) {
                out.push(b);
            }
        }
    }
    out
}

/// Tracks at most one active `tt-smi -r` invocation at a time. A second
/// reset detected while one is already tracked is dropped (see design
/// spec's "overlapping resets" non-goal) — call `clear()` once its visual
/// consequence (a takeover animation, or a HivemindSweeper injection) is
/// also done, not merely once the process itself exits.
///
/// `clear()` is routinely called well before the real `tt-smi -r` process
/// actually exits (HivemindSweeper clears right after injecting a feed
/// event; a skipped takeover clears the instant the user hits a key) — the
/// visual consequence finishing has nothing to do with the process's own
/// lifetime. Without remembering which pid was already handled,
/// `observe()` would re-detect that same still-running invocation on the
/// very next scan and treat it as a brand-new reset. `last_handled_pid`
/// closes that gap: it survives `clear()` and is only replaced once a
/// *different* pid is observed, so the same invocation is never reported
/// twice, while a later, genuinely different `tt-smi -r` (a new pid) is
/// still detected normally.
pub struct ResetDetector {
    active: Option<ResetEvent>,
    last_handled_pid: Option<i32>,
}

impl ResetDetector {
    pub fn new() -> Self {
        Self {
            active: None,
            last_handled_pid: None,
        }
    }

    /// Scans `processes` — `(pid, name, cmdline)` triples for every process
    /// currently visible, e.g. from
    /// `HostProcessMonitor::processes_snapshot()` — for a new `tt-smi -r`
    /// invocation. Returns `None` if one is already tracked, if none is
    /// found, or if the only match is the pid most recently handled (its
    /// visual consequence already ran and was `clear()`-ed, but the
    /// process itself may still be alive).
    pub fn observe(
        &mut self,
        processes: &[(i32, String, String)],
        devices: &[Device],
    ) -> Option<&ResetEvent> {
        if self.active.is_some() {
            return None;
        }
        for (pid, name, cmdline) in processes {
            if self.last_handled_pid == Some(*pid) {
                continue;
            }
            if let Some(ev) = parse_reset_process(*pid, name, cmdline, devices) {
                self.active = Some(ev);
                return self.active.as_ref();
            }
        }
        None
    }

    /// True once the tracked pid is no longer present in `processes`.
    /// `false` if nothing is being tracked.
    pub fn is_finished(&self, processes: &[(i32, String, String)]) -> bool {
        match &self.active {
            Some(ev) => !processes.iter().any(|(pid, _, _)| *pid == ev.pid),
            None => false,
        }
    }

    /// Ends tracking of the current reset's visual consequence. Remembers
    /// its pid (see [`ResetDetector`] docs) so `observe()` doesn't
    /// re-detect the same still-running invocation as a new one.
    pub fn clear(&mut self) {
        if let Some(ev) = self.active.take() {
            self.last_handled_pid = Some(ev.pid);
        }
    }
}

impl Default for ResetDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture_devices(n: usize) -> Vec<Device> {
        (0..n)
            .map(|i| {
                Device::new(
                    i,
                    "wormhole".to_string(),
                    format!("0000:{i:02x}:00.0"),
                    format!("({i})"),
                )
            })
            .collect()
    }

    #[test]
    fn ignores_non_tt_smi_process() {
        let devices = fixture_devices(4);
        assert!(parse_reset_process(100, "bash", "bash -c ls", &devices).is_none());
    }

    #[test]
    fn ignores_tt_smi_snapshot_invocation() {
        let devices = fixture_devices(4);
        assert!(parse_reset_process(100, "tt-smi", "tt-smi -s", &devices).is_none());
    }

    #[test]
    fn omitted_targets_is_full_reset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.chip_count, 4);
        assert_eq!(ev.device_indices, vec![0, 1, 2, 3]);
        assert_eq!(ev.pid, 100);
    }

    #[test]
    fn literal_all_is_full_reset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi --reset all", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.total_devices, 4);
    }

    #[test]
    fn single_umd_id_is_subset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 2", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.chip_count, 1);
        assert_eq!(ev.device_indices, vec![2]);
    }

    #[test]
    fn space_separated_targets_are_subset() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 2", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0, 2]);
    }

    #[test]
    fn explicit_enumeration_of_every_device_counts_as_full() {
        // The trickiest boundary: not `all`, not omitted, but happens to name
        // every known device — the design spec calls this out explicitly.
        let devices = fixture_devices(2);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 1", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.chip_count, 2);
    }

    #[test]
    fn bdf_target_resolves_to_device_index() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0000:02:00.0", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![2]);
    }

    #[test]
    fn dev_tenstorrent_path_target_resolves_to_device_index() {
        let devices = fixture_devices(4);
        let ev =
            parse_reset_process(100, "tt-smi", "tt-smi -r /dev/tenstorrent/3", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![3]);
    }

    #[test]
    fn comma_separated_targets_are_split() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0,1", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0, 1]);
        assert_eq!(ev.raw_targets, vec!["0".to_string(), "1".to_string()]);
    }

    #[test]
    fn unresolvable_target_is_dropped_from_device_indices_but_still_counted() {
        let devices = fixture_devices(4);
        let ev =
            parse_reset_process(100, "tt-smi", "tt-smi -r 0000:ff:00.0", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.chip_count, 1); // one token was asked for...
        assert!(ev.device_indices.is_empty()); // ...but it couldn't be resolved, never guessed
    }

    #[test]
    fn trailing_flag_after_targets_does_not_get_consumed_as_a_target() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 1 --use_luwen", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0, 1]);
    }

    #[test]
    fn absolute_path_process_name_is_recognized() {
        let devices = fixture_devices(4);
        let ev = parse_reset_process(100, "tt-smi", "/usr/local/bin/tt-smi -r", &devices).unwrap();
        assert!(ev.is_full);
    }

    #[test]
    fn duplicated_target_does_not_falsely_claim_full_coverage() {
        // Regression test: on a 2-chip box, `tt-smi -r 0 0` should NOT be a full reset.
        // Only device 0 is actually targeted, so is_full must be false.
        let devices = fixture_devices(2);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 0", &devices).unwrap();
        assert!(!ev.is_full, "Duplicated target should not count as full coverage");
        assert_eq!(ev.chip_count, 2, "chip_count should reflect the raw target count");
        assert_eq!(
            ev.device_indices,
            vec![0, 0],
            "device_indices should preserve duplicates"
        );
    }

    fn procs(entries: &[(i32, &str, &str)]) -> Vec<(i32, String, String)> {
        entries
            .iter()
            .map(|(pid, name, cmd)| (*pid, name.to_string(), cmd.to_string()))
            .collect()
    }

    #[test]
    fn detector_observes_a_new_reset() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(1, "bash", "bash"), (500, "tt-smi", "tt-smi -r 1")]);
        let ev = det.observe(&live, &devices).expect("should detect reset");
        assert_eq!(ev.pid, 500);
        assert_eq!(ev.device_indices, vec![1]);
    }

    #[test]
    fn detector_ignores_overlapping_second_reset() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let first = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        assert!(det.observe(&first, &devices).is_some());

        let second = procs(&[(500, "tt-smi", "tt-smi -r 0"), (600, "tt-smi", "tt-smi -r 1")]);
        assert!(det.observe(&second, &devices).is_none());
    }

    #[test]
    fn is_finished_false_while_pid_present() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&live, &devices);
        assert!(!det.is_finished(&live));
    }

    #[test]
    fn is_finished_true_once_pid_gone() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&live, &devices);
        let gone = procs(&[(1, "bash", "bash")]);
        assert!(det.is_finished(&gone));
    }

    #[test]
    fn is_finished_false_with_nothing_tracked() {
        let det = ResetDetector::new();
        let any = procs(&[(1, "bash", "bash")]);
        assert!(!det.is_finished(&any));
    }

    #[test]
    fn clear_allows_a_later_reset_to_be_tracked() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let first = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&first, &devices);
        det.clear();
        let second = procs(&[(600, "tt-smi", "tt-smi -r 1")]);
        let ev = det.observe(&second, &devices).expect("should detect after clear");
        assert_eq!(ev.pid, 600);
    }

    /// Regression: `clear()` is called well before the real `tt-smi -r`
    /// process exits (HivemindSweeper clears right after injecting a feed
    /// event; a skipped takeover clears immediately). If the same
    /// still-running pid shows up on the very next scan, it must NOT be
    /// re-detected as a brand-new reset — that would flood HivemindSweeper's
    /// feed with duplicate injections, or instantly re-show a takeover the
    /// user just skipped.
    #[test]
    fn clear_does_not_let_the_same_still_alive_pid_be_redetected() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let live = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&live, &devices)
            .expect("should detect the reset");
        det.clear();

        // Same pid, still present in the process list (the real `tt-smi -r`
        // process commonly outlives the visual consequence's `clear()`).
        assert!(
            det.observe(&live, &devices).is_none(),
            "the same already-handled pid must not be reported as a new reset"
        );
    }

    /// Companion to the regression above: dedup is by pid identity, not a
    /// blanket "never detect again" — once a genuinely different `tt-smi -r`
    /// invocation (a different pid) shows up, it must still be detected.
    #[test]
    fn clear_still_allows_a_different_pid_to_be_detected_afterward() {
        let devices = fixture_devices(4);
        let mut det = ResetDetector::new();
        let first = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        det.observe(&first, &devices)
            .expect("should detect the first reset");
        det.clear();

        let second = procs(&[
            (500, "tt-smi", "tt-smi -r 0"),
            (600, "tt-smi", "tt-smi -r 1"),
        ]);
        let ev = det
            .observe(&second, &devices)
            .expect("a different pid must still be detected as a new reset");
        assert_eq!(ev.pid, 600);
    }
}
