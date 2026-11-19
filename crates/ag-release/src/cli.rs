//! Command-line interface and command dispatch.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

use crate::file_system::FileSystem;
use crate::plan::ReleasePlan;
use crate::process::ProcessRunner;
use crate::tag::ReleaseTag;
use crate::workspace::{App, Workspace};
use crate::{assemble, build, release_note};

/// Plans and packages GitHub releases of Cargo workspace binaries.
#[derive(Debug, Parser)]
#[command(name = "ag-release", version)]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: Command,
}

/// Supported release commands.
#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Validates the release configuration (and the tag when given) and
    /// prints the release plan as JSON.
    Plan(PlanOptions),
    /// Builds every app for one target and writes its archives and checksum
    /// files.
    Build(BuildOptions),
    /// Verifies every target's archives and writes the release-wide
    /// artifacts next to them.
    Assemble(AssembleOptions),
    /// Prints the GitHub release body, or only its title, built from the
    /// `CHANGELOG.md` entry of the tag.
    Notes(NotesOptions),
}

/// Options for `plan`.
#[derive(Debug, Args)]
pub(crate) struct PlanOptions {
    /// Release tag, `v<semver>` or `<semver>`, that every app's version must
    /// match.
    #[arg(long)]
    pub(crate) tag: Option<String>,
    /// Configured target triple to plan for, instead of every configured
    /// target; repeat for several.
    #[arg(long = "target", value_name = "TRIPLE")]
    pub(crate) targets: Vec<String>,
}

/// Options for `build`.
#[derive(Debug, Args)]
pub(crate) struct BuildOptions {
    /// Directory for the archives and checksum files; defaults to
    /// `ag-release.out` in Cargo's target directory.
    #[arg(long)]
    pub(crate) out_dir: Option<PathBuf>,
    /// Cargo profile to build with instead of the configured one.
    #[arg(long)]
    pub(crate) profile: Option<String>,
    /// Configured target triple to build. Its Rust target must be installed.
    #[arg(long)]
    pub(crate) target: String,
}

/// Options for `assemble`.
#[derive(Debug, Args)]
pub(crate) struct AssembleOptions {
    /// Directory holding every target's archives and checksum files; the
    /// release-wide artifacts are written into it.
    #[arg(long)]
    pub(crate) artifacts_dir: PathBuf,
    /// Release tag, `v<semver>` or `<semver>`, naming the apps' version.
    #[arg(long)]
    pub(crate) tag: String,
    /// Configured target triple to assemble, instead of every configured
    /// target; repeat for several.
    #[arg(long = "target", value_name = "TRIPLE")]
    pub(crate) targets: Vec<String>,
}

/// Options for `notes`.
#[derive(Debug, Args)]
pub(crate) struct NotesOptions {
    /// Release tag, `v<semver>` or `<semver>`, naming the apps' version.
    #[arg(long)]
    pub(crate) tag: String,
    /// Prints only the release title instead of the body.
    #[arg(long)]
    pub(crate) title: bool,
}

/// Host boundaries and settings that commands run against.
pub(crate) struct Host<'host> {
    /// Cargo executable, normally `$CARGO` or `cargo`.
    pub(crate) cargo: OsString,
    pub(crate) file_system: &'host dyn FileSystem,
    pub(crate) process: &'host dyn ProcessRunner,
    /// `SOURCE_DATE_EPOCH` value that fixes archive modification times.
    pub(crate) source_date_epoch: Option<OsString>,
}

