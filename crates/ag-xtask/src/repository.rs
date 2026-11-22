//! Read-only repository facts shared by checks: the Git file inventory and the
//! hook IDs in the `prek` catalog.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Deserialize;

/// Returns tracked and untracked, non-ignored files below `root`, relative to
/// it, without taking optional Git locks.
///
/// # Errors
/// Returns an error when Git cannot run, fails, or prints a non-UTF-8 listing.
pub(crate) fn git_files(git: &Path, root: &Path) -> Result<Vec<PathBuf>, String> {
    let output = Command::new(git)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .current_dir(root)
        .output()
        .map_err(|error| format!("Failed to run git ls-files: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let listing = String::from_utf8(output.stdout)
        .map_err(|error| format!("Invalid UTF-8 in git ls-files: {error}"))?;

    Ok(listing
        .split('\0')
        .filter(|name| !name.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Returns every hook ID that the `.pre-commit-config.yaml` text `catalog`
/// declares, across local and remote hook repositories.
///
/// # Errors
/// Returns an error when the catalog is not valid YAML or lacks the
/// repository and hook lists.
pub(crate) fn hook_ids(catalog: &str) -> Result<BTreeSet<String>, String> {
    let catalog: Catalog = serde_yaml_ng::from_str(catalog)
        .map_err(|error| format!("Invalid hook catalog: {error}"))?;

    Ok(catalog
        .repos
        .into_iter()
        .flat_map(|repo| repo.hooks.into_iter().map(|hook| hook.id))
        .collect())
}

/// Required hook-catalog structure; other hook configuration is irrelevant
/// here.
#[derive(Deserialize)]
struct Catalog {
    repos: Vec<HookRepository>,
}

#[derive(Deserialize)]
struct HookRepository {
    hooks: Vec<Hook>,
}

#[derive(Deserialize)]
struct Hook {
    id: String,
}

#[cfg(test)]
#[path = "repository_test.rs"]
mod tests;
