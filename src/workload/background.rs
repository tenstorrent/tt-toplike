// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Lower scheduling priority for helper threads, so they yield the CPU to the
//! thread that draws the frames.
//!
//! The TUI starts several helper threads at once: the process scan, the
//! tt-smi reader, the liveness prober, the inference-server monitor, the
//! catalog refresher and the gozer poll. Each one, and each program it runs
//! (`tt-smi`, `docker`, `curl`, `ps`, `gozer`), competes with the render
//! thread for a CPU. On a box with more runnable work than cores the render
//! thread then waits for a turn and frames come late.
//!
//! [`lower_priority`] raises the calling thread's nice value by
//! [`NICE_STEP`]. On Linux the nice value belongs to the thread, and a child
//! process inherits the value of the thread that started it, so the programs
//! these threads run are lowered too. The render thread keeps its own value.
//! A helper thread therefore still gets a whole core when one is free, and
//! waits only when the render thread wants the same core.
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
pub(crate) fn thread_nice() -> i32 {
    // SAFETY: as above.
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        libc::getpriority(libc::PRIO_PROCESS, tid)
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

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
