//! Decides whether a change needs the published-crate release checks.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;

use clap::Args;
use tracing::info;

use crate::validation::Changes;

/// Files whose change can break the published-crate checks themselves, so a
/// toolchain or tooling update fails on its own pull request instead of on
/// the next release.
const CHECK_INPUTS: &[&str] = &[
    ".github/actions/setup-rust-prek/action.yml",
    ".github/workflows/published-crates.yml",
    ".pre-commit-config.yaml",
    "crates/ag-xtask/src/release_gate.rs",
    "crates/ag-xtask/src/semver_check.rs",
    "rust-toolchain.toml",
];

/// Arguments for the release-check decision.
#[derive(Args)]
pub(crate) struct Options {
    /// Base revision whose fork point bounds the change.
    #[arg(long, default_value = "origin/main")]
    base: String,
    /// File that receives `run=true` or `run=false`, such as
    /// `$GITHUB_OUTPUT`.
    #[arg(long)]
    output: PathBuf,
}

/// Appends `run=true` to the output file when the change moves
/// `[workspace.package].version` or edits a check input, else `run=false`.
///
/// # Errors
///
/// Returns an error when Git fails, when either manifest has no
/// `[workspace.package].version`, or when the output cannot be written.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let fork = git(&[
        "merge-base",
        "HEAD",
        &format!("{}^{{commit}}", options.base),
    ])?;
    let base_manifest = git(&["show", &format!("{}:Cargo.toml", fork.trim())])?;
    let changes = Changes::collect(&options.base)?;
    let head_manifest = fs::read_to_string(changes.root.join("Cargo.toml"))
        .map_err(|error| format!("Could not read the workspace Cargo.toml: {error}"))?;
    let decision = Decision::new(
        &workspace_version(&base_manifest)?,
        &workspace_version(&head_manifest)?,
        &changes.files,
    );
    info!("{}", decision.reason());

    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&options.output)
        .and_then(|mut output| writeln!(output, "run={}", decision.runs()))
        .map_err(|error| format!("Could not write {}: {error}", options.output.display()))
}

/// Why the release checks run or are skipped.
#[derive(Debug, PartialEq)]
enum Decision {
    /// The change edits files the checks depend on.
    InputsChanged(Vec<PathBuf>),
    /// Neither the version nor a check input changed.
    Unchanged,
    /// The change moves the workspace version, as release preparation does.
    VersionChanged { from: String, to: String },
}

impl Decision {
    /// Classifies a change by its workspace versions and changed files.
    fn new(base_version: &str, head_version: &str, files: &[PathBuf]) -> Self {
        if base_version != head_version {
            return Self::VersionChanged {
                from: base_version.to_string(),
                to: head_version.to_string(),
            };
        }
        let inputs = files
            .iter()
            .filter(|path| CHECK_INPUTS.iter().any(|input| Path::new(input) == *path))
            .cloned()
            .collect::<Vec<_>>();
        if inputs.is_empty() {
            return Self::Unchanged;
        }

        Self::InputsChanged(inputs)
    }

    /// Returns whether the release checks run.
    fn runs(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }

    /// Describes the decision for the job log.
    fn reason(&self) -> String {
        match self {
            Self::InputsChanged(paths) => format!(
                "Release checks run: their inputs changed: {}",
                paths
                    .iter()
                    .map(|path| path.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Self::Unchanged => "Release checks skipped: the workspace version and their inputs \
                                are unchanged"
                .to_string(),
            Self::VersionChanged { from, to } => {
                format!("Release checks run: the workspace version moves from {from} to {to}")
            }
        }
    }
}

/// Reads `[workspace.package].version` from a workspace manifest.
fn workspace_version(manifest: &str) -> Result<String, String> {
    let manifest = manifest
        .parse::<toml::Table>()
        .map_err(|error| format!("Could not parse the workspace Cargo.toml: {error}"))?;

    manifest
        .get("workspace")
        .and_then(|workspace| workspace.get("package"))
        .and_then(|package| package.get("version"))
        .and_then(toml::Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| "The workspace Cargo.toml has no [workspace.package].version".to_string())
}

/// Runs Git and returns its standard output.
fn git(arguments: &[&str]) -> Result<String, String> {
    let output = Command::new("git")
        .args(arguments)
        .output()
        .map_err(|error| format!("Could not execute git: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
#[path = "release_gate_test.rs"]
mod tests;
