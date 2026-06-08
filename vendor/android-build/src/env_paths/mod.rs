use std::{env, path::{Path, PathBuf}};
use self::find_java::find_java_home;

mod find_android_sdk;
mod find_java;


pub const ANDROID_HOME:                 &str = "ANDROID_HOME";
pub const ANDROID_SDK_ROOT:             &str = "ANDROID_SDK_ROOT";
pub const ANDROID_BUILD_TOOLS_VERSION:  &str = "ANDROID_BUILD_TOOLS_VERSION";
pub const ANDROID_PLATFORM:             &str = "ANDROID_PLATFORM";
pub const ANDROID_SDK_VERSION:          &str = "ANDROID_SDK_VERSION";
pub const ANDROID_API_LEVEL:            &str = "ANDROID_API_LEVEL";
pub const ANDROID_SDK_EXTENSION:        &str = "ANDROID_SDK_EXTENSION";
pub const ANDROID_D8_JAR:               &str = "ANDROID_D8_JAR";
pub const ANDROID_JAR:                  &str = "ANDROID_JAR";
pub const JAVA_HOME:                    &str = "JAVA_HOME";
pub const JAVA_SOURCE_VERSION:          &str = "JAVA_SOURCE_VERSION";
pub const JAVA_TARGET_VERSION:          &str = "JAVA_TARGET_VERSION";

/// An extension trait for checking if a path exists.
pub trait PathExt {
    fn path_if_exists(self) -> Option<Self> where Self: Sized;
}
impl<P: AsRef<Path>> PathExt for P {
    fn path_if_exists(self) -> Option<P> {
        if self.as_ref().as_os_str().is_empty() {
            return None;
        }
        match self.as_ref().try_exists() {
            Ok(true) => Some(self),
            _ => None,
        }
    }
}


/// Returns the path to the Android SDK directory.
///
/// The path is determined by an ordered set of attempts:
/// * The `ANDROID_HOME` environment variable, if it is set and if the directory exists.
/// * The `ANDROID_SDK_HOME` environment variable, if it is set and if the directory exists.
/// * The default installation location for the Android SDK, if it exists.
///   * On Windows, this is `%LOCALAPPDATA%\Android\Sdk`.
///   * On macOS, this is `~/Library/Android/sdk`.
///   * On Linux, this is `~/Android/Sdk`.
#[doc(alias("ANDROID_HOME", "ANDROID_SDK_ROOT", "home", "sdk", "root"))]
pub fn android_sdk() -> Option<PathBuf> {
    env_var(ANDROID_HOME).ok()
        .and_then(PathExt::path_if_exists)
        .or_else(|| env_var(ANDROID_SDK_ROOT).ok()
            .and_then(PathExt::path_if_exists)
        )
        .map(PathBuf::from)
        .or_else(|| find_android_sdk::find_android_sdk().and_then(PathExt::path_if_exists))
}

/// Returns the path to the `android.jar` file for the given API level.
///
/// The path is determined by an ordered set of attempts:
/// * The `ANDROID_JAR` environment variable, if it is set and points to a file that exists.
/// * The argument `platform_string` is used if it is `Some`, to find the subdirectory for
///   the specific platform version under the Android SDK `platforms` directory, in which
///   the `android.jar` file should exist.
/// * The value of the following environment variables are used to calculate the platform string:
///   * `ANDROID_PLATFORM`, `ANDROID_API_LEVEL` or `ANDROID_SDK_VERSION`
///   * `ANDROID_SDK_EXTENSION` (optional)
/// * The highest Android platform version found in the SDK `platforms` directory is used if
///   the platform version is not set by environment variables.
pub fn android_jar(platform_string: Option<&str>) -> Option<PathBuf> {
    env_var(ANDROID_JAR).ok()
        .and_then(PathExt::path_if_exists)
        .map(PathBuf::from)
        .or_else(|| android_sdk()
            .and_then(|sdk| {
                let platforms = sdk.join("platforms");
                platform_string.map(ToString::to_string)
                    .or_else(env_android_platform_api_level)
                    .or_else(|| {
                        let latest = find_latest_version(&platforms, "android.jar");
                        #[cfg(feature = "cargo")]
                        if let Some(ver) = latest.as_ref() {
                            println!("cargo::warning=ANDROID_PLATFORM environment variable \
                                is not set, using '{ver}'.");
                        }
                        latest
                    })
                    .map(|version| platforms.join(version))
            })
            .and_then(|path| path.join("android.jar").path_if_exists())
        )
}

