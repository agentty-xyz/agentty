//! Release plan unit tests.

use serde_json::{Value, json};

use crate::plan::ReleasePlan;
use crate::tag::ReleaseTag;
use crate::workspace::Workspace;
use crate::workspace::test_support::{demo_metadata, metadata_bytes};

fn workspace(metadata: &Value) -> Workspace {
    Workspace::from_metadata(&metadata_bytes(metadata)).expect("demo workspace")
}

fn tag(name: &str) -> ReleaseTag {
    ReleaseTag::parse(name).expect("release tag")
}

/// Returns the serialized plan of the demo workspace for `tag`.
fn plan_json(metadata: &Value, tag: Option<&ReleaseTag>) -> Value {
    let plan = ReleasePlan::new(&workspace(metadata), tag).expect("release plan");

    serde_json::to_value(plan).expect("serialize plan")
}

#[test]
fn plan_serializes_the_documented_contract() {
    // Arrange
    let metadata = demo_metadata();
    let tag = tag("v1.2.3");

    // Act
    let plan = plan_json(&metadata, Some(&tag));

    // Assert
    let archive = |app: &str, target: &str| {
        json!({
            "checksum": format!("{app}-{target}.tar.gz.sha256"),
            "directory": format!("{app}-{target}"),
            "name": format!("{app}-{target}.tar.gz"),
            "target": target,
        })
    };
    let app = |name: &str, binaries: &[&str], npm_package: &str| {
        json!({
            "archives": [
                archive(name, "aarch64-apple-darwin"),
                archive(name, "x86_64-unknown-linux-gnu"),
            ],
            "authors": ["Demo Author <demo@example.com>"],
            "binaries": binaries,
            "description": "Demo package",
            "license": "Apache-2.0",
            "name": name,
            "npm_package": npm_package,
            "npm_tarball": format!("{name}-npm-package.tar.gz"),
            "shell_installer": format!("{name}-installer.sh"),
        })
    };
    assert_eq!(
        plan,
        json!({
            "apps": [
                app("demo-app", &["demo-app", "demo-helper"], "demo-app"),
                app("demo-tool", &["demo-tool"], "@demo-org/demo-tool"),
            ],
            "artifacts": {
                "checksums": "sha256.sum",
                "manifest": "release-manifest.json",
                "source_archive": "source.tar.gz",
                "source_checksum": "source.tar.gz.sha256",
            },
            "include": ["README.md", "LICENSE"],
            "installers": ["shell", "npm"],
            "matrix": {
                "include": [
                    {"runner": "macos-latest", "target": "aarch64-apple-darwin"},
                    {"runner": "ubuntu-22.04", "target": "x86_64-unknown-linux-gnu"},
                ],
            },
            "min_glibc": "2.35",
            "prerelease": false,
            "profile": "dist",
            "repository": {
                "name": "demo-app",
                "owner": "demo-org",
                "url": "https://github.com/demo-org/demo-app",
            },
            "tag": "v1.2.3",
            "version": "1.2.3",
        })
    );
}

#[test]
fn plan_without_tag_or_installers_omits_installer_artifacts() {
    // Arrange
    let mut metadata = demo_metadata();
    let release = metadata
        .pointer_mut("/metadata/release")
        .and_then(Value::as_object_mut)
        .expect("release table");
    release.remove("installers");
    release.remove("min-glibc");

    // Act
    let plan = plan_json(&metadata, None);

    // Assert
    assert_eq!(plan["tag"], Value::Null);
    assert_eq!(plan["min_glibc"], Value::Null);
    assert_eq!(plan["installers"], json!([]));
    for app in plan["apps"].as_array().expect("apps") {
        assert_eq!(app["npm_package"], Value::Null);
        assert_eq!(app["npm_tarball"], Value::Null);
        assert_eq!(app["shell_installer"], Value::Null);
    }
}

#[test]
fn plan_marks_semver_prereleases() {
    // Arrange
    let mut metadata = demo_metadata();
    for index in 0..3 {
        metadata["packages"][index]["version"] = json!("2.0.0-rc.1");
    }
    let tag = tag("2.0.0-rc.1");

    // Act
    let plan = plan_json(&metadata, Some(&tag));

    // Assert
    assert_eq!(plan["prerelease"], json!(true));
    assert_eq!(plan["tag"], json!("2.0.0-rc.1"));
    assert_eq!(plan["version"], json!("2.0.0-rc.1"));
}

#[test]
fn plan_rejects_a_tag_for_another_version() {
    // Arrange
    let workspace = workspace(&demo_metadata());
    let tag = tag("v1.2.4");

    // Act
    let plan = ReleasePlan::new(&workspace, Some(&tag));

    // Assert
    assert_eq!(
        plan,
        Err(
            "release tag `v1.2.4` names version 1.2.4, but the apps have version 1.2.3".to_string()
        )
    );
}
