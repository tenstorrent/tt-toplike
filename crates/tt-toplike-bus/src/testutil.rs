//! Test helpers shared by the module tests. Compiled only for tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

/// A unique path under the system temp directory, removed on drop.
///
/// The directory itself is not created, so a test can check that the bus
/// creates it. The name holds the process id and a counter, so parallel
/// tests and parallel test runs never share one.
pub(crate) struct TempDir(PathBuf);

impl TempDir {
    pub(crate) fn new(tag: &str) -> TempDir {
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::SeqCst);
        let p = std::env::temp_dir().join(format!(
            "tt-toplike-bus-test-{tag}-{}-{n}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&p);
        TempDir(p)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // A test may have made the directory read-only; undo that first.
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
        let _ = fs::remove_dir_all(&self.0);
    }
}
