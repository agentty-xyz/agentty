//! Manifest inheritance and Tokio feature check unit tests.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use tempfile::tempdir;

use crate::check_manifest::{
    Dependency, ManifestCheck, MockWorkspace, Package, RealWorkspace, Workspace, run,
};

/// Member manifest that inherits every dependency, including target-specific
/// ones and dotted `name.workspace = true` keys.
const INHERITED_MANIFEST: &str = r#"
[package]
name = "example"

[dependencies]
serde.workspace = true
tokio = { workspace = true, features = ["net"] }

[dev-dependencies]
tempfile.workspace = true

[target.'cfg(unix)'.dependencies]
rustix.workspace = true
"#;

fn dependency(name: &str, features: &[&str]) -> Dependency {
    Dependency {
        features: features.iter().map(ToString::to_string).collect(),
        name: name.to_string(),
    }
}

fn package(manifest: &str, dependencies: Vec<Dependency>) -> Package {
    Package {
        dependencies,
        manifest: manifest.to_string(),
        manifest_path: PathBuf::from("crates/example/Cargo.toml"),
    }
}

fn check_packages(packages: Vec<Package>) -> Result<(), String> {
    let mut workspace = MockWorkspace::new();
    workspace
        .expect_packages()
        .once()
        .return_once(move || Ok(packages));

    ManifestCheck {
        workspace: &workspace,
    }
    .run()
}

#[test]
fn inherited_dependencies_and_selected_tokio_features_pass() {
    // Arrange
    let packages = vec![package(
        INHERITED_MANIFEST,
        vec![dependency("tokio", &["net", "time"])],
    )];

    // Act
    let result = check_packages(packages);

    // Assert
    assert_eq!(result, Ok(()));
}

#[test]
fn uninherited_dependencies_and_full_tokio_are_reported_together() {
    // Arrange
    let manifest = r#"
[dependencies]
regex = "1"
serde = { version = "1", features = ["derive"] }

[build-dependencies]
cc = { workspace = false }

[target.'cfg(unix)'.dev-dependencies]
libc = "0.2"

[target.'cfg(windows)']
dependencies = "invalid"
"#;
    let packages = vec![package(
        manifest,
        vec![
            dependency("tokio", &["full"]),
            dependency("tokio-util", &["full"]),
        ],
    )];

    // Act
    let result = check_packages(packages);

    // Assert
    let fix = "must use `workspace = true`; declare it under `[workspace.dependencies]` in the \
               root `Cargo.toml`";
    assert_eq!(
        result,
        Err([
            "Manifest violations:".to_string(),
            format!("crates/example/Cargo.toml: [dependencies] `regex` {fix}"),
            format!("crates/example/Cargo.toml: [dependencies] `serde` {fix}"),
            format!("crates/example/Cargo.toml: [build-dependencies] `cc` {fix}"),
            format!("crates/example/Cargo.toml: [target.cfg(unix).dev-dependencies] `libc` {fix}"),
            "crates/example/Cargo.toml: [target.cfg(windows).dependencies] must be a table"
                .to_string(),
            "crates/example/Cargo.toml: `tokio` enables `full`; enable only the Tokio features \
             the crate uses"
                .to_string(),
        ]
        .join("\n"))
    );
}

#[test]
fn unparsable_manifests_and_workspace_errors_fail_the_check() {
    // Arrange
    let invalid = vec![package("[dependencies", Vec::new())];
    let mut unavailable = MockWorkspace::new();
    unavailable
        .expect_packages()
        .once()
        .return_once(|| Err("cargo metadata failed: offline".to_string()));

    // Act
    let parse_result = check_packages(invalid);
    let workspace_result = ManifestCheck {
        workspace: &unavailable,
    }
    .run();

    // Assert
    assert!(
        parse_result
            .expect_err("invalid manifest")
            .starts_with("Failed to parse crates/example/Cargo.toml:")
    );
    assert_eq!(
        workspace_result,
        Err("cargo metadata failed: offline".to_string())
    );
}

#[test]
fn real_workspace_reads_members_with_root_relative_manifest_paths() {
    // Arrange
    let workspace = RealWorkspace {
        cargo: OsString::from(env!("CARGO")),
    };

    // Act
    let packages = workspace.packages().expect("workspace metadata");

    // Assert
    let xtask = packages
        .iter()
        .find(|package| package.manifest_path == Path::new("crates/ag-xtask/Cargo.toml"))
        .expect("ag-xtask member");
    assert!(xtask.manifest.contains("name = \"ag-xtask\""));
    assert!(
        xtask
            .dependencies
            .iter()
            .any(|dependency| dependency.name == "toml")
    );
}

#[test]
fn real_workspace_reports_cargo_metadata_failures() {
    // Arrange
    let failing = RealWorkspace {
        cargo: OsString::from("false"),
    };
    let missing = RealWorkspace {
        cargo: OsString::from("ag-xtask-missing-cargo"),
    };

    // Act
    let failing_result = failing.packages();
    let missing_result = missing.packages();

    // Assert
    assert_eq!(failing_result, Err("cargo metadata failed: ".to_string()));
    assert!(
        missing_result
            .expect_err("missing cargo")
            .starts_with("Failed to run cargo metadata:")
    );
}

#[test]
fn metadata_parsing_keeps_outside_paths_and_rejects_unreadable_inputs() {
    // Arrange
    let directory = tempdir().expect("workspace");
    let manifest_path = directory.path().join("Cargo.toml");
    fs::write(&manifest_path, "[package]\n").expect("manifest");
    let metadata = |path: &Path| {
        serde_json::json!({
            "packages": [{
                "dependencies": [{"features": ["time"], "name": "tokio"}],
                "manifest_path": path,
            }],
            "workspace_root": "/elsewhere",
        })
        .to_string()
    };
    let missing_path = directory.path().join("missing/Cargo.toml");

    // Act
    let packages = RealWorkspace::parse_packages(metadata(&manifest_path).as_bytes());
    let missing = RealWorkspace::parse_packages(metadata(&missing_path).as_bytes());
    let invalid = RealWorkspace::parse_packages(b"{}");

    // Assert
    assert_eq!(
        packages,
        Ok(vec![Package {
            dependencies: vec![dependency("tokio", &["time"])],
            manifest: "[package]\n".to_string(),
            manifest_path,
        }])
    );
    assert!(
        missing
            .expect_err("missing manifest")
            .starts_with(&format!("Failed to read {}:", missing_path.display()))
    );
    assert!(
        invalid
            .expect_err("invalid metadata")
            .starts_with("Failed to parse cargo metadata:")
    );
}

#[test]
fn production_composition_passes_on_this_workspace() {
    // Act
    let result = run();

    // Assert
    assert_eq!(result, Ok(()));
}