impl Host<'_> {
    /// Runs `command`, writing its output to `stdout`.
    ///
    /// # Errors
    /// Returns an error when the command fails or its output cannot be
    /// written.
    pub(crate) fn run(&self, command: &Command, stdout: &mut dyn Write) -> Result<(), String> {
        match command {
            Command::Plan(options) => self.plan(options, stdout),
            Command::Build(options) => build::run(self, options, stdout),
            Command::Assemble(options) => assemble::run(self, options, stdout),
            Command::Notes(options) => release_note::run(self, options, stdout),
        }
    }

    /// Returns each `include` entry of `workspace` with the file that
    /// supplies it for `app`, as [`Self::app_file`] resolves it.
    ///
    /// # Errors
    /// Returns an error naming the first entry and `app` when neither
    /// directory holds the file, or naming a path that cannot be inspected.
    pub(crate) fn include_files<'workspace>(
        &self,
        workspace: &'workspace Workspace,
        app: &App,
    ) -> Result<Vec<(&'workspace str, PathBuf)>, String> {
        workspace
            .config
            .include
            .iter()
            .map(|include| {
                let source = self.app_file(workspace, app, include)?.ok_or_else(|| {
                    format!(
                        "`workspace.metadata.release.include` entry `{include}` is not a file in \
                         the package directory of app `{}` or in the workspace root",
                        app.name
                    )
                })?;

                Ok((include.as_str(), source))
            })
            .collect()
    }

    /// Returns the file that supplies the relative `path` for `app`:
    /// `<package directory>/<path>` when it is a file, else
    /// `<workspace root>/<path>` when it is one, else `None`.
    ///
    /// # Errors
    /// Returns an error naming a path whose metadata is unreadable.
    pub(crate) fn app_file(
        &self,
        workspace: &Workspace,
        app: &App,
        path: &str,
    ) -> Result<Option<PathBuf>, String> {
        for directory in [&app.package_directory, &workspace.root] {
            let source = directory.join(path);
            if self.is_file(&source)? {
                return Ok(Some(source));
            }
        }

        Ok(None)
    }

    /// Returns whether `path` is a regular file.
    ///
    /// # Errors
    /// Returns an error naming the path when its metadata is unreadable.
    pub(crate) fn is_file(&self, path: &Path) -> Result<bool, String> {
        self.file_system
            .is_file(path)
            .map_err(|error| format!("failed to inspect `{}`: {error}", path.display()))
    }

    /// Returns the contents of the file at `path`.
    ///
    /// # Errors
    /// Returns an error naming the path when the file is unreadable.
    pub(crate) fn read_file(&self, path: &Path) -> Result<Vec<u8>, String> {
        self.file_system
            .read(path)
            .map_err(|error| format!("failed to read `{}`: {error}", path.display()))
    }

    /// Writes `contents` to the file at `path`, creating parent directories.
    ///
    /// # Errors
    /// Returns an error naming the path when the file cannot be written.
    pub(crate) fn write_file(&self, path: &Path, contents: &[u8]) -> Result<(), String> {
        self.file_system
            .write(path, contents)
            .map_err(|error| format!("failed to write `{}`: {error}", path.display()))
    }

    /// Prints each written file's path on its own line.
    ///
    /// # Errors
    /// Returns an error when `stdout` cannot be written.
    pub(crate) fn report_written(stdout: &mut dyn Write, paths: &[PathBuf]) -> Result<(), String> {
        paths
            .iter()
            .try_for_each(|path| writeln!(stdout, "{}", path.display()))
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("failed to write the command output: {error}"))
    }

    /// Writes the pretty-printed plan JSON with a trailing newline.
    fn plan(&self, options: &PlanOptions, stdout: &mut dyn Write) -> Result<(), String> {
        let tag = options.tag.as_deref().map(ReleaseTag::parse).transpose()?;
        let mut workspace = Workspace::load(self.process, self.cargo.clone())?;
        workspace.config.select_targets(&options.targets)?;
        for app in &workspace.apps {
            self.include_files(&workspace, app)?;
        }
        let plan = ReleasePlan::new(&workspace, tag.as_ref())?;

        serde_json::to_writer_pretty(&mut *stdout, &plan)
            .map_err(io::Error::from)
            .and_then(|()| writeln!(stdout))
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("failed to write the release plan: {error}"))
    }
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
