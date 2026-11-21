//! `cargo semver-checks` over the published library crates that have a
//! crates.io baseline.

use std::process::{Output, Stdio};

use clap::Args;
use serde_json::Value;
use tracing::info;

use crate::cargo_environment;

/// Arguments for the published-crate semver check.
#[derive(Args)]
pub(crate) struct Options {
    /// Workspace package to leave unchecked, such as an application no crate
    /// depends on.
    #[arg(long)]
    exclude: Vec<String>,
}

/// Runs `cargo semver-checks` for every publishable library crate except
/// the excluded ones, skipping crates crates.io does not list yet because a
/// new crate has no baseline to compare with.
///
/// # Errors
///
/// Returns an error when Cargo metadata cannot be read, an excluded name is
/// not a workspace package, a registry lookup fails for another reason than
/// a missing crate, or `cargo semver-checks` reports failures.
pub(crate) fn run(options: &Options) -> Result<(), String> {
    let metadata = cargo(&["metadata", "--no-deps", "--format-version", "1"])?;
    if !metadata.status.success() {
        return Err(format!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&metadata.stderr).trim()
        ));
    }
    let candidates = publishable_libraries(&metadata.stdout, &options.exclude)?;
    let mut packages = Vec::new();
    for name in candidates {
        let lookup = cargo(&["info", "--registry", "crates-io", &name])?;
        match Baseline::from_lookup(&name, &lookup)? {
            Baseline::Missing => info!("Skipping {name}: new crate, no crates.io baseline"),
            Baseline::Published => packages.push(name),
        }
    }
    if packages.is_empty() {
        info!("No published library crate has a crates.io baseline to check");

        return Ok(());
    }
    let mut arguments = vec!["semver-checks"];
    arguments.extend(
        packages
            .iter()
            .flat_map(|name| ["--package", name.as_str()]),
    );
    let checks = cargo_with_output(&arguments, Stdio::inherit)?;
    if !checks.status.success() {
        return Err("cargo semver-checks reported failures".to_string());
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
    cargo_with_output(arguments, Stdio::piped)
}

/// Runs a Cargo subcommand with `stdio` for its standard output and error.
fn cargo_with_output(arguments: &[&str], stdio: fn() -> Stdio) -> Result<Output, String> {
    cargo_environment::command("cargo")
        .args(arguments)
        .stdout(stdio())
        .stderr(stdio())
        .output()
        .map_err(|error| format!("Could not execute cargo: {error}"))
}

#[cfg(test)]
#[path = "semver_check_test.rs"]
mod tests;
