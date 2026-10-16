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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Our own process is readable; pid 1 is root-owned and is not, unless the
    /// suite itself runs as root, where the guard must stay permissive.
    #[test]
    fn owns_self_and_declines_other_users() {
        let me = i32::try_from(std::process::id()).unwrap_or(i32::MAX);
        assert!(owned_by_us(me), "we must recognise our own process");

        let running_as_root = our_uid() == Some(0);
        assert_eq!(
            owned_by_us(1),
            running_as_root,
            "pid 1 is root-owned: readable only when we are root"
        );
    }

    /// A pid that cannot exist must be declined rather than attempted.
    #[test]
    fn declines_a_nonexistent_pid() {
        assert!(!owned_by_us(i32::MAX));
    }
}