/// Returns the path to the `d8.jar` file for the given build tools version.
///
/// The path is determined by an ordered set of attempts:
/// * The `ANDROID_D8_JAR` environment variable, if it is set and points to a file that exists.
/// * The argument `build_tools_version` is used if it is `Some`, to find the subdirectory for
///   the specific build tools version under the Android SDK `build-tools` directory.
/// * The `ANDROID_BUILD_TOOLS_VERSION` environment variable is used to find the subdirectory for
///   the build tools version under the Android SDK `build-tools` directory.
/// * The highest Android build tools version found in the SDK `build-tools` directory is used if
///   the build tools version is not set by the environment variable.
pub fn android_d8_jar(build_tools_version: Option<&str>) -> Option<PathBuf> {
    env_var(ANDROID_D8_JAR).ok()
        .and_then(PathExt::path_if_exists)
        .map(PathBuf::from)
        .or_else(|| android_sdk()
            .and_then(|sdk| {
                let build_tools = sdk.join("build-tools");
                build_tools_version.map(ToString::to_string)
                    .or_else(|| env_var(ANDROID_BUILD_TOOLS_VERSION).ok())
                    .or_else(|| {
                        let latest = find_latest_version(&build_tools, Path::new("lib").join("d8.jar"));
                        #[cfg(feature = "cargo")]
                        if let Some(ver) = latest.as_ref() {
                            println!("cargo::warning=ANDROID_BUILD_TOOLS_VERSION environment variable \
                                is not set, using '{ver}'.");
                        }
                        latest
                    })
                    .map(|version| build_tools.join(version))
            })
            .and_then(|path| path.join("lib").join("d8.jar").path_if_exists())
        )
}

/// Returns the platform version string (aka API level, SDK version) being targeted for compilation.
/// This deals with environment variables `ANDROID_PLATFORM`, `ANDROID_API_LEVEL`, and `ANDROID_SDK_VERSION`,
/// as well as the optional `ANDROID_SDK_EXTENSION`.
fn env_android_platform_api_level() -> Option<String> {
    let mut base = env_var(ANDROID_PLATFORM).ok()
        .or_else(|| env_var(ANDROID_API_LEVEL).ok())
        .or_else(|| env_var(ANDROID_SDK_VERSION).ok())?;
    
    if base.is_empty() {
        return None;
    }

    if !base.starts_with("android-") {
        base = format!("android-{}", base);
    }

    if base.contains("-ext") {
        return Some(base);
    }

    if let Ok(raw_ext) = env_var(ANDROID_SDK_EXTENSION) {
        let ext_num = raw_ext
            .trim_start_matches("-")
            .trim_start_matches("ext");
        if !ext_num.is_empty() {
            base = format!("{}-ext{}", base, ext_num);
        }
    }
    
    Some(base)
}

/// Finds subdirectories in which the subpath `arg` exists, and returns the maximum
/// item name in lexicographical order based on `Ord` impl of `std::path::Path`.
/// NOTE: the behavior can be changed in the future.
/// 
/// Code inspired by <https://docs.rs/crate/i-slint-backend-android-activity/1.9.1/source/build.rs>.
fn find_latest_version(base: impl AsRef<Path>, arg: impl AsRef<Path>) -> Option<String> {
    std::fs::read_dir(base)
        .ok()?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().join(arg.as_ref()).exists())
        .map(|entry| entry.file_name())
        .max()
        .and_then(|name| name.to_os_string().into_string().ok())
}

