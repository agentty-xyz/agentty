//! Release publication must follow the workspace's registry dependency graph.

use std::collections::BTreeMap;
use std::error::Error;
use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

#[test]
fn publish_steps_include_every_publishable_crate_after_its_dependencies()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .ok_or("missing workspace root")?;
    let workflow: serde_yaml_ng::Value = serde_yaml_ng::from_str(&fs::read_to_string(
        root.join(".github/workflows/publish-crates-io.yml"),
    )?)?;
    let steps = workflow["jobs"]["publish"]["steps"]
        .as_sequence()
        .ok_or("missing publication steps")?;
    let published = steps
        .iter()
        .filter_map(|step| {
            let arguments = step["run"].as_str()?.split_whitespace().collect::<Vec<_>>();
            if !arguments.starts_with(&["cargo", "publish"]) {
                return None;
            }
            arguments
                .windows(2)
                .find(|pair| pair[0] == "-p")
                .map(|pair| pair[1].to_string())
        })
        .collect::<Vec<_>>();
    // Act
    let output = Command::new("cargo")
        .args([
            "metadata",
            "--locked",
            "--offline",
            "--no-deps",
            "--format-version",
            "1",
        ])
        .current_dir(root)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let metadata: Value = serde_json::from_slice(&output.stdout)?;
    let packages = metadata["packages"].as_array().ok_or("missing packages")?;
    let positions = published
        .iter()
        .enumerate()
        .map(|(index, name)| (name.as_str(), index))
        .collect::<BTreeMap<_, _>>();
    // Assert
    assert_eq!(
        positions.len(),
        published.len(),
        "duplicate publication step"
    );
    let publishable = packages
        .iter()
        .filter(|package| {
            package["publish"]
                .as_array()
                .is_none_or(|registries| !registries.is_empty())
        })
        .collect::<Vec<_>>();
    assert_eq!(
        positions.len(),
        publishable.len(),
        "publication plan differs from workspace"
    );
    for package in publishable {
        let name = package["name"].as_str().ok_or("missing package name")?;
        let position = positions
            .get(name)
            .ok_or_else(|| format!("{name} is not published"))?;
        for dependency in package["dependencies"]
            .as_array()
            .ok_or("missing dependencies")?
        {
            if dependency["path"].is_null() || dependency["kind"] == "dev" {
                continue;
            }
            let dependency_name = dependency["name"]
                .as_str()
                .ok_or("missing dependency name")?;
            let dependency_position = positions
                .get(dependency_name)
                .ok_or_else(|| format!("{name} depends on unpublished {dependency_name}"))?;
            assert!(
                dependency_position < position,
                "{dependency_name} must be published before {name}"
            );
        }
    }

    Ok(())
}
