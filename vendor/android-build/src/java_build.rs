//! Builder for customizing and invoking a `javac` command.

use std::path::PathBuf;
use std::ffi::{OsStr, OsString};
use std::process::{Command, ExitStatus};
use crate::env_paths::{self, PathExt};

/// A builder for a `javac` command that can be invoked.
///
/// If you need to customize the `javac` command beyond what is provided here,
/// you can use the [`JavaBuild::command()`] method to get a [`Command`]
/// that can be further customized with additional arguments.
///
/// Documentation on `javac` options are based on
/// <https://dev.java/learn/jvm/tools/core/javac/>.
#[derive(Clone, Debug, Default)]
pub struct JavaBuild {
    /// Override the default `JAVA_HOME` path.
    /// Otherwise, the default path is found using the `JAVA_HOME` env var.
    java_home: Option<PathBuf>,
    /// Debug info to include in the output ("-g" flag).
    debug_info: Option<DebugInfo>,
    /// If `true`, all warnings are disabled.
    nowarn: bool,
    /// Enable verbose output.
    verbose: bool,
    /// If `true`, warnings are treated as compilation errors.
    warnings_as_errors: bool,
    /// If `true`, show full descriptions of all places where
    /// deprecated members/classes are used or overridden.
    /// If `false`, is to show only a summary on a per-source file basis).
    deprecation: bool,
    /// If `true`, enable preview language features.
    enable_preview_features: bool,
    /// Specify where to find user class files and annotation processors.
    /// If not provided, the current directory will be used.
    class_paths: Vec<OsString>,
    /// Specify where to find input source files.
    /// If not specified, `class_paths` will be searched for source files.
    source_paths: Vec<OsString>,
    /// Override the location of bootstrap class files.
    boot_class_paths: Vec<OsString>,
    /// Override the location of installed extensions.
    extension_dirs: Vec<OsString>,
    /// Override the location of endorsed standards path.
    endorsed_dirs: Vec<OsString>,
    /// Specify names of the annotation processors to run.
    /// Setting this will bypass the default discovery process.
    annotation_processors: Vec<OsString>,
    /// Specify where to find annotation processors.
    /// If not provided, the `class_paths` will be searched.
    annotation_processor_paths: Vec<OsString>,
    /// Enable generation of metadata on method parameters
    /// such that the reflection API can be used to retrieve parameter info.
    method_paramater_metadata: bool,
    /// Specify where to place generated class files.
    /// If not provided, class files will be placed
    /// in the same directory as the source files.
    #[doc(alias = "-d")]
    classes_out_dir: Option<OsString>,
    /// Specify where to place generated source files.
    #[doc(alias = "-s")]
    sources_out_dir: Option<OsString>,
    /// Specify where to place generated native header files.
    #[doc(alias = "-h")]
    headers_out_dir: Option<OsString>,
    /// Pass an option to an annotation processor.
    #[doc(alias = "-A")]
    annotation_parameters: Vec<(String, String)>,
    /// The Java version for source compatibility.
    java_source_version: Option<u32>,
    /// The Java version for target compatibility.
    java_target_version: Option<u32>,
    /// Paths to the java source files to be compiled.
    files: Vec<OsString>,
}

/// Debug information to include in the output of a `javac` build.
///
/// The default value for this struct is for everything to be `true`,
/// meaning all debug information is included.
/// This is only relevant *if* you set the `debug_info` field in [`JavaBuild`].
#[derive(Clone, Debug)]
pub struct DebugInfo {
    pub line_numbers: bool,
    pub variables: bool,
    pub source_files: bool,
}
impl Default for DebugInfo {
    fn default() -> Self {
        Self {
            line_numbers: true,
            variables: true,
            source_files: true,
        }
    }
}
impl DebugInfo {
    fn add_as_args_to<'c>(&self, cmd: &'c mut Command) -> &'c mut Command {
        if self.line_numbers {
            cmd.arg("-g:lines");
        }
        if self.variables {
            cmd.arg("-g:vars");
        }
        if self.source_files {
            cmd.arg("-g:source");
        }
        if !self.line_numbers && !self.variables && !self.source_files {
            cmd.arg("-g:none");
        }
        cmd
    }
}

impl JavaBuild {
    /// Creates a new `JavaBuild` instance with default values,
    /// which can be further customized using the builder methods.
    pub fn new() -> Self {
        Default::default()
    }

    /// Executes the `javac` command based on this `JavaBuild` instance.
    pub fn compile(&self) -> std::io::Result<ExitStatus> {
        self.command()?.status()
    }

