//! Builder for customizing and invoking a `java` command.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;
use std::process::{Command, ExitStatus};
use crate::env_paths::{self, PathExt};

/// A builder for a `java` command that can be invoked.
///
/// If you need to customize the `java` command beyond what is provided here,
/// you can use the [`JavaRun::command()`] method to get a [`Command`]
/// that can be further customized with additional arguments.
///
/// Documentation on `java` options are based on
/// <https://dev.java/learn/jvm/tools/core/java/>.
#[derive(Clone, Debug, Default)]
pub struct JavaRun {
    /// Override the default `JAVA_HOME` path.
    /// Otherwise, the default path is found using the `JAVA_HOME` env var.
    java_home: Option<PathBuf>,

    /// Specify where to find user class files and annotation processors.
    /// If not provided, the current directory will be used.
    class_paths: Vec<OsString>,

    /// Specify which main class to run.
    main_class: Option<OsString>,

    /// Specify a JAR file to run instead of a main class.
    jar_file: Option<OsString>,

    /// Arguments to be passed to the main class being run by `java`.
    args: Vec<OsString>,

    /// If `true`, enable preview language features.
    enable_preview_features: bool,
}

impl JavaRun {
    /// Creates a new `JavaRun` instance with default values,
    /// which can be further customized using the builder methods.
    pub fn new() -> Self {
        Default::default()
    }

    /// Executes the `java` command based on this `JavaRun` instance.
    pub fn run(&self) -> std::io::Result<ExitStatus> {
        self.command()?.status()
    }

    /// Returns a [`Command`] based on this `JavaRun` instance
    /// that can be inspected or customized before being executed.
    pub fn command(&self) -> std::io::Result<Command> {
        let jh_clone = self.java_home.clone();
        let java_home = jh_clone
            .and_then(PathExt::path_if_exists)
            .or_else(env_paths::java_home)
            .ok_or_else(|| std::io::Error::other(
                "JAVA_HOME not provided, and could not be auto-discovered."
            ))?;

        let mut cmd = Command::new(java_home.join("bin").join("java"));

        if self.enable_preview_features {
            cmd.arg("--enable-preview");
        }
        if !self.class_paths.is_empty() {
            cmd.arg("-cp").arg(self.class_paths.join(OsStr::new(";")));
        }
        match (self.main_class.as_ref(), self.jar_file.as_ref()) {
            (Some(main_class), None) => { cmd.arg(main_class); }
            (None, Some(jar_file)) => { cmd.arg("-jar").arg(jar_file); }
            (Some(_), Some(_)) => {
                return Err(std::io::Error::other(
                    "Cannot provide both a main class AND a JAR file."
                ));
            },
            _ => { }
        }
        

        self.args.iter().for_each(|f| { cmd.arg(f); });

        Ok(cmd)
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

    /// Specify where to find user class files.
    ///
    /// If no class paths are provided, the current directory will be used.
    pub fn class_path<S: AsRef<OsStr>>(&mut self, class_path: S) -> &mut Self {
        self.class_paths.push(class_path.as_ref().into());
        self
    }

    /// Enable or disable preview language features.
    pub fn enable_preview_features(&mut self, enable_preview_features: bool) -> &mut Self {
        self.enable_preview_features = enable_preview_features;
        self
    }
    
    /// Specify the main class to launch when running the `java` command.
    ///
    /// Note that this and the `jar_file` are mutually exclusive;
    /// only one can be chosen at a time.
    pub fn main_class<S: AsRef<OsStr>>(&mut self, class: S) -> &mut Self {
        self.main_class = Some(class.as_ref().into());
        self
    }

    /// Specify the JAR file to run with the `java` command.
    ///
    /// Note that this and the `main_class` are mutually exclusive;
    /// only one can be chosen at a time.
    pub fn jar_file<P: AsRef<OsStr>>(&mut self, jar_file: P) -> &mut Self {
        self.jar_file = Some(jar_file.as_ref().into());
        self
    }

    /// Add an argument to be passed to the main class being run by `java`.
    pub fn arg<S: AsRef<OsStr>>(&mut self, arg: S) -> &mut Self {
        self.args.push(arg.as_ref().into());
        self
    }

    /// Adds multiple arguments to be passed to the main class being run by `java`.
    pub fn args<I>(&mut self, args: I) -> &mut Self
    where
        I: IntoIterator,
        I::Item: AsRef<OsStr>,
    {
        self.args.extend(args.into_iter().map(|a| a.as_ref().into()));
        self
    }
}
