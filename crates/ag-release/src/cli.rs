//! Command-line interface and command dispatch.

use std::ffi::OsString;
use std::io::{self, Write};

use clap::{Args, Parser, Subcommand};

use crate::file_system::FileSystem;
use crate::plan::ReleasePlan;
use crate::process::ProcessRunner;
use crate::tag::ReleaseTag;
use crate::workspace::Workspace;

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
}

/// Options for `plan`.
#[derive(Debug, Args)]
pub(crate) struct PlanOptions {
    /// Release tag, `v<semver>` or `<semver>`, that every app's version must
    /// match.
    #[arg(long)]
    pub(crate) tag: Option<String>,
}

/// Host boundaries and settings that commands run against.
pub(crate) struct Host<'host> {
    /// Cargo executable, normally `$CARGO` or `cargo`.
    pub(crate) cargo: OsString,
    pub(crate) file_system: &'host dyn FileSystem,
    pub(crate) process: &'host dyn ProcessRunner,
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
        }
    }

    /// Writes the pretty-printed plan JSON with a trailing newline.
    fn plan(&self, options: &PlanOptions, stdout: &mut dyn Write) -> Result<(), String> {
        let tag = options.tag.as_deref().map(ReleaseTag::parse).transpose()?;
        let workspace = Workspace::load(self.process, self.cargo.clone())?;
        self.check_include(&workspace)?;
        let plan = ReleasePlan::new(&workspace, tag.as_ref())?;

        serde_json::to_writer_pretty(&mut *stdout, &plan)
            .map_err(io::Error::from)
            .and_then(|()| writeln!(stdout))
            .and_then(|()| stdout.flush())
            .map_err(|error| format!("failed to write the release plan: {error}"))
    }

    /// Checks that every `include` entry is a file in the workspace root.
    fn check_include(&self, workspace: &Workspace) -> Result<(), String> {
        for include in &workspace.config.include {
            let path = workspace.root.join(include);
            let is_file = self
                .file_system
                .is_file(&path)
                .map_err(|error| format!("failed to inspect `{}`: {error}", path.display()))?;
            if !is_file {
                return Err(format!(
                    "`workspace.metadata.release.include` entry `{include}` is not a file in the \
                     workspace root"
                ));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "cli_test.rs"]
mod tests;
