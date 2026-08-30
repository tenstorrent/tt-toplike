// SPDX-License-Identifier: Apache-2.0
// SPDX-FileCopyrightText: 2026 Tenstorrent USA, Inc.

//! Find a running tt-train process's log without being told where it is.
//!
//! `/proc/<pid>/fd/1` is a symlink to whatever stdout actually is. For the
//! common long-run launch shape (`./nano_gpt … > train.log &`) it resolves to
//! the real file, which we can then tail from outside knowing only the pid —
//! this is what lets the Training view attach with no command from the user.
//!
//! When stdout is a pipe or a terminal the link reads `pipe:[…]` /
//! `/dev/pts/N` instead. There is no way to retroactively read a process's
//! un-redirected stdout — that's an OS property, not a gap here — so we report
//! `NotRedirected` and the view explains the situation rather than inventing
//! data.

use std::path::{Path, PathBuf};

/// Where (if anywhere) a process's stdout can be read from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LogSource {
    /// stdout is redirected to this regular file — tailable.
    File(PathBuf),
    /// stdout is a pipe/tty/socket — per-step metrics are unavailable.
    NotRedirected,
}

/// Classify a resolved `/proc/<pid>/fd/1` target. Pure, so the pipe/tty
/// shapes are testable without spawning anything.
pub fn classify_fd_target(target: &Path) -> LogSource {
    let s = target.to_string_lossy();
    // Anonymous fds render as `pipe:[N]` / `socket:[N]`; a tty is under /dev.
    if s.starts_with("pipe:") || s.starts_with("socket:") || s.starts_with("anon_inode:") {
        return LogSource::NotRedirected;
    }
    if s.starts_with("/dev/pts/") || s == "/dev/null" || s == "/dev/tty" {
        return LogSource::NotRedirected;
    }
    if target.is_file() {
        return LogSource::File(target.to_path_buf());
    }
    LogSource::NotRedirected
}

/// Resolve a pid's stdout. Never errors: an exited process, a permission
/// denial, or a non-Linux target all read as `NotRedirected`.
pub fn discover_log(pid: i32) -> LogSource {
    match std::fs::read_link(format!("/proc/{pid}/fd/1")) {
        Ok(target) => classify_fd_target(&target),
        Err(_) => LogSource::NotRedirected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_a_pipe_or_tty_as_not_redirected() {
        // What readlink returns when stdout is a pipe or a terminal.
        assert_eq!(
            classify_fd_target(std::path::Path::new("pipe:[123456]")),
            LogSource::NotRedirected
        );
        assert_eq!(
            classify_fd_target(std::path::Path::new("/dev/pts/3")),
            LogSource::NotRedirected
        );
        assert_eq!(
            classify_fd_target(std::path::Path::new("socket:[99]")),
            LogSource::NotRedirected
        );
    }

    /// The real thing: spawn a process with stdout redirected to a file and
    /// confirm we can recover that path from outside knowing only the pid.
    /// A mock cannot prove this works, and the whole auto-attach UX rests on
    /// it, so this test drives a genuine process.
    #[test]
    #[cfg(target_os = "linux")]
    fn discovers_the_log_path_of_a_real_redirected_process() {
        use std::process::{Command, Stdio};

        let dir = std::env::temp_dir().join(format!("ttlog_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("train.log");
        let f = std::fs::File::create(&log).unwrap();

        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .stdout(Stdio::from(f))
            .spawn()
            .expect("spawn test child");

        let found = discover_log(child.id() as i32);

        child.kill().ok();
        child.wait().ok();

        match found {
            LogSource::File(p) => assert_eq!(
                p.canonicalize().unwrap(),
                log.canonicalize().unwrap(),
                "must recover the real redirected log path"
            ),
            LogSource::NotRedirected => panic!("a file-redirected stdout must be discoverable"),
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    #[cfg(target_os = "linux")]
    fn reports_not_redirected_when_stdout_is_a_pipe() {
        use std::process::{Command, Stdio};
        let mut child = Command::new("/bin/sleep")
            .arg("5")
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn test child");
        let found = discover_log(child.id() as i32);
        child.kill().ok();
        child.wait().ok();
        assert_eq!(found, LogSource::NotRedirected);
    }

    #[test]
    fn a_dead_pid_is_not_redirected_rather_than_an_error() {
        assert_eq!(discover_log(999_999_998), LogSource::NotRedirected);
    }
}
