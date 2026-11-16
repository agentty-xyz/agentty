//! Rust formatting limited to the files changed since the branch fork point.

use std::path::{Path, PathBuf};

use clap::Args;
use tracing::info;

use crate::cargo_environment;
use crate::validation::Changes;

/// Files whose change can reformat code that the branch did not touch.
const WORKSPACE_TRIGGERS: &[&str] = &[
    ".rustfmt.toml",
    "rust-toolchain",
    "rust-toolchain.toml",
    "rustfmt.toml",
];

/// Arguments for changed-file Rust formatting.
#[derive(Args)]
pub(crate) struct Options {
    /// Base revision whose fork point bounds the changed files.
    #[arg(long, default_value = "origin/main")]
    base: String,
}

/// Formats changed Rust files with `rustfmt`, or the whole workspace with
/// `cargo fmt --all` when formatter configuration or the toolchain changed.
///
/// Unchanged files on a branch already match `main`, whose formatting CI
/// enforces, so formatting only changed files gives the workspace-wide
/// result at a fraction of the cost.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let changes = Changes::collect(&options.base)?;
    let selection =
        Selection::from_changes(&changes.files, |path| changes.root.join(path).is_file());
    let Some((program, arguments)) = selection.command() else {
        info!("No changed Rust files to format");

        return Ok(());
    };
    let status = cargo_environment::command(program)
        .args(arguments)
        .current_dir(&changes.root)
        .status()
        .map_err(|error| format!("Could not execute {program}: {error}"))?;
    if !status.success() {
        return Err(format!("{program} reported failures"));
    }

    Ok(())
}

/// Formatting scope derived from a change set.
#[derive(Debug, PartialEq)]
enum Selection {
    /// Existing changed Rust files, relative to the repository root.
    Files(Vec<PathBuf>),
    /// No changed Rust file remains to format.
    Nothing,
    /// Formatter configuration changed, so every workspace file may change.
    Workspace,
}

impl Selection {
    /// Selects the workspace for configuration changes, otherwise the changed
    /// Rust files that `exists` reports as present.
    fn from_changes(files: &[PathBuf], exists: impl Fn(&Path) -> bool) -> Self {
        if files.iter().any(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| WORKSPACE_TRIGGERS.contains(&name))
        }) {
            return Self::Workspace;
        }
        let files = files
            .iter()
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .filter(|path| exists(path))
            .cloned()
            .collect::<Vec<_>>();
        if files.is_empty() {
            return Self::Nothing;
        }

        Self::Files(files)
    }

    /// Returns the formatter program and arguments, or `None` when nothing
    /// needs formatting. Changed files skip their child modules, which are
    /// either unchanged or selected themselves.
    fn command(&self) -> Option<(&'static str, Vec<String>)> {
        match self {
            Self::Files(files) => Some((
                "rustfmt",
                ["--config".to_string(), "skip_children=true".to_string()]
                    .into_iter()
                    .chain(files.iter().map(|path| path.display().to_string()))
                    .collect(),
            )),
            Self::Nothing => None,
            Self::Workspace => Some(("cargo", vec!["fmt".into(), "--all".into()])),
        }
    }
}

#[cfg(test)]
#[path = "format_changed_test.rs"]
mod tests;
