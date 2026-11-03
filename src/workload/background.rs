// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Lower scheduling priority for helper threads that start no programs, so
//! they yield the CPU to the thread that draws the frames.
//!
//! The TUI starts several helper threads at once. On a box with more
//! runnable work than cores, each competes with the render thread for a CPU
//! and frames come late. [`lower_priority`] raises the calling thread's nice
//! value by [`NICE_STEP`]. The render thread keeps its own value. A lowered
//! thread still gets a whole core when one is free, and waits only when
//! something at normal priority wants the same core.
//!
//! # Only threads that start no programs
//!
//! On Linux the nice value belongs to the thread, and a child process
//! inherits the value of the thread that started it. An unprivileged process
//! cannot lower a nice value again (that needs `CAP_SYS_NICE`), so a child
//! cannot be put back at normal priority before or after `exec`. Several
//! helpers run programs under a wall-clock timeout: `gozer status` (2 s),
//! `docker` (4 s), `curl`, `tt-smi -s`. At nice 10 on a loaded box such a
//! program gets a few percent of a core and can time out on every poll. So
//! only threads that start no programs are lowered:
//!
//! * the process scan worker (`tt-toplike-proc-scan`): `/proc` and sysinfo
//!   reads, and HTTP probes of local serving endpoints;
//! * the liveness prober (`tt-liveness-probe`): HTTP probes only.
//!
//! The tt-smi reader, the inference-server monitor, the catalog refresher and
//! the gozer poll run at normal priority.
//!
//! On other platforms it does nothing.

/// How far a helper thread's nice value is raised (capped at 19).
pub const NICE_STEP: i32 = 10;

/// Raise the calling thread's nice value by [`NICE_STEP`]. Best effort: a
/// failure leaves the thread as it was.
#[cfg(target_os = "linux")]
pub fn lower_priority() {
    // SAFETY: gettid, getpriority and setpriority take plain integers and
    // touch only this thread's scheduling attributes.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        // getpriority can return -1 as a valid value; errno tells them apart.
        *libc::__errno_location() = 0;
        let cur = libc::getpriority(libc::PRIO_PROCESS, tid);
        if cur == -1 && *libc::__errno_location() != 0 {
            return;
        }
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid, (cur + NICE_STEP).min(19));
    }
}

/// Other platforms: no per-thread nice value to change.
#[cfg(not(target_os = "linux"))]
pub fn lower_priority() {}

/// The calling thread's nice value (Linux), for tests.
#[cfg(all(test, target_os = "linux"))]
pub fn thread_nice() -> i32 {
    // SAFETY: as above.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::getpriority(libc::PRIO_PROCESS, tid)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// Why the spawning helpers are not lowered: a program started from a
    /// lowered thread runs lowered, and cannot raise itself back.
    #[test]
    fn a_child_inherits_its_thread_s_nice_value() {
        let parent = thread_nice();
        let child_nice = std::thread::spawn(|| {
            lower_priority();
            let out = std::process::Command::new("/bin/sh")
                .args(["-c", "cut -d' ' -f19 /proc/$$/stat"])
                .output()
                .unwrap();
            String::from_utf8_lossy(&out.stdout)
                .trim()
                .parse::<i32>()
                .unwrap()
        })
        .join()
        .unwrap();
        assert_eq!(child_nice, (parent + NICE_STEP).min(19));
    }

    /// Only the calling thread changes: a thread that lowers itself does not
    /// lower the thread that started it.
    #[test]
    fn only_the_calling_thread_is_lowered() {
        let parent_before = thread_nice();
        let child = std::thread::spawn(|| {
            let before = thread_nice();
            lower_priority();
            (before, thread_nice())
        })
        .join()
        .unwrap();
        assert_eq!(
            child.1,
            (child.0 + NICE_STEP).min(19),
            "the helper is lowered"
        );
        assert_eq!(thread_nice(), parent_before, "the spawning thread is not");
    }
}
