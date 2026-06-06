# android-build

[![Latest Version](https://img.shields.io/crates/v/android-build.svg)](https://crates.io/crates/android_build)
[![Docs](https://docs.rs/android-build/badge.svg)](https://docs.rs/android-build/latest/android_build/)
[![Project Robius Matrix Chat](https://img.shields.io/matrix/robius-general%3Amatrix.org?server_fqdn=matrix.org&style=flat&logo=matrix&label=Project%20Robius%20Matrix%20Chat&color=B7410E)](https://matrix.to/#/#robius:matrix.org)

Use this crate from your Cargo `build.rs` build script to compile Java source files and to run Java/Android commands as part of a Rust build,
specifically designed for Android-targeted builds and Android tools.

This crate aims to behave similarly to [`cc-rs`](https://github.com/rust-lang/cc-rs/tree/main), but for Java (primarily on Android) instead of C/C++.

This crate is part of [Project Robius](https://github.com/project-robius) and is primarily used by those crates.

## Usage
Add this crate as a build dependency to your `Cargo.toml`:
```toml
[build-dependencies]
android-build = "0.1.0"
```

Then add this to your `build.rs` build script:
```rust
fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    if target_os == "android" {
        let output_dir = std::env::var("OUT_DIR").unwrap();
        let android_jar_path = android_build::android_jar(None)
            .expect("Failed to find android.jar");

        android_build::JavaBuild::new()
            .class_path(android_jar_path)
            .classes_out_dir(std::path::PathBuf::from(output_dir))
            .file("YourJavaFile.java")
            .compile()
            .expect("java build failed!");

        // `YourJavaFile.class` will be in the Cargo-specified OUT_DIR.
    }
}
```

The above code will automatically run when you build your crate using `cargo build`.

## Configuration via environment variables
The [crate-level documentation](https://docs.rs/android-build/latest/android_build/) provides a detailed list of environment variables that can be set
to configure this crate.

## Examples
Check out the [`robius-authentication` build script](https://github.com/project-robius/robius-authentication/blob/main/build.rs) to see how we use this crate for more complicated build procedures:
* Discovering specific Android jarfiles and SDK directories (platforms, build tools, etc).
* Compiling Java classes against the main `android.jar` jarfile.
* Invoking Android's `d8` DEXer tool.
