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
    /// Resolved real device indices for the targeted chip(s). Equals the
    /// indices of every known device when `is_full` (which need not be
    /// `0..total_devices` if the backend exposes sparse ids). A target token
    /// that couldn't be resolved (unknown BDF, junk) is dropped from this list
    /// but still counted in `raw_targets`/`chip_count` — never guessed.
    pub device_indices: Vec<usize>,
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
///
/// `tt-smi` is a Python entry-point script. Launched as `tt-smi -r` the
/// process name is `tt-smi`; launched as `python /path/to/tt-smi -r` the name
/// is `python` and the script is the first non-flag argument, which is also
/// accepted. Any other command that merely mentions `tt-smi` is not.
pub fn parse_reset_process(
    pid: i32,
    name: &str,
    cmdline: &str,
    devices: &[Device],
) -> Option<ResetEvent> {
    let tokens: Vec<&str> = cmdline.split_whitespace().collect();
    if !is_tt_smi_process(name, &tokens) {
        return None;
    }

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

    // The indices of every device the backend knows. Not assumed to be
    // `0..total_devices`: a backend can expose sparse ids.
    let known_indices: Vec<usize> = devices.iter().map(|d| d.index).collect();

    let resolved_indices: Vec<usize> = if is_all_literal {
        known_indices.clone()
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
        // Every known device must be among the resolved indices for is_full.
        let resolved_set: HashSet<_> = resolved_indices.iter().collect();
        known_indices.iter().all(|i| resolved_set.contains(i))
    };

    // When is_full, device_indices is every known device; otherwise the
    // resolved indices as given.
    let device_indices = if is_full {
        known_indices
    } else {
        resolved_indices
    };

    // A full reset resets every known device once, however the targets were
    // spelled (`0 1 1` on two devices is still two chips). A subset keeps the
    // raw target count.
    let chip_count = if is_full {
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

/// File name part of a path-like token.
fn base_name(tok: &str) -> &str {
    tok.rsplit('/').next().unwrap_or(tok)
}

/// True for a process named `tt-smi`, for one whose first argument is
/// `tt-smi`, or for a Python interpreter whose script (its first non-flag
/// argument) is `tt-smi`.
fn is_tt_smi_process(name: &str, tokens: &[&str]) -> bool {
    if base_name(name) == "tt-smi" {
        return true;
    }
    let Some(first) = tokens.first() else {
        return false;
    };
    if base_name(first) == "tt-smi" {
        return true;
    }
    if base_name(first).starts_with("python") {
        let script = tokens[1..].iter().find(|t| !t.starts_with('-'));
        return script.is_some_and(|t| base_name(t) == "tt-smi");
    }
    false
}

/// Resolve each raw TARGETS token to a real device index: a bare integer or
/// `/dev/tenstorrent/<n>` is taken as the UMD logical id directly; anything
/// else is matched against each device's real PCI bus id. Unresolvable
/// tokens are silently dropped — never guessed.
fn resolve_target_indices(targets: &[String], devices: &[Device]) -> Vec<usize> {
    let mut out = Vec::new();
    for t in targets {
        let idx = if let Some(rest) = t.strip_prefix("/dev/tenstorrent/") {
            rest.parse::<usize>().ok()
        } else if let Ok(n) = t.parse::<usize>() {
            Some(n)
        } else {
            devices
                .iter()
                .find(|d| d.bus_id.eq_ignore_ascii_case(t))
                .map(|d| d.index)
        };
        if let Some(i) = idx {
            out.push(i);
        }
    }
    out
}

/// The reset a takeover animation is waiting on, if any. Holds at most one.
///
/// The TUI finds resets with `ui::tui::reset_status::ResetStatus`, which
/// follows every live `tt-smi -r` process and drives the status segment and
/// HivemindSweeper's feed events. This type only tracks the takeover:
/// `begin()` records the reset a new takeover is for, `is_active()` says a
/// takeover is waiting on one (so a second reset gets no takeover of its
/// own, see the design spec's "overlapping resets" non-goal),
/// `is_finished()` says when that reset's process has exited, and `clear()`
/// runs when the animation ends, which frees it for the next reset.
pub struct ResetDetector {
    /// The tracked reset and the cmdline it was seen with. The process is
    /// identified by pid and cmdline together, as `ResetStatus` does, so a pid
    /// the kernel reuses for an unrelated process is not mistaken for it.
    active: Option<(ResetEvent, String)>,
}

impl ResetDetector {
    pub fn new() -> Self {
        Self { active: None }
    }

    /// Records `ev` (seen with `cmdline`) as the reset a takeover animation
    /// is waiting on. Does nothing if one is already tracked.
    pub fn begin(&mut self, ev: ResetEvent, cmdline: &str) {
        if self.active.is_none() {
            self.active = Some((ev, cmdline.to_string()));
        }
    }

    /// True while a reset is tracked (between `begin` and `clear`).
    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    /// True once no process in `processes` has the tracked pid AND cmdline.
    /// A different command under the same pid (the kernel reused it) does not
    /// keep the reset alive. `false` if nothing is being tracked.
    pub fn is_finished(&self, processes: &[(i32, String, String)]) -> bool {
        match &self.active {
            Some((ev, cmdline)) => !processes
                .iter()
                .any(|(pid, _, cmd)| *pid == ev.pid && cmd == cmdline),
            None => false,
        }
    }

    /// Stops tracking: the takeover has ended. The next `begin` is accepted.
    pub fn clear(&mut self) {
        self.active = None;
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
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0000:ff:00.0", &devices).unwrap();
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
        assert!(
            !ev.is_full,
            "Duplicated target should not count as full coverage"
        );
        assert_eq!(
            ev.chip_count, 2,
            "chip_count should reflect the raw target count"
        );
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

    /// A detector tracking the reset `tt-smi -r 0` (pid 500).
    fn tracking_500(devices: &[Device]) -> ResetDetector {
        let mut det = ResetDetector::new();
        det.begin(
            parse_reset_process(500, "tt-smi", "tt-smi -r 0", devices).unwrap(),
            "tt-smi -r 0",
        );
        det
    }

    #[test]
    fn is_finished_false_while_pid_present() {
        let devices = fixture_devices(4);
        let det = tracking_500(&devices);
        let live = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        assert!(!det.is_finished(&live));
    }

    #[test]
    fn is_finished_true_once_pid_gone() {
        let devices = fixture_devices(4);
        let det = tracking_500(&devices);
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
    fn begin_tracks_one_reset_until_clear() {
        let devices = fixture_devices(4);
        let a = parse_reset_process(500, "tt-smi", "tt-smi -r 0", &devices).unwrap();
        let b = parse_reset_process(600, "tt-smi", "tt-smi -r 1", &devices).unwrap();
        let mut det = ResetDetector::new();
        assert!(!det.is_active());
        det.begin(a, "tt-smi -r 0");
        assert!(det.is_active());
        // A second begin while one is tracked is ignored: 500 is still the
        // pid whose exit finishes the takeover.
        det.begin(b.clone(), "tt-smi -r 1");
        assert!(!det.is_finished(&procs(&[(500, "tt-smi", "tt-smi -r 0")])));
        assert!(det.is_finished(&procs(&[(600, "tt-smi", "tt-smi -r 1")])));
        det.clear();
        assert!(!det.is_active());
        det.begin(b, "tt-smi -r 1");
        assert!(!det.is_finished(&procs(&[(600, "tt-smi", "tt-smi -r 1")])));
    }

    #[test]
    fn clear_allows_a_later_reset_to_be_tracked() {
        let devices = fixture_devices(4);
        let mut det = tracking_500(&devices);
        det.clear();
        assert!(!det.is_finished(&procs(&[])), "nothing tracked after clear");
        det.begin(
            parse_reset_process(600, "tt-smi", "tt-smi -r 1", &devices).unwrap(),
            "tt-smi -r 1",
        );
        assert!(det.is_active());
        assert!(det.is_finished(&procs(&[(500, "tt-smi", "tt-smi -r 0")])));
    }

    // ---- Python-launched tt-smi ------------------------------------------

    #[test]
    fn python_interpreter_form_is_recognized() {
        // `python /path/to/tt-smi -r 2`: the process name is `python`.
        let devices = fixture_devices(4);
        let ev = parse_reset_process(
            100,
            "python",
            "/home/u/.venv/bin/python /home/u/.venv/bin/tt-smi -r 2",
            &devices,
        )
        .unwrap();
        assert_eq!(ev.device_indices, vec![2]);
        // Interpreter flags before the script are skipped.
        let ev = parse_reset_process(100, "python3", "python3 -u /x/tt-smi -r", &devices).unwrap();
        assert!(ev.is_full);
    }

    #[test]
    fn other_commands_that_mention_tt_smi_are_not_resets() {
        let devices = fixture_devices(4);
        // The script is not tt-smi.
        assert!(parse_reset_process(1, "python", "python train.py tt-smi -r", &devices).is_none());
        // Not a Python interpreter, and tt-smi is only an argument.
        assert!(parse_reset_process(1, "vim", "vim tt-smi -r", &devices).is_none());
        assert!(parse_reset_process(1, "grep", "grep -r tt-smi .", &devices).is_none());
        // A Python tt-smi that is not a reset.
        assert!(parse_reset_process(1, "python", "python /x/tt-smi -s", &devices).is_none());
    }

    // ---- Device ids beyond u8, and sparse ids ----------------------------

    #[test]
    fn exactly_256_devices_does_not_make_every_reset_full() {
        // `total_devices as u8` was 0 here, so the coverage check passed
        // vacuously and `-r 0` was classed as a full reset.
        let devices = fixture_devices(256);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0", &devices).unwrap();
        assert!(!ev.is_full);
        assert_eq!(ev.device_indices, vec![0]);
        let all = parse_reset_process(100, "tt-smi", "tt-smi -r", &devices).unwrap();
        assert!(all.is_full);
        assert_eq!(all.device_indices.len(), 256);
    }

    #[test]
    fn indices_above_255_are_kept() {
        let devices = fixture_devices(300);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 299 256", &devices).unwrap();
        assert_eq!(ev.device_indices, vec![299, 256]);
        assert!(!ev.is_full);
    }

    fn sparse_devices(ids: &[usize]) -> Vec<Device> {
        ids.iter()
            .map(|&i| {
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
    fn sparse_device_ids_are_judged_against_the_ids_that_exist() {
        // Devices 1 and 2 only. A reset of 2 is a subset; naming both is full;
        // an omitted target resets both, and lists those two ids.
        let devices = sparse_devices(&[1, 2]);
        let one = parse_reset_process(100, "tt-smi", "tt-smi -r 2", &devices).unwrap();
        assert!(!one.is_full);
        assert_eq!(one.device_indices, vec![2]);
        let both = parse_reset_process(100, "tt-smi", "tt-smi -r 1 2", &devices).unwrap();
        assert!(both.is_full);
        let all = parse_reset_process(100, "tt-smi", "tt-smi -r", &devices).unwrap();
        assert_eq!(all.device_indices, vec![1, 2]);
    }

    // ---- Reused pids -----------------------------------------------------

    #[test]
    fn a_reused_pid_running_another_command_does_not_keep_the_reset_alive() {
        let devices = fixture_devices(4);
        let det = tracking_500(&devices);
        // tt-smi exited and the kernel gave pid 500 to something else.
        let reused = procs(&[(500, "nginx", "nginx -g daemon off;")]);
        assert!(det.is_finished(&reused));
        // The same pid with the same cmdline is still the reset.
        let same = procs(&[(500, "tt-smi", "tt-smi -r 0")]);
        assert!(!det.is_finished(&same));
    }

    #[test]
    fn full_enumeration_with_duplicates_still_counts_each_chip_once() {
        // `0 1 1` on two devices covers everything: a full reset of 2 chips,
        // not 3, and device_indices is the two devices.
        let devices = fixture_devices(2);
        let ev = parse_reset_process(100, "tt-smi", "tt-smi -r 0 1 1", &devices).unwrap();
        assert!(ev.is_full);
        assert_eq!(ev.chip_count, 2);
        assert_eq!(ev.device_indices, vec![0, 1]);
        // A subset still reports the raw target count (see the `0 0` test).
        let sub = parse_reset_process(100, "tt-smi", "tt-smi -r 0 0", &devices).unwrap();
        assert_eq!(sub.chip_count, 2);
    }
}
