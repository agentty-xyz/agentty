//! Workspace metadata loading unit tests.

use std::ffi::OsString;
use std::path::PathBuf;

use semver::Version;
use serde_json::{Value, json};

use crate::process::{MockProcessRunner, ProcessCommand};
use crate::repository::GitHubRepository;
use crate::workspace::test_support::{DEMO_ROOT, demo_metadata, metadata_bytes};
use crate::workspace::{App, Workspace};

/// Returns the demo metadata with `value` set at the JSON `pointer`, whose
/// parent must be an object.
fn demo_metadata_with(pointer: &str, value: Value) -> Vec<u8> {
    let mut metadata = demo_metadata();
    let (parent, key) = pointer.rsplit_once('/').expect("fixture pointer");
    metadata
        .pointer_mut(parent)
        .and_then(Value::as_object_mut)
        .expect("fixture parent object")
        .insert(key.to_string(), value);

    metadata_bytes(&metadata)
}

fn demo_repository() -> GitHubRepository {
    GitHubRepository::parse("https://github.com/demo-org/demo-app").expect("repository")
}

#[test]
fn load_runs_one_cargo_metadata_call_and_resolves_apps() {
    // Arrange
    let mut process = MockProcessRunner::new();
    process
        .expect_output()
        .withf(|command| {
            *command
                == ProcessCommand {
                    args: ["metadata", "--format-version", "1", "--no-deps"]
                        .map(OsString::from)
                        .to_vec(),
                    current_dir: None,
                    program: OsString::from("demo-cargo"),
                }
        })
        .once()
        .return_once(|_| Ok(metadata_bytes(&demo_metadata())));

    // Act
    let workspace = Workspace::load(&process, OsString::from("demo-cargo")).expect("workspace");

    // Assert
    let version = Version::new(1, 2, 3);
    let app = |name: &str, binaries: &[&str], npm_scope: Option<&str>| App {
        authors: vec!["Demo Author <demo@example.com>".to_string()],
        binaries: binaries.iter().map(ToString::to_string).collect(),
        description: Some("Demo package".to_string()),
        license: Some("Apache-2.0".to_string()),
        name: name.to_string(),
        npm_scope: npm_scope.map(ToString::to_string),
        package_directory: PathBuf::from(DEMO_ROOT).join("crates").join(name),
        repository: demo_repository(),
        version: version.clone(),
    };
    assert_eq!(
        workspace.apps,
        vec![
            app("demo-app", &["demo-app", "demo-helper"], None),
            app("demo-tool", &["demo-tool"], Some("@demo-org")),
        ]
    );
    assert_eq!(workspace.config.profile, "dist");
    assert_eq!(workspace.repository, demo_repository());
    assert_eq!(workspace.root, PathBuf::from(DEMO_ROOT));
    assert_eq!(
        workspace.target_directory,
        PathBuf::from(DEMO_ROOT).join("target")
    );
    assert_eq!(workspace.version, version);
}

#[test]
fn load_reports_cargo_metadata_failures() {
    // Arrange
    let mut process = MockProcessRunner::new();
    process
        .expect_output()
        .once()
        .return_once(|_| Err("`cargo metadata` failed".to_string()));

    // Act
    let workspace = Workspace::load(&process, OsString::from("cargo"));

    // Assert
    assert_eq!(workspace, Err("`cargo metadata` failed".to_string()));
}

#[test]
fn from_metadata_rejects_invalid_apps() {
    // Arrange
    let cases = [
        (
            demo_metadata_with("/metadata/release/apps/demo-missing", json!({})),
            "`workspace.metadata.release.apps.demo-missing` is not a workspace package",
        ),
        (
            demo_metadata_with("/metadata/release/apps/demo-lib", json!({})),
            "app `demo-lib` has no binary targets",
        ),
        (
            demo_metadata_with("/packages/2/repository", Value::Null),
            "app `demo-tool` must set `repository` to its GitHub URL in `Cargo.toml`",
        ),
        (
            demo_metadata_with("/packages/2/repository", json!("https://example.com/demo")),
            "app `demo-tool`: repository `https://example.com/demo` must be a \
             `https://github.com/<owner>/<name>` URL",
        ),
        (
            demo_metadata_with("/packages/2/version", json!("1.2")),
            "app `demo-tool` has an invalid version: unexpected end of input while parsing \
             minor version number",
        ),
        (
            demo_metadata_with("/packages/2/manifest_path", json!("/")),
            "app `demo-tool` has manifest path `/` without a parent directory",
        ),
        (
            demo_metadata_with("/packages/2/version", json!("1.3.0")),
            "apps release in lockstep, but `demo-app` has version 1.2.3 and `demo-tool` has \
             version 1.3.0",
        ),
        (
            demo_metadata_with(
                "/packages/2/repository",
                json!("https://github.com/demo-org/demo-tool"),
            ),
            "apps must share one repository, but `demo-app` names \
             https://github.com/demo-org/demo-app and `demo-tool` names \
             https://github.com/demo-org/demo-tool",
        ),
        (
            demo_metadata_with("/metadata", Value::Null),
            "missing `[workspace.metadata.release]` in the workspace `Cargo.toml`",
        ),
    ];

    for (metadata, expected) in cases {
        // Act
        let workspace = Workspace::from_metadata(&metadata);

        // Assert
        assert_eq!(workspace, Err(expected.to_string()));
    }
}

#[test]
fn from_metadata_rejects_output_that_is_not_cargo_metadata() {
    // Arrange
    let output = b"{\"packages\": []}";

    // Act
    let workspace = Workspace::from_metadata(output);

    // Assert
    assert!(
        workspace
            .expect_err("invalid metadata")
            .starts_with("failed to parse `cargo metadata` output: missing field")
    );
}
