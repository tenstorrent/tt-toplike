//! Finds the Android SDK installation at the default location
//! on multiple platforms: macOS, Windows, and Linux.

use std::path::PathBuf;

#[cfg(target_os = "macos")]
pub fn find_android_sdk() -> Option<PathBuf> {
    PathBuf::from(std::env::var("HOME").ok()?)
        .join("Library")
        .join("Android")
        .join("sdk")
        .into()
}

#[cfg(target_os = "linux")]
pub fn find_android_sdk() -> Option<PathBuf> {
    PathBuf::from(std::env::var("HOME").ok()?)
        .join("Android")
        .join("Sdk")
        .into()
}

#[cfg(target_os = "windows")]
pub fn find_android_sdk() -> Option<PathBuf> {
    windows_home_dir()?
        .join("AppData")
        .join("Local")
        .join("Android")
        .join("Sdk")
        .into()
}

// When building on an Android host, there is no need to locate
// SDK files; we can just use env vars to point to local native files.
#[cfg(target_os = "android")]
pub fn find_android_sdk() -> Option<PathBuf> {
    None
}

#[cfg(target_os = "windows")]
/// Returns the path to the current user's home directory on Windows.
///
/// Code inspired by the `dirs-sys` crate:
/// <https://github.com/dirs-dev/dirs-sys-rs/blob/c0fd66cb08f1f97ebf670914253a34bd42d284fb/src/lib.rs#L151>
pub fn windows_home_dir() -> Option<PathBuf> {
    use std::ffi::{OsString, c_void};
    use std::os::windows::ffi::OsStringExt;
    use windows_sys::{
        core::PWSTR,
        Win32::{UI::Shell, Foundation, Globalization, System::Com},
    };

    unsafe {
        let mut dir_out: PWSTR = std::ptr::null_mut();
        let res = Shell::SHGetKnownFolderPath(
            &Shell::FOLDERID_Profile,
            0,
            Foundation::HANDLE::default(),
            &mut dir_out,
        );
        let pathbuf = if res == 0 {
            let ostr: OsString = OsStringExt::from_wide(
                std::slice::from_raw_parts(
                    dir_out,
                    Globalization::lstrlenW(dir_out) as usize,
                )
            );
            Some(PathBuf::from(ostr))
        } else {
            None
        };
        Com::CoTaskMemFree(dir_out as *const c_void);
        pathbuf
    }
    
}
