//! The Native Harness Sandbox pull request path filter must name exactly the
//! workspace crates that `ag-harness` and `ag-agent` build from, so a pull
//! request that changes one of them still qualifies the macOS launcher and
//! Agentty's sandbox grants.

use std::collections::{BTreeSet, HashMap};
use std::error::Error;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as Json;
use serde_yaml_ng::Value as Yaml;

/// Packages whose sandbox suites the workflow runs on macOS.
const ROOT_PACKAGES: [&str; 2] = ["ag-harness", "ag-agent"];

#[test]
fn pull_request_filter_names_every_sandboxed_workspace_crate() -> Result<(), Box<dyn Error>> {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()?;
    let workflow: Yaml = serde_yaml_ng::from_str(include_str!(
        "../../../.github/workflows/harness-sandbox.yml"
    ))?;
    let filtered_crates = workflow["on"]["pull_request"]["paths"]
        .as_sequence()
        .ok_or("pull_request paths")?
        .iter()
        .map(|path| path.as_str().ok_or("path filter entry"))
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .filter(|path| path.starts_with("crates/"))
        .map(str::to_string)
        .collect::<BTreeSet<_>>();

    // Act
    let built_crates = sandboxed_workspace_crates(&root)?
        .into_iter()
        .map(|directory| format!("{}/**", directory.display()))
        .collect::<BTreeSet<_>>();

    // Assert
    assert_eq!(
        filtered_crates, built_crates,
        "`.github/workflows/harness-sandbox.yml` must filter on every workspace crate \
         `ag-harness` and `ag-agent` build from, and no others"
    );

    Ok(())
}

/// Returns the repository-relative directories of the root packages and the
/// workspace crates they build from: their own normal, build, and dev
/// dependencies, then those dependencies' normal and build dependencies
/// transitively.
fn sandboxed_workspace_crates(root: &Path) -> Result<BTreeSet<PathBuf>, Box<dyn Error>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--locked", "--no-deps", "--format-version", "1"])
        .current_dir(root)
        .output()?;
    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).into_owned().into());
    }
    let metadata: Json = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"]
        .as_array()
        .ok_or("packages")?
        .iter()
        .map(|package| Ok((package["name"].as_str().ok_or("package name")?, package)))
        .collect::<Result<HashMap<_, _>, Box<dyn Error>>>()?;
    let mut pending = ROOT_PACKAGES.map(|package| (package, true)).to_vec();
    let mut seen = BTreeSet::new();
    let mut directories = BTreeSet::new();
    while let Some((name, is_root)) = pending.pop() {
        if !seen.insert(name) {
            continue;
        }
        let package = packages.get(name).ok_or("workspace package")?;
        let manifest = PathBuf::from(package["manifest_path"].as_str().ok_or("manifest path")?);
        let directory = manifest.parent().ok_or("package directory")?;
        directories.insert(directory.canonicalize()?.strip_prefix(root)?.to_path_buf());
        for dependency in package["dependencies"].as_array().ok_or("dependencies")? {
            let is_dev = dependency["kind"].as_str() == Some("dev");
            if dependency["path"].is_null() || (is_dev && !is_root) {
                continue;
            }
            pending.push((dependency["name"].as_str().ok_or("dependency name")?, false));
        }
    }

    Ok(directories)
}