    /// Returns a [`Command`] based on this `JavaBuild` instance
    /// that can be inspected or customized before being executed.
    pub fn command(&self) -> std::io::Result<Command> {
        let jh_clone = self.java_home.clone();
        let java_home = jh_clone
            .and_then(PathExt::path_if_exists)
            .or_else(env_paths::java_home)
            .ok_or_else(|| std::io::Error::other(
                "JAVA_HOME not provided, and could not be auto-discovered."
            ))?;
        
        let mut cmd = Command::new(java_home.join("bin").join("javac"));
        if let Some(d) = self.debug_info.as_ref() {
            d.add_as_args_to(&mut cmd);
        }

        let java_ver = crate::check_javac_version(&java_home)?;
        if java_ver < 8 {
            return Err(std::io::Error::other(
                format!("The minimum required Java version is Java 8. Your Java version: {java_ver}")
            ));
        }
        
        if let Some(source) = self.java_source_version.or(env_paths::java_source_version()) {
            if java_ver < source {
                return Err(std::io::Error::other(
                    format!("'-source {source}' is higher than your Java version: {java_ver}")
                ));
            }
            cmd.arg("-source").arg(source.to_string());
        }

        if let Some(target) = self.java_target_version.or(env_paths::java_target_version()) {
            if java_ver < target {
                return Err(std::io::Error::other(
                    format!("'-target {target}' is higher than your Java version: {java_ver}")
                ));
            }
            cmd.arg("-target").arg(target.to_string());
        }

        let add_path_arg = |cmd: &mut Command, arg_name, paths: &[OsString]| {
            if paths.is_empty() { return; }
            let seperator = if std::path::MAIN_SEPARATOR == '\\' { ";" } else { ":" };
            cmd.arg(arg_name).arg(paths.join(OsStr::new(seperator)));
        };
        add_path_arg(&mut cmd, "-cp",            &self.class_paths);
        add_path_arg(&mut cmd, "-sourcepath",    &self.source_paths);
        add_path_arg(&mut cmd, "-bootclasspath", &self.boot_class_paths);
        add_path_arg(&mut cmd, "-extdirs",       &self.extension_dirs);

        let processors = self.annotation_processors.join(OsStr::new(","));
        if processors.len() != 0 {
            cmd.arg("-processor").arg(processors); 
        }

        self.annotation_processor_paths.iter()
            .for_each(|p| { cmd.arg("-processorpath").arg(p); });

        for (flag, dir) in [
            ("-d", self.classes_out_dir.as_ref()),
            ("-s", self.sources_out_dir.as_ref()),
            ("-h", self.headers_out_dir.as_ref()),
        ].iter() {
            if let Some(dir) = dir {
                cmd.arg(flag).arg(dir);
            }
        }

        for (flag, cond) in [
            ("-nowarn",          self.nowarn),
            ("-verbose",         self.verbose),
            ("-deprecation",     self.deprecation),
            ("-parameters",      self.method_paramater_metadata),
            ("-Werror",          self.warnings_as_errors),
            ("--enable-preview", self.enable_preview_features)
        ].into_iter() {
            if cond { cmd.arg(flag); }
        }

        self.annotation_parameters.iter()
            .for_each(|(k,v)| { cmd.arg(format!("-A{}={}", k, v)); });
        self.files.iter().for_each(|f| { cmd.arg(f); });

        Ok(cmd)
    }

    ///////////////////////////////////////////////////////////////////////////
    //////////////////////// Builder methods below ////////////////////////////
    ///////////////////////////////////////////////////////////////////////////

    /// Override the default `JAVA_HOME` path.
    ///
    /// If not set, the default path is found using the `JAVA_HOME` env var.
    pub fn java_home<P: Into<PathBuf>>(&mut self, java_home: P) -> &mut Self {
        self.java_home = Some(java_home.into());
        self
    }

    /// Set which debug info should be included in the generated class files
    #[doc(alias("-g"))]
    pub fn debug_info(&mut self, debug_info: DebugInfo) -> &mut Self {
        self.debug_info = Some(debug_info);
        self
    }

    /// If set to `true`, all warnings are disabled.
    pub fn nowarn(&mut self, nowarn: bool) -> &mut Self {
        self.nowarn = nowarn;
        self
    }

    /// Enable verbose output.
    pub fn verbose(&mut self, verbose: bool) -> &mut Self {
        self.verbose = verbose;
        self
    }

    /// Configure the output about `deprecation` usage.
    ///
    /// * If `true`, javac will output full descriptions of all places
    ///   where deprecated members/classes are used or overridden.
    /// * If `false`, javac will output only a summary on a per-source file basis.
    pub fn deprecation(&mut self, deprecation: bool) -> &mut Self {
        self.deprecation = deprecation;
        self
    }

    /// Enable or disable preview language features.
    pub fn enable_preview_features(&mut self, enable_preview_features: bool) -> &mut Self {
        self.enable_preview_features = enable_preview_features;
        self
    }

    /// Specify where to find user class files and annotation processors.
    ///
    /// If no class paths are provided, the current directory will be used.
    pub fn class_path<P: AsRef<OsStr>>(&mut self, class_path: P) -> &mut Self {
        self.class_paths.push(class_path.as_ref().into());
        self
    }

