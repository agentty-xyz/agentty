//! Strict Clippy, or a compile-only check, limited to the packages that
//! changed files can affect.

use std::path::PathBuf;

use clap::Args;

use crate::cargo_environment;
use crate::validation::{self, Metadata};

/// Explicit package selection supplied only to a coordinated Clippy hook.
pub(crate) const PACKAGES_ENV: &str = "AGENTTY_CLIPPY_PACKAGES";

/// Arguments for affected-package Clippy and compile checks.
#[derive(Args)]
pub(crate) struct Options {
    /// Lint Agentty's explicit PTY targets only when Agentty is affected.
    #[arg(long)]
    agentty_e2e: bool,
    /// Run `cargo check` instead of Clippy to report compile errors quickly.
    #[arg(long)]
    check: bool,
    /// Changed files, relative to the workspace root.
    files: Vec<PathBuf>,
}

/// Lints, or with `--check` compile-checks, affected packages and, when
/// Agentty is affected, its explicit PTY targets. Shared files select the
/// workspace. Coordinated validation supplies an authoritative package
/// selection through [`PACKAGES_ENV`].
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
        // `cargo metadata` already succeeded from this directory.
        let root = std::env::current_dir().map_err(validation::error_text)?;
        match metadata.owners(&options.files, &root) {
            Some(owners) => Some(metadata.affected(&owners)?),
            None => None,
        }
    };
    let mut failed = false;
    for agentty_e2e in [false, true]
        .into_iter()
        .filter(|agentty_e2e| *agentty_e2e || !options.agentty_e2e)
    {
        let Some(arguments) = cargo_arguments(packages.as_deref(), agentty_e2e, options.check)
        else {
            continue;
        };
        let status = cargo_environment::command("cargo")
            .args(arguments)
            .status()
            .map_err(|error| format!("Could not execute cargo: {error}"))?;
        failed |= !status.success();
    }
    if failed {
        let tool = if options.check {
            "cargo check"
        } else {
            "Clippy"
        };

        return Err(format!("{tool} reported failures"));
    }

    Ok(())
}

/// Builds strict Clippy arguments, or compile-check arguments when `check` is
/// set, retaining workspace-wide fallback when no package is selected.
/// Returns `None` for unaffected explicit PTY targets.
fn cargo_arguments(
    packages: Option<&[String]>,
    agentty_e2e: bool,
    check: bool,
) -> Option<Vec<String>> {
    let subcommand = if check { "check" } else { "clippy" };
    let mut arguments = [subcommand, "-q", "--locked"].map(String::from).to_vec();
    if agentty_e2e {
        if packages.is_some_and(|packages| {
            !packages.is_empty() && !packages.iter().any(|package| package == "agentty")
        }) {
            return None;
        }
        arguments.extend(
            [
                "-p",
                "agentty",
                "--test",
                "e2e",
                "--test",
                "protocol_compliance_e2e",
                "--test",
                "showcase",
            ]
            .map(String::from),
        );
    } else {
        arguments.extend(["--all-targets", "--all-features"].map(String::from));
        match packages {
            Some(packages) if !packages.is_empty() => {
                for package in packages {
                    arguments.extend(["-p".to_string(), package.clone()]);
                }
            }
            _ => arguments.push("--workspace".into()),
        }
    }
    // Clippy enforces warnings; failing compile checks on them would only
    // duplicate that gate.
    if !check {
        arguments.extend(["--", "-D", "warnings"].map(String::from));
    }

    Some(arguments)
}

#[cfg(test)]
#[path = "affected_clippy_test.rs"]
mod tests;
