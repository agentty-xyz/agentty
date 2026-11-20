//! Shell installer rendering unit tests. Running the rendered script is
//! covered by the command-line tests in `tests/cli/shell_installer.rs`.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::plan::ReleasePlan;
use crate::shell_installer::ShellInstaller;
use crate::workspace::Workspace;
use crate::workspace::test_support::{demo_metadata, metadata_bytes};

/// Returns the demo release plan for `metadata`.
fn plan(metadata: &Value) -> ReleasePlan {
    let workspace = Workspace::from_metadata(&metadata_bytes(metadata)).expect("demo workspace");

    ReleasePlan::new(&workspace, None).expect("release plan")
}

/// Returns a stand-in digest for every archive of `plan`: its target triple
/// written twice.
fn digests(plan: &ReleasePlan) -> BTreeMap<String, String> {
    plan.apps
        .iter()
        .flat_map(|app| &app.archives)
        .map(|archive| (archive.name.clone(), archive.target.triple().repeat(2)))
        .collect()
}

#[test]
fn render_bakes_in_every_release_value() {
    // Arrange
    let plan = plan(&demo_metadata());
    let sha256 = digests(&plan);
    let app = &plan.apps[0];

    // Act
    let script = ShellInstaller::render(&plan, app, "v1.2.3", &sha256).expect("render");
    let again = ShellInstaller::render(&plan, app, "v1.2.3", &sha256).expect("render");

    // Assert
    assert_eq!(script, again);
    assert!(script.starts_with(
        "#!/bin/sh\n# Installs demo-app 1.2.3 from https://github.com/demo-org/demo-app.\n"
    ));
    assert!(script.ends_with("\nmain \"$@\"\n"), "{script}");
    let version = env!("CARGO_PKG_VERSION");
    for expected in [
        "APP_NAME='demo-app'\nAPP_VERSION='1.2.3'\n".to_string(),
        "DEFAULT_DOWNLOAD_URL='https://github.com/demo-org/demo-app/releases/download/v1.2.3'\n"
            .to_string(),
        "SUPPORTED_TARGETS='aarch64-apple-darwin, x86_64-unknown-linux-gnu'\n".to_string(),
        "MIN_GLIBC=2.35\nMIN_GLIBC_MAJOR=2\nMIN_GLIBC_MINOR=35\nRECEIPT_BINARIES=".to_string(),
        "RECEIPT_BINARIES='[\"demo-app\",\"demo-helper\"]'\n".to_string(),
        format!("RECEIPT_PROVIDER='{{\"source\":\"ag-release\",\"version\":\"{version}\"}}'\n"),
        "RECEIPT_SOURCE='{\"app_name\":\"demo-app\",\"name\":\"demo-app\",\"owner\":\"demo-org\",\
         \"release_type\":\"github\"}'\n"
            .to_string(),
        "RECEIPT_VERSION='\"1.2.3\"'\n".to_string(),
        "_download_base=${DEMO_APP_DOWNLOAD_URL:-$DEFAULT_DOWNLOAD_URL}\n".to_string(),
        "_no_modify_path=${DEMO_APP_NO_MODIFY_PATH:-0}\n".to_string(),
        "            case \"$_system $_candidate $_libc\" in\n                \
         'Darwin arm64 none' | 'Darwin aarch64 none')\n                    \
         _target=aarch64-apple-darwin\n                    \
         _archive='demo-app-aarch64-apple-darwin.tar.gz'\n                    \
         _directory='demo-app-aarch64-apple-darwin'\n                    \
         _sha256='aarch64-apple-darwinaarch64-apple-darwin'\n                    \
         ;;\n                \
         'Linux x86_64 gnu' | 'Linux amd64 gnu')\n                    \
         _target=x86_64-unknown-linux-gnu\n"
            .to_string(),
        "_sha256='x86_64-unknown-linux-gnux86_64-unknown-linux-gnu'\n                    \
         ;;\n                *)\n"
            .to_string(),
        "for _binary in 'demo-app' 'demo-helper'; do\n".to_string(),
        "\nUsage: sh demo-app-installer.sh [--help]\n".to_string(),
        "set DEMO_APP_NO_MODIFY_PATH=1 instead\"\n".to_string(),
    ] {
        assert!(script.contains(&expected), "missing {expected:?} in:\n{script}");
    }
}

#[test]
fn render_leaves_the_glibc_minimum_empty_when_unset() {
    // Arrange
    let mut metadata = demo_metadata();
    metadata["metadata"]["release"]
        .as_object_mut()
        .expect("release table")
        .remove("min-glibc");
    let plan = plan(&metadata);

    // Act
    let script =
        ShellInstaller::render(&plan, &plan.apps[1], "1.2.3", &digests(&plan)).expect("render");

    // Assert
    assert!(script.contains("\nMIN_GLIBC=\nMIN_GLIBC_MAJOR=\nMIN_GLIBC_MINOR=\n"));
    assert!(script.contains("APP_NAME='demo-tool'\n"));
    assert!(script.contains("/releases/download/1.2.3'\n"));
    assert!(script.contains("_unmanaged_dir=${DEMO_TOOL_UNMANAGED_INSTALL:-}\n"));
}

#[test]
fn render_requires_every_archive_digest() {
    // Arrange
    let plan = plan(&demo_metadata());
    let mut sha256 = digests(&plan);
    sha256.remove("demo-app-x86_64-unknown-linux-gnu.tar.gz");

    // Act
    let result = ShellInstaller::render(&plan, &plan.apps[0], "v1.2.3", &sha256);

    // Assert
    assert_eq!(
        result,
        Err("no SHA-256 is recorded for `demo-app-x86_64-unknown-linux-gnu.tar.gz`".to_string())
    );
}

#[test]
fn quote_makes_one_shell_word() {
    // Arrange
    let values = [
        ("", "''"),
        ("demo app", "'demo app'"),
        ("it's", r"'it'\''s'"),
    ];

    for (value, expected) in values {
        // Act
        let quoted = ShellInstaller::quote(value);

        // Assert
        assert_eq!(quoted, expected);
    }
}
