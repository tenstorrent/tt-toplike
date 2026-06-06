//! Builder for compiling Java source code into Android DEX bytecode.

use std::path::{Path, PathBuf};
use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitStatus};
use crate::env_paths::{self, PathExt};
use crate::JavaRun;

/// A builder for generating Android DEX bytecode by invoking `d8` commands.
/// 
/// Currently incremental building options are not provided here.
/// 
/// If you need to customize the `d8` command beyond what is provided here,
/// you can use the [`Dexer::command()`] method to get a [`Command`]
/// that can be further customized with additional arguments.
/// 
/// Documentation on `d8` options are based on
/// <https://developer.android.com/tools/d8/>.
/// 
/// Note: Newer JDK versions (including JDK 21 and above) may not work with
/// Android D8 from older build tools versions (below 35.0.0) if there are
/// anonymous classes in the Java code, which produce files like `Cls$1.class`.
#[derive(Clone, Debug, Default)]
pub struct Dexer {
    /// Override the default `JAVA_HOME` path.
    /// Otherwise, the default path is found using the `JAVA_HOME` env var.
    java_home: Option<PathBuf>,

    /// Override the default `d8.jar` path.
    /// Otherwise, the default path is found using [crate::android_d8_jar].
    android_d8_jar_path: Option<PathBuf>,

    /// Compile DEX bytecode without debug information. However, `d8` includes some information
    /// that's used when generating stacktraces and logging exceptions.
    release: bool,

    /// Specify the minimum Android API level you want the output DEX files to support.
    android_min_api: Option<u32>,

    /// Disable Java 8 language features. Use this flag only if you don't intend to compile
    /// Java bytecode that uses language features introduced in Java 8.
    no_desugaring: bool,

    /// Specify the path to the `android.jar` of your Android SDK.
    /// Otherwise, the default path is found using [crate::android_jar].
    android_jar_path: Option<PathBuf>,

    /// Specify classpath resources that `d8` may require to compile your project's DEX files.
    class_paths: Vec<OsString>,

    /// Specify the desired path for the DEX output. By default, `d8` outputs the DEX file(s)
    /// in the current working directory.
    out_dir: Option<OsString>,

    /// Specifies paths to compiled Java bytecodes that you want to convert into DEX bytecode.
    /// The input bytecode can be in any combination of `*.class` files or containers, such as
    /// JAR, APK, or ZIP files.
    files: Vec<OsString>,
}

impl Dexer {
    /// Creates a new `Dexer` instance with default values,
    /// which can be further customized using the builder methods.
    pub fn new() -> Self {
        Default::default()
    }

    /// Executes the `java` command based on this `Dexer` instance.
    pub fn run(&self) -> std::io::Result<ExitStatus> {
        self.command()?.status()
    }

    /// Returns a [`Command`] based on this `Dexer` instance
    /// that can be inspected or customized before being executed.
    pub fn command(&self) -> std::io::Result<Command> {
        let mut d8_run = JavaRun::new();
        
        if let Some(java_home) = &self.java_home {
            d8_run.java_home(java_home);
        }

        let d8_jar_path = self.android_d8_jar_path
            .clone()
            .and_then(PathExt::path_if_exists)
            .or_else(|| env_paths::android_d8_jar(None))
            .ok_or_else(|| std::io::Error::other(
                "d8.jar not provided, and could not be auto-discovered."
            ))?;

        d8_run.class_path(d8_jar_path)
            .main_class("com.android.tools.r8.D8");

        if self.release {
            d8_run.arg("--release");
        }

        if let Some(min_api) = self.android_min_api {
            d8_run.arg("--min-api").arg(min_api.to_string());
        }

        if self.no_desugaring {
            // `--lib` and `--classpath` options are probably redundant under this mode.
            d8_run.arg("--no-desugaring");
        }

        let android_jar_path = self.android_jar_path
            .clone()
            .and_then(PathExt::path_if_exists)
            .or_else(|| env_paths::android_jar(None))
            .ok_or_else(|| std::io::Error::other(
                "android.jar not provided, and could not be auto-discovered."
            ))?;
        d8_run.arg("--lib").arg(android_jar_path);

        for class_path in &self.class_paths {
            d8_run.arg("--classpath").arg(class_path);
        }

        if let Some(out_dir) = &self.out_dir {
            d8_run.arg("--output").arg(out_dir);
        }

        for file in &self.files {
            d8_run.arg(file);
        }

        d8_run.command()
    }

    ///////////////////////////////////////////////////////////////////////////
    //////////////////////// Builder methods below ////////////////////////////
    ///////////////////////////////////////////////////////////////////////////

    /// Override the default `JAVA_HOME` path.
    ///
    /// If not set, the default path is found using the `JAVA_HOME` env var.
    pub fn java_home<P: AsRef<OsStr>>(&mut self, java_home: P) -> &mut Self {
        self.java_home = Some(java_home.as_ref().into());
        self
    }

