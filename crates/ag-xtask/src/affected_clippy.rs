//! Strict Clippy limited to the packages that changed files can affect.

use std::path::PathBuf;

use clap::Args;

use crate::cargo_environment;
use crate::validation::Metadata;

/// Explicit package selection supplied only to a coordinated Clippy hook.
pub(crate) const PACKAGES_ENV: &str = "AGENTTY_CLIPPY_PACKAGES";

/// Arguments for affected-package Clippy.
#[derive(Args)]
pub(crate) struct Options {
    /// Changed files, relative to the workspace root.
    files: Vec<PathBuf>,
}

/// Lints the packages owning `files` and their dependents, or the whole
/// workspace when a file belongs to no package. Coordinated validation
/// supplies an authoritative package selection through [`PACKAGES_ENV`].
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let metadata = Metadata::load()?;
    let packages = if let Ok(packages) = std::env::var(PACKAGES_ENV) {
        let packages = packages
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>();
        if packages.is_empty() {
            return Err(format!("{PACKAGES_ENV} must contain package names"));
        }

        Some(metadata.affected(&packages)?)
    } else {
        let root = std::env::current_dir().map_err(|error| error.to_string())?;
        match metadata.owners(&options.files, &root) {
            Some(owners) => Some(metadata.affected(&owners)?),
            None => None,
        }
    };
    let status = cargo_environment::command("cargo")
        .args(clippy_arguments(packages.as_deref()))
        .status()
        .map_err(|error| format!("Could not execute cargo: {error}"))?;
    if !status.success() {
        return Err("Clippy reported failures".into());
    }

    Ok(())
}

/// Builds `cargo clippy` arguments for `packages`, or for the whole
/// workspace when no package is selected.
fn clippy_arguments(packages: Option<&[String]>) -> Vec<String> {
    let mut arguments = [
        "clippy",
        "-q",
        "--locked",
        "--all-targets",
        "--all-features",
    ]
    .map(String::from)
    .to_vec();
    match packages {
        Some(packages) if !packages.is_empty() => {
            for package in packages {
                arguments.extend(["-p".to_string(), package.clone()]);
            }
        }
        _ => arguments.push("--workspace".into()),
    }
    arguments.extend(["--", "-D", "warnings"].map(String::from));

    arguments
}

#[cfg(test)]
#[path = "affected_clippy_test.rs"]
mod tests;
