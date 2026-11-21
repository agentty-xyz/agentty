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
        homebrew_description: None,
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
    assert_eq!(workspace.tool_directory, None);
    assert_eq!(workspace.version, version);
}

#[test]
fn from_metadata_finds_the_tool_package_directory() {
    // Arrange
    let metadata = demo_metadata_with("/packages/0/name", json!(env!("CARGO_PKG_NAME")));

    // Act
    let workspace = Workspace::from_metadata(&metadata).expect("workspace");

    // Assert
    assert_eq!(
        workspace.tool_directory,
        Some(PathBuf::from(DEMO_ROOT).join("crates/demo-lib"))
    );
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
fn binstall_installer_requires_metadata_naming_the_release_archives() {
    // Arrange
    let binstall = |pkg_url: &str| {
        json!({"binstall": {
            "bin-dir": "{ name }-{ target }/{ bin }{ binary-ext }",
            "pkg-fmt": "tgz",
            "pkg-url": pkg_url,
        }})
    };
    let prefixed = binstall("{ repo }/releases/download/v{ version }/{ name }-{ target }.tar.gz");
    let unprefixed = binstall("{ repo }/releases/download/{ version }/{ name }-{ target }.tar.gz");
    let metadata = |demo_app: &Value, demo_tool: &Value| {
        let mut metadata = demo_metadata();
        metadata["metadata"]["release"]["installers"] = json!(["binstall"]);
        metadata["packages"][1]["metadata"] = demo_app.clone();
        metadata["packages"][2]["metadata"] = demo_tool.clone();

        metadata_bytes(&metadata)
    };
    let mut zip = prefixed.clone();
    zip["binstall"]["pkg-fmt"] = json!("zip");
    let expected = "the `binstall` installer needs these keys in \
                    `/workspace/demo/crates/demo-tool/Cargo.toml`, with `v` removed from \
                    `pkg-url` for release tags without \
                    it:\n\n[package.metadata.binstall]\npkg-url = \"{ repo }/releases/download/v{ \
                    version }/{ name }-{ target }.tar.gz\"\nbin-dir = \"{ name }-{ target }/{ bin \
                    }{ binary-ext }\"\npkg-fmt = \"tgz\"";

    // Act
    let accepted = Workspace::from_metadata(&metadata(&prefixed, &unprefixed));
    let rejected = [
        Value::Null,
        json!({"other": {}}),
        zip,
        binstall("{ repo }/demo.tgz"),
    ]
    .map(|demo_tool| Workspace::from_metadata(&metadata(&prefixed, &demo_tool)));

    // Assert
    assert!(accepted.is_ok(), "{accepted:?}");
    for workspace in rejected {
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