    /// Override the default `d8.jar` path.
    /// 
    /// Otherwise, the default path is found using [crate::android_d8_jar].
    pub fn android_d8_jar<P: AsRef<OsStr>>(&mut self, android_d8_jar_path: P) -> &mut Self {
        self.android_d8_jar_path.replace(android_d8_jar_path.as_ref().into());
        self
    }

    /// Compile DEX bytecode without debug information (including those enabled with
    /// [crate::DebugInfo] when running [crate::JavaBuild]). However, `d8` includes some
    /// information that's used when generating stacktraces and logging exceptions.
    pub fn release(&mut self, release: bool) -> &mut Self {
        self.release = release;
        self
    }

    /// Specify the minimum Android API level you want the output DEX files to support.
    /// 
    /// Set it to `20` to disable the multidex feature, so it may be loaded by `DexClassLoader`
    /// available on Android 7.1 and older versions without using the legacy multidex library.
    /// This is also useful if you want to make sure of having only one `classes.dex` output
    /// file; still, it keeps compatible with newest Android versions.
    pub fn android_min_api(&mut self, api_level: u32) -> &mut Self {
        self.android_min_api.replace(api_level);
        self
    }

    /// Disable Java 8 language features. Use this flag only if you don't intend to compile
    /// Java bytecode that uses language features introduced in Java 8.
    pub fn no_desugaring(&mut self, no_desugaring: bool) -> &mut Self {
        self.no_desugaring = no_desugaring;
        self
    }

    /// Specify the path to the `android.jar` of your Android SDK. This is required when
    /// [compiling bytecode that uses Java 8 language features](https://developer.android.google.cn/tools/d8#j8).
    ///
    /// If not set, the default path is found using [crate::android_jar].
    pub fn android_jar<P: AsRef<OsStr>>(&mut self, android_jar_path: P) -> &mut Self {
        self.android_jar_path.replace(android_jar_path.as_ref().into());
        self
    }

    /// Specify classpath resources that `d8` may require to compile your project's DEX files.
    /// 
    /// In particular, `d8` requires that you specify certain resources when [compiling bytecode
    /// that uses Java 8 language features](https://developer.android.google.cn/tools/d8#j8).
    /// This is usually the the path to all of your project's Java bytecode, even if you don't
    /// intend to compile all of the bytecode into DEX bytecode.
    pub fn class_path<S: AsRef<OsStr>>(&mut self, class_path: S) -> &mut Self {
        self.class_paths.push(class_path.as_ref().into());
        self
    }

    /// Specify the desired path for the DEX output. By default, `d8` outputs the DEX file(s)
    /// in the current working directory.
    pub fn out_dir<P: AsRef<OsStr>>(&mut self, out_dir: P) -> &mut Self {
        self.out_dir = Some(out_dir.as_ref().into());
        self
    }

    /// Adds a compiled Java bytecode file that you want to convert into DEX bytecode.
    /// The input bytecode can be in any combination of `*.class` files or containers, such as
    /// JAR, APK, or ZIP files.
    pub fn file<P: AsRef<OsStr>>(&mut self, file: P) -> &mut Self {
        self.files.push(file.as_ref().into());
        self
    }

    /// Adds multiple compiled Java bytecode files that you want to convert into DEX bytecode.
    ///
    /// This is the same as calling [`Dexer::file()`] multiple times.
    pub fn files<P>(&mut self, files: P) -> &mut Self
    where
        P: IntoIterator,
        P::Item: AsRef<OsStr>,
    {
        self.files.extend(files.into_iter().map(|f| f.as_ref().into()));
        self
    }

    /// Searches and adds `.class` files under `class_path` directory recursively.
    ///
    /// This is the same as calling [`Dexer::files()`] for these files, usually more convenient.
    pub fn collect_classes<P: AsRef<OsStr>>(&mut self, class_path: P) -> std::io::Result<&mut Self> {
        let class_path = PathBuf::from(class_path.as_ref());
        if !class_path.is_dir() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "`class_path` is not a directory"
            ));
        }
        let extension = Some(std::ffi::OsStr::new("class"));
        visit_dirs(class_path, &mut |entry| {
            if entry.path().extension() == extension {
                self.file(entry.path());
            }
        })?;
        Ok(self)
    }
}

/// Walking a directory only visiting files. Copied from `std::fs::read_dir` examples.
fn visit_dirs(
    dir: impl AsRef<Path>,
    cb: &mut impl FnMut(&std::fs::DirEntry),
) -> std::io::Result<()> {
    if dir.as_ref().is_dir() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                visit_dirs(&path, cb)?;
            } else {
                cb(&entry);
            }
        }
    }
    Ok(())
}
