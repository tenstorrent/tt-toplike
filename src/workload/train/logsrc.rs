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
///
/// The prefix checks below are an optimization for the common cases, not the
/// whole contract: the real gate is `target.is_file()`. That single call is
/// also what rejects two shapes with no dedicated prefix rule:
/// - A **rotated/unlinked log**: the kernel appends a literal ` (deleted)`
///   suffix to the readlink target when the process still holds an fd open
///   to a file that's been removed (e.g. by `logrotate`). No prefix matches
///   that path, so it falls through to `is_file()`, which is `false` for a
///   path that no longer exists on disk — correctly `NotRedirected` rather
///   than handing back a path nothing can open.
/// - A **FIFO or on-disk socket**: these have ordinary-looking paths (no
///   `pipe:`/`socket:` prefix — that form is only for *anonymous* fds), so
///   they're rejected purely because `is_file()` checks the file-type bits
///   and `S_IFIFO`/`S_IFSOCK` aren't `S_IFREG`.
///
/// Do not remove or short-circuit the `is_file()` call on the assumption
/// that the prefix list already covers everything — it doesn't.
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

    /// A rotated/unlinked log: the kernel appends ` (deleted)` to the
    /// readlink target when the process still holds the fd open to a file
    /// that's been removed from the directory (e.g. by logrotate). No
    /// prefix rule matches this, and the path no longer exists on disk, so
    /// it must fall through to `NotRedirected` — handing back `File(path)`
    /// here would give a caller a path that can't be opened.
    #[test]
    fn classifies_a_deleted_log_path_as_not_redirected() {
        assert_eq!(
            classify_fd_target(std::path::Path::new("/tmp/train.log (deleted)")),
            LogSource::NotRedirected
        );
    }

    /// A FIFO has an ordinary-looking path — no `pipe:`/`socket:` prefix,
    /// since that form is only for anonymous fds — so it's rejected purely
    /// by the `is_file()` file-type check. This proves that reliance is
    /// real, not incidental: if someone later "optimizes" by trusting the
    /// prefix list alone, this test goes red.
    #[test]
    #[cfg(target_os = "linux")]
    fn classifies_a_named_pipe_as_not_redirected() {
        let dir = std::env::temp_dir().join(format!("ttlog_fifo_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fifo = dir.join("train.fifo");

        let status = std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .expect("run mkfifo");
        assert!(status.success(), "mkfifo must succeed to run this test");

        assert_eq!(classify_fd_target(&fifo), LogSource::NotRedirected);

        std::fs::remove_dir_all(&dir).ok();
    }
}
