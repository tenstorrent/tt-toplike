// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Ownership guard for `/proc/<pid>` reads.
//!
//! # Why this exists
//!
//! Several collectors walk every process on the box and read files under
//! `/proc/<pid>` that only the owning user — or root — may open: `environ`,
//! `io`, `fd/`, `maps`. For a process this user does not own, the open fails
//! with `EACCES` and every caller already degrades to an empty value. The data
//! was therefore never obtainable, and skipping the attempt changes no
//! behaviour.
//!
//! The attempt is not free, though. On a host running the STIG audit ruleset,
//! `-F exit=-EACCES -k access` records every denied open as a four-record audit
//! event of roughly 1.2 KB. These scans run every 2 seconds across every
//! process, and `HostProcessMonitor` walks every *thread*. Measured on a
//! 1,000-thread box: ~1,400 denials/s, about 4 GB of audit log per hour. That
//! filled a 20 GB audit volume in one afternoon, at which point auditd stopped
//! logging host-wide — a compliance outage caused entirely by reads that could
//! never have succeeded.
//!
//! [`owned_by_us`] answers the question with a `stat` of `/proc/<pid>`, which
//! is world readable and is not covered by the open-family audit rules, so the
//! check itself is silent.

/// Non-Linux targets have no `/proc`, so there are no denied `/proc` opens to
/// avoid and nothing for this guard to decide. Staying permissive leaves every
/// caller behaving exactly as it did before the guard existed: it attempts the
/// read, that read fails harmlessly, and the caller degrades to an empty value.
///
/// `probe.rs` compiles on macOS and Windows, so this arm is what keeps the
/// guard callable from unconditional code.
#[cfg(not(target_os = "linux"))]
pub fn owned_by_us(_pid: i32) -> bool {
    true
}

/// Non-Linux: no `/proc`, so there is no denial to remember (see above).
#[cfg(not(target_os = "linux"))]
pub fn may_read(_pid: i32) -> bool {
    true
}

/// Non-Linux: no-op, kept so callers need no `cfg` of their own.
#[cfg(not(target_os = "linux"))]
pub fn note_denied(_pid: i32) {}

/// Non-Linux: no-op, kept so callers need no `cfg` of their own.
#[cfg(not(target_os = "linux"))]
pub fn note_io_error(_pid: i32, _err: &std::io::Error) {}

#[cfg(target_os = "linux")]
use std::sync::OnceLock;

/// This process's uid, resolved once. `None` if `/proc/self` cannot be read,
/// in which case callers fall back to attempting nothing.
#[cfg(target_os = "linux")]
fn our_uid() -> Option<u32> {
    static UID: OnceLock<Option<u32>> = OnceLock::new();
    *UID.get_or_init(|| {
        use std::os::unix::fs::MetadataExt;
        std::fs::metadata("/proc/self").ok().map(|m| m.uid())
    })
}

/// True when the restricted files under `/proc/<pid>` are actually readable by
/// this process: either we own the target, or we are root.
///
/// A pid that has exited between enumeration and this call returns `false`,
/// which is the correct answer — there is nothing left to read.
#[cfg(target_os = "linux")]
pub fn owned_by_us(pid: i32) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Some(me) = our_uid() else {
        return false;
    };
    // root may read any process, so the guard must not cost root the data.
    if me == 0 {
        return true;
    }
    std::fs::metadata(format!("/proc/{pid}"))
        .map(|m| m.uid() == me)
        .unwrap_or(false)
}

// ── Negative cache ───────────────────────────────────────────────────────────
//
// Ownership is necessary but not sufficient. A process this user owns can
// still refuse `fd/`, `maps` and `environ`: the kernel applies a ptrace-style
// access check (`PTRACE_MODE_READ_FSCREDS`) on top of the file mode, so a
// setuid binary, a process that dropped privileges, a non-dumpable process, or
// a Yama `ptrace_scope` restriction all yield `EACCES` for a pid we own. That
// was the residual ~24 denials per 14 s after the ownership guard alone.
//
// The answer for one pid does not change from one 2 s scan to the next, so a
// denial is remembered per pid and the pid is skipped for `DENIED_TTL`. The
// first failure costs one audit record; the following scans cost none. The TTL
// bounds how long a recycled pid, or a process that later became readable, can
// be wrongly skipped.

/// How long a denied pid is skipped before one more attempt is allowed.
#[cfg(target_os = "linux")]
const DENIED_TTL: std::time::Duration = std::time::Duration::from_secs(60);

/// Prune expired entries once the map grows past this, so a long run on a busy
/// host cannot grow it without bound.
#[cfg(target_os = "linux")]
const DENIED_PRUNE_AT: usize = 4096;

#[cfg(target_os = "linux")]
fn denied() -> &'static std::sync::Mutex<std::collections::HashMap<i32, std::time::Instant>> {
    static DENIED: OnceLock<std::sync::Mutex<std::collections::HashMap<i32, std::time::Instant>>> =
        OnceLock::new();
    DENIED.get_or_init(Default::default)
}