/// Returns the path to the `java` executable by looking for `$JAVA_HOME/bin/java`.
pub fn java() -> Option<PathBuf> {
    java_home().and_then(|jh| jh
        .join("bin")
        .join("java")
        .path_if_exists()
    )
}

/// Returns the path to the `javac` compiler by looking for `$JAVA_HOME/bin/javac`.
pub fn javac() -> Option<PathBuf> {
    java_home().and_then(|jh| jh
        .join("bin")
        .join("javac")
        .path_if_exists()
    )
}

/// Returns the `JAVA_HOME` path by attempting to discover it.
/// 
/// First, if the `$JAVA_HOME` environment variable is set and points to a directory that exists,
/// that path is returned.
/// Otherwise, a series of common installation locations is used,
/// based on the current platform (macOS, Linux, Windows).
pub fn java_home() -> Option<PathBuf> {
    env_var(JAVA_HOME).ok()
        .and_then(PathExt::path_if_exists)
        .map(PathBuf::from)
        .or_else(find_java_home)
}

/// Returns the source version for compilation from environment variable `JAVA_SOURCE_VERSION`.
pub fn java_source_version() -> Option<u32> {
    env_var(JAVA_SOURCE_VERSION).ok()?.parse().ok()
}

/// Returns the target version for compilation from environment variable `JAVA_TARGET_VERSION`.
pub fn java_target_version() -> Option<u32> {
    env_var(JAVA_TARGET_VERSION).ok()?.parse().ok()
}

/// Returns the major version number of the `javac` compiler.
pub fn check_javac_version(java_home: impl AsRef<Path>) -> std::io::Result<u32> {
    let javac = java_home.as_ref().join("bin").join("javac");
    let output = std::process::Command::new(&javac)
        .arg("-version")
        .output()
        .map_err(|e| std::io::Error::other(
            format!("Failed to execute javac -version: {:?}", e)
        ))?;
    if !output.status.success() {
        return Err(std::io::Error::other(format!(
            "Failed to get javac version: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    let mut version_output = String::from_utf8_lossy(&output.stdout);
    if version_output.is_empty() {
        // old versions of java use stderr
        version_output = String::from_utf8_lossy(&output.stderr);
    }
    let version = parse_javac_version_output(&version_output);
    if version > 0 {
        Ok(version as u32)
    } else {
        Err(std::io::Error::other(
            format!("Failed to parse javac version: '{version_output}'")
        ))
    }
}

/// Migrated from <https://docs.rs/crate/i-slint-backend-android-activity/1.9.1/source/build.rs>.
fn parse_javac_version_output(version_output: &str) -> i32 {
    let version = version_output
        .lines()
        .last()
        .unwrap_or_default()
        .split_whitespace()
        .nth(1)
        .and_then(|v| v.split('-').next())
        .unwrap_or_default();
    let mut java_ver: i32 = version.split('.').next().unwrap_or("0").parse().unwrap_or(0);
    if java_ver == 1 {
        // Before java 9, the version was something like javac 1.8
        java_ver = version.split('.').nth(1).unwrap_or("0").parse().unwrap_or(0);
    }
    java_ver
}

#[test]
fn test_parse_javac_version() {
    assert_eq!(parse_javac_version_output("javac 1.8.0_292"), 8);
    assert_eq!(parse_javac_version_output("javac 17.0.13"), 17);
    assert_eq!(parse_javac_version_output("javac 21.0.5"), 21);
    assert_eq!(parse_javac_version_output("javac 24-ea"), 24);
    assert_eq!(parse_javac_version_output("error"), 0);
    assert_eq!(parse_javac_version_output("javac error"), 0);
}

/// Rerun the build script if the variable is changed. Do not use it for variables set by Cargo.
fn env_var(var: &str) -> Result<String, env::VarError> {
    #[cfg(feature = "cargo")]
    println!("cargo:rerun-if-env-changed={}", var);
    env::var(var)
}