    /// Specify where to find input source files.
    ///
    /// If not specified, `class_paths` will be searched for source files.
    pub fn source_path<P: AsRef<OsStr>>(&mut self, source_path: P) -> &mut Self {
        self.source_paths.push(source_path.as_ref().into());
        self
    }

    /// Specify where to find bootstrap class files.
    ///
    /// If set, this will override the default search locations.
    pub fn boot_class_path<P: AsRef<OsStr>>(&mut self, boot_class_path: P) -> &mut Self {
        self.boot_class_paths.push(boot_class_path.as_ref().into());
        self
    }

    /// Specify where to find installed extensions.
    ///
    /// If set, this will override the default search locations.
    pub fn extension_dir<P: AsRef<OsStr>>(&mut self, extension_dir: P) -> &mut Self {
        self.extension_dirs.push(extension_dir.as_ref().into());
        self
    }

    /// Specify where to find endorsed standards.
    ///
    /// If set, this will override the default endorsed standards path.
    pub fn endorsed_dir<P: AsRef<OsStr>>(&mut self, endorsed_dir: P) -> &mut Self {
        self.endorsed_dirs.push(endorsed_dir.as_ref().into());
        self
    }

    /// Add an annotation processor to be run during compilation.
    ///
    /// Setting this will bypass the default discovery process.
    pub fn annotation_processor<S: AsRef<OsStr>>(&mut self, annotation_processor: S) -> &mut Self {
        self.annotation_processors.push(annotation_processor.as_ref().into());
        self
    }

    /// Add a path to search for annotation processors.
    ///
    /// If not provided, the class paths will be searched by default.
    pub fn annotation_processor_path<P: AsRef<OsStr>>(&mut self, annotation_processor_path: P) -> &mut Self {
        self.annotation_processor_paths.push(annotation_processor_path.as_ref().into());
        self
    }

    /// Enable generation of metadata on method parameters
    /// such that the reflection API can be used to retrieve parameter info.
    pub fn method_paramater_metadata(&mut self, method_paramater_metadata: bool) -> &mut Self {
        self.method_paramater_metadata = method_paramater_metadata;
        self
    }

    /// Specify where to place generated class files.
    ///
    /// If not provided, class files will be placed
    /// in the same directory as the source files.
    #[doc(alias("-d"))]
    pub fn classes_out_dir<P: AsRef<OsStr>>(&mut self, classes_out_dir: P) -> &mut Self {
        self.classes_out_dir = Some(classes_out_dir.as_ref().into());
        self
    }

    /// Specify where to place generated source files.
    #[doc(alias("-s"))]
    pub fn sources_out_dir<P: AsRef<OsStr>>(&mut self, sources_out_dir: P) -> &mut Self {
        self.sources_out_dir = Some(sources_out_dir.as_ref().into());
        self
    }

    /// Specify where to place generated native header files.
    #[doc(alias("-h"))]
    pub fn headers_out_dir<P: AsRef<OsStr>>(&mut self, headers_out_dir: P) -> &mut Self {
        self.headers_out_dir = Some(headers_out_dir.as_ref().into());
        self
    }

    /// Add a key-value pair to be passed as an option to an annotation processor.
    #[doc(alias("-A"))]
    pub fn annotation_parameter<K, V>(&mut self, key: K, value: V) -> &mut Self 
    where
        K: Into<String>,
        V: Into<String>,
    {
        self.annotation_parameters.push((key.into(), value.into()));
        self
    }

    /// Specify the Java version for source compatibility.
    pub fn java_source_version(&mut self, version: u32) -> &mut Self {
        self.java_source_version.replace(version);
        self
    }

    /// Specify the Java version for target compatibility.
    pub fn java_target_version(&mut self, version: u32) -> &mut Self {
        self.java_target_version.replace(version);
        self
    }

    /// If set to `true`, warnings are treated as compilation errors.
    pub fn warnings_as_errors(&mut self, warnings_as_errors: bool) -> &mut Self {
        self.warnings_as_errors = warnings_as_errors;
        self
    }

    /// Adds a Java source file to be compiled by javac.
    #[doc(alias("source file"))]
    pub fn file<P: AsRef<OsStr>>(&mut self, file: P) -> &mut Self {
        self.files.push(file.as_ref().into());
        self
    }

    /// Adds multiple Java source files to be compiled by javac.
    ///
    /// This is the same as calling [`JavaBuild::file()`] multiple times.
    #[doc(alias("source files"))]
    pub fn files<P>(&mut self, files: P) -> &mut Self
    where
        P: IntoIterator,
        P::Item: AsRef<OsStr>,
    {
        self.files.extend(files.into_iter().map(|f| f.as_ref().into()));
        self
    }
}
