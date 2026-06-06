
use std::{path::PathBuf, process::Command};

#[cfg(target_os = "macos")]
const FIND_CMD: &str = "/usr/libexec/java_home";

#[cfg(target_os = "windows")]
const FIND_CMD: &str = "where";

#[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
const FIND_CMD: &str = "which";

/// Attempts to find the JAVA_HOME directory
/// based on common installation locations on macOS, Linux, and Windows.
///
/// Code inspired by <https://github.com/astonbitecode/java-locator/>.
pub fn find_java_home() -> Option<PathBuf> {
    let mut command = Command::new(FIND_CMD);

    #[cfg(not(target_os = "macos"))] {
        command.arg("java");
    }

    let cmd_output = command.output().map_err(|error| {
        eprintln!("Command '{FIND_CMD}' not found. Error: {error}");
    }).ok()?;
    let found_java_path = String::from_utf8(cmd_output.stdout).ok().map(|output| {
        let lines = output.lines().collect::<Vec<&str>>();
        if lines.len() > 1 {
            eprintln!("Using the last of {} discovered Java locations:\n\t{}",
                lines.len(),
                lines.join("\n\t"),
            );
            lines.last().unwrap().to_string()
        } else {
            output
        }
    })?;

    if found_java_path.is_empty() {
        eprintln!("Java is not installed, or is missing from the system PATH.");
        return None;
    }

    let mut java_path = PathBuf::from(found_java_path.trim());

    while let Ok(path) = java_path.read_link() {
        java_path = if path.is_absolute() {
            path
        } else {
            java_path.pop();
            java_path.push(path);
            java_path
        };
    }

    // On macOS, `java_path` is already pointing to the Java home directory.
    // On other systems, `java_path` is pointing to "$JAVA_HOME/bin/java",
    // so we must go up 2 directories to get to java home.
    #[cfg(not(target_os = "macos"))] {
        java_path.pop();
        java_path.pop();
    }

    Some(java_path)
}
