//! `cargo semver-checks` over the published library crates that have a
//! usable crates.io baseline.

use std::process::{Output, Stdio};

use clap::Args;
use serde_json::Value;
use tracing::{info, warn};

use crate::cargo_environment;

/// Why a crate whose newest crates.io release fails to build is skipped.
const BROKEN_BASELINE: &str = "its newest crates.io release does not build, so it cannot serve as \
                               a semver baseline; yank it or publish a release that builds";

/// Arguments for the published-crate semver check.
#[derive(Args)]
pub(crate) struct Options {
    /// Workspace package to leave unchecked, such as an application no crate
    /// depends on.
    #[arg(long)]
    exclude: Vec<String>,
    /// Release type to assume instead of deriving it from the workspace
    /// version. `major` still builds the current and baseline rustdoc of
    /// every crate, but runs no lint.
    #[arg(long, value_parser = ["major", "minor", "patch"])]
    release_type: Option<String>,
}

/// Runs `cargo semver-checks` for every publishable library crate except
/// the excluded ones, one crate at a time.
///
/// A crate crates.io does not list yet is skipped, because a new crate has
/// no baseline. So is a crate whose newest crates.io release no longer
/// builds, with a warning naming that release, because no API can be read
/// from it. A workspace crate whose own rustdoc fails is a failure.
///
/// # Errors
///
/// Returns an error when Cargo metadata cannot be read, an excluded name is
/// not a workspace package, a registry lookup fails for another reason than
/// a missing crate, or a crate's rustdoc or semver check fails.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let metadata = cargo(&["metadata", "--no-deps", "--format-version", "1"])?;
    if !metadata.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata.stderr).trim()
        ));
    }
    let candidates = publishable_libraries(&metadata.stdout, &options.exclude)?;
    let mut failures = Vec::new();
    for name in candidates {
        let lookup = cargo(&["info", "--registry", "crates-io", &name])?;
        if Baseline::from_lookup(&name, &lookup)? == Baseline::Missing {
            info!("Skipping {name}: new crate, no crates.io baseline");

            continue;
        }
        let mut arguments = vec!["semver-checks", "--color", "never"];
        if let Some(release_type) = &options.release_type {
            arguments.extend(["--release-type", release_type.as_str()]);
        }
        arguments.extend(["--package", name.as_str()]);
        // Its report goes to standard error, which is kept to classify a
        // failure and then logged.
        let checks = cargo_with_output(&arguments, Stdio::inherit, Stdio::piped)?;
        let report = String::from_utf8_lossy(&checks.stderr);
        info!("cargo semver-checks report for {name}:\n{report}");
        match Outcome::from_report(&name, checks.status.success(), &report) {
            Outcome::BrokenBaseline => {
                warn!("Skipping {name}: {BROKEN_BASELINE}");
            }
            Outcome::BrokenCurrent => failures.push(format!(
                "{name}: the workspace crate's rustdoc does not build; see its error above"
            )),
            Outcome::Breaking => failures.push(format!(
                "{name}: public API changes need a larger version bump"
            )),
            Outcome::Passed => {}
        }
    }
    if !failures.is_empty() {
        return Err(format!(
            "cargo semver-checks reported failures:\n{}",
            failures.join("\n")
        ));
    }

    Ok(())
}

/// Whether crates.io holds a release to compare a crate with.
#[derive(Debug, PartialEq)]
enum Baseline {
    /// crates.io does not list the crate.
    Missing,
    /// crates.io lists at least one release.
    Published,
}

impl Baseline {
    /// Classifies a `cargo info --registry crates-io` result. Only Cargo's
    /// not-found error counts as missing, so network and index failures stay
    /// failures.
    fn from_lookup(name: &str, lookup: &Output) -> Result<Self, String> {
        if lookup.status.success() {
            return Ok(Self::Published);
        }
        let stderr = String::from_utf8_lossy(&lookup.stderr);
        if stderr.contains(&format!("could not find `{name}` in registry")) {
            return Ok(Self::Missing);
        }

        Err(format!(
            "Could not look up {name} on crates.io: {}",
            stderr.trim()
        ))
    }
}

/// Result of one crate's `cargo semver-checks` run.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// The newest crates.io release failed to build its rustdoc.
    BrokenBaseline,
    /// The workspace crate failed to build its rustdoc.
    BrokenCurrent,
    /// Lints found changes the release type does not allow.
    Breaking,
    /// Both rustdoc builds and every lint passed.
    Passed,
}

impl Outcome {
    /// Classifies a run from its exit status and uncolored report.
    /// `cargo semver-checks` builds the current crate before the baseline and
    /// announces each build as `Building <name> v<version> (<side>)`, so a
    /// rustdoc failure after the baseline announcement is the baseline's.
    fn from_report(name: &str, success: bool, report: &str) -> Self {
        if success {
            return Self::Passed;
        }
        if !report.contains(&format!("failed to build rustdoc for crate {name} v")) {
            return Self::Breaking;
        }
        let building = format!("Building {name} v");
        let baseline_started = report.lines().any(|line| {
            let line = line.trim();

            line.starts_with(&building) && line.ends_with("(baseline)")
        });
        if baseline_started {
            return Self::BrokenBaseline;
        }

        Self::BrokenCurrent
    }
}

/// Returns the names of the workspace packages that Cargo may publish and
/// that have a library target, minus `exclude`, in metadata order.
fn publishable_libraries(metadata: &[u8], exclude: &[String]) -> Result<Vec<String>, String> {
    let metadata = serde_json::from_slice::<Value>(metadata)
        .map_err(|error| format!("Could not parse cargo metadata: {error}"))?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or("cargo metadata has no package list")?;
    let names = packages
        .iter()
        .filter_map(|package| package.get("name").and_then(Value::as_str))
        .collect::<Vec<_>>();
    if let Some(unknown) = exclude.iter().find(|name| !names.contains(&name.as_str())) {
        return Err(format!(
            "Excluded package {unknown} is not a workspace package"
        ));
    }

    Ok(packages
        .iter()
        .filter(|package| {
            // `publish = false` is an empty registry list; `null` allows any.
            package
                .get("publish")
                .and_then(Value::as_array)
                .is_none_or(|registries| !registries.is_empty())
        })
        .filter(|package| {
            package
                .get("targets")
                .and_then(Value::as_array)
                .is_some_and(|targets| {
                    targets.iter().any(|target| {
                        target
                            .get("kind")
                            .and_then(Value::as_array)
                            .is_some_and(|kinds| kinds.iter().any(|kind| kind == "lib"))
                    })
                })
        })
        .filter_map(|package| package.get("name").and_then(Value::as_str))
        .filter(|name| !exclude.iter().any(|excluded| excluded == name))
        .map(str::to_string)
        .collect())
}

/// Runs a Cargo subcommand and captures its output.
fn cargo(arguments: &[&str]) -> Result<Output, String> {
    cargo_with_output(arguments, Stdio::piped, Stdio::piped)
}

/// Runs a Cargo subcommand with the given standard output and error.
fn cargo_with_output(
    arguments: &[&str],
    stdout: fn() -> Stdio,
    stderr: fn() -> Stdio,
) -> Result<Output, String> {
    cargo_environment::command("cargo")
        .args(arguments)
        .stdout(stdout())
        .stderr(stderr())
        .output()
        .map_err(|error| format!("Could not execute cargo: {error}"))
}

#[cfg(test)]
#[path = "semver_check_test.rs"]
mod tests;