/// True if `pid` was denied within the last [`DENIED_TTL`].
#[cfg(target_os = "linux")]
fn denied_recently(pid: i32) -> bool {
    let map = denied().lock().unwrap_or_else(|e| e.into_inner());
    map.get(&pid).is_some_and(|t| t.elapsed() < DENIED_TTL)
}

/// Remember that a restricted `/proc/<pid>` read was refused, so later scans
/// skip this pid for [`DENIED_TTL`]. Call it wherever a guarded read still
/// fails with a permission error.
#[cfg(target_os = "linux")]
pub fn note_denied(pid: i32) {
    let mut map = denied().lock().unwrap_or_else(|e| e.into_inner());
    if map.len() >= DENIED_PRUNE_AT {
        map.retain(|_, t| t.elapsed() < DENIED_TTL);
    }
    map.insert(pid, std::time::Instant::now());
}

/// [`note_denied`] when `err` is a permission error; any other error (the pid
/// exited, the file is absent) says nothing about access and is ignored.
#[cfg(target_os = "linux")]
pub fn note_io_error(pid: i32, err: &std::io::Error) {
    if err.kind() == std::io::ErrorKind::PermissionDenied {
        note_denied(pid);
    }
}

/// The question every restricted `/proc/<pid>` read should ask first: is it
/// worth attempting? True when we own the pid (or are root) and it has not
/// refused us recently.
#[cfg(target_os = "linux")]
pub fn may_read(pid: i32) -> bool {
    owned_by_us(pid) && !denied_recently(pid)
}

/// Whether a denial is currently remembered for `pid`. Exists so tests can
/// observe the wiring: a guarded caller never attempts a foreign pid, so never
/// records a denial for it.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn is_denied_for_test(pid: i32) -> bool {
    denied_recently(pid)
}

/// A pid this process cannot read, for wiring tests: pid 1 when it is owned by
/// someone else and we are not root. `None` (skip the test) otherwise, e.g. in a
/// root or rootless-userns container where pid 1 is readable and a denial could
/// never occur.
///
/// These tests assert that a guarded caller never *attempts* the read. A caller
/// that did attempt it would get `EACCES`, call [`note_denied`], and leave the
/// pid in the cache, so `!is_denied_for_test(pid)` is a check on the wiring and
/// not on the arithmetic of [`owned_by_us`].
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn foreign_pid_for_test() -> Option<i32> {
    if owned_by_us(1) || !std::path::Path::new("/proc/1").exists() {
        return None;
    }
    clear_denied_for_test(1);
    Some(1)
}

/// Remove a remembered denial. Test-only.
#[cfg(all(test, target_os = "linux"))]
pub(crate) fn clear_denied_for_test(pid: i32) {
    denied()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&pid);
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Our own process is readable; pid 1 is root-owned and is not, unless the
    /// suite itself runs as root, where the guard must stay permissive.
    #[test]
    fn owns_self_and_declines_other_users() {
        let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        assert!(owned_by_us(me), "we must recognise our own process");

        // Expected answer comes from pid 1's real owner. It is root on a host,
        // but inside a container it can belong to the user running the tests,
        // and root may read anything (the guard's fast path).
        use std::os::unix::fs::MetadataExt;
        let running_as_root = our_uid() == Some(0);
        let pid1_is_ours = std::fs::metadata("/proc/1")
            .map(|m| Some(m.uid()) == our_uid())
            .unwrap_or(false);
        assert_eq!(
            owned_by_us(1),
            running_as_root || pid1_is_ours,
            "pid 1 is readable only when we are root or own it"
        );
    }

    /// A pid that cannot exist is declined rather than attempted. Root is the
    /// exception: the guard answers `true` for uid 0 before looking at the pid,
    /// so a root test run expects `true` here.
    #[test]
    fn declines_a_nonexistent_pid() {
        assert_eq!(owned_by_us(i32::MAX), our_uid() == Some(0));
    }

    /// A remembered denial blocks reads of that pid, and only that pid.
    ///
    /// The cache is process-wide and tests run in parallel, so this must never
    /// deny the test runner's own pid: other tests read it. A throwaway child
    /// owned by us passes the ownership check, so only the cache can decline it.
    #[test]
    fn denial_is_remembered_per_pid() {
        let mut child = std::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn a throwaway child");
        let pid = i32::try_from(child.id()).unwrap_or(i32::MAX);
        let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);

        assert!(may_read(pid), "child readable before any denial");
        note_denied(pid);
        assert!(!may_read(pid), "child declined after a denial");
        assert!(may_read(me), "a denial for one pid must not affect another");
        clear_denied_for_test(pid);
        assert!(may_read(pid), "child readable once the entry is cleared");

        let _ = child.kill();
        let _ = child.wait();
    }

    /// Only a permission error is remembered. A vanished pid is not a denial.
    #[test]
    fn only_permission_errors_are_remembered() {
        let pid = i32::MAX - 1;
        clear_denied_for_test(pid);
        note_io_error(pid, &std::io::Error::from(std::io::ErrorKind::NotFound));
        assert!(!is_denied_for_test(pid));
        note_io_error(
            pid,
            &std::io::Error::from(std::io::ErrorKind::PermissionDenied),
        );
        assert!(is_denied_for_test(pid));
        clear_denied_for_test(pid);
    }
}
