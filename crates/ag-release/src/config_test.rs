//! Release configuration validation unit tests.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{Value, json};

use crate::config::{AppConfig, GlibcVersion, Installer, ReleaseConfig};
use crate::target::Target;
use crate::workflow_config::WorkflowConfig;

/// Placeholder for `workspace.metadata.release` in expected error messages.
const RELEASE: &str = "$";

/// One invalid configuration: the `release.<pointer>` value to set (or
/// remove with `None`) and the expected error with [`RELEASE`] placeholders.
type RejectedCase<'case> = (&'case str, Option<Value>, &'case str);

/// Workspace metadata with every release key set.
fn full_metadata() -> Value {
    json!({
        "release": {
            "apps": {
                "demo-app": {},
                "demo-tool": {"npm-scope": "@demo-org"},
            },
            "include": ["README.md", "docs/LICENSE"],
            "installers": ["npm", "shell"],
            "min-glibc": "2.35",
            "profile": "dist",
            "targets": {
                "x86_64-unknown-linux-gnu": "ubuntu-22.04",
                "aarch64-apple-darwin": "macos-latest",
            },
            "workflow": {"node-version": "24.x"},
        },
        "other-tool": {"ignored": true},
    })
}

/// Returns [`full_metadata`] with `release.<pointer>` replaced or removed.
fn metadata_with(pointer: &str, value: Option<Value>) -> Value {
    let mut metadata = full_metadata();
    let (parent, key) = pointer.rsplit_once('/').expect("pointer with a parent");
    let parent = metadata
        .pointer_mut(&format!("/release{parent}"))
        .and_then(Value::as_object_mut)
        .expect("parent table");
    match value {
        Some(value) => parent.insert(key.to_string(), value),
        None => parent.remove(key),
    };

    metadata
}

/// Asserts that each case fails to parse with its expected error.
fn assert_rejected(cases: Vec<RejectedCase<'_>>) {
    for (pointer, value, expected) in cases {
        let metadata = metadata_with(pointer, value);

        let config = ReleaseConfig::parse(Some(&metadata));

        assert_eq!(
            config,
            Err(expected.replace(RELEASE, "workspace.metadata.release")),
            "{pointer}"
        );
    }
}

#[test]
fn parse_reads_every_release_key() {
    // Arrange
    let metadata = full_metadata();

    // Act
    let config = ReleaseConfig::parse(Some(&metadata));

    // Assert
    assert_eq!(
        config,
        Ok(ReleaseConfig {
            apps: BTreeMap::from([
                ("demo-app".to_string(), AppConfig::default()),
                (
                    "demo-tool".to_string(),
                    AppConfig {
                        npm_scope: Some("@demo-org".to_string()),
                    },
                ),
            ]),
            include: vec!["README.md".to_string(), "docs/LICENSE".to_string()],
            installers: BTreeSet::from([Installer::Shell, Installer::Npm]),
            min_glibc: Some(GlibcVersion {
                major: 2,
                minor: 35,
            }),
            profile: "dist".to_string(),
            targets: BTreeMap::from([
                (Target::Aarch64AppleDarwin, "macos-latest".to_string()),
                (Target::X8664UnknownLinuxGnu, "ubuntu-22.04".to_string()),
            ]),
            workflow: WorkflowConfig {
                node_version: Some("24.x".to_string()),
                ..WorkflowConfig::default()
            },
        })
    );
}

#[test]
fn parse_applies_defaults_for_optional_keys() {
    // Arrange
    let metadata = json!({
        "release": {
            "apps": {"demo-app": {}},
            "targets": {"x86_64-unknown-linux-gnu": "ubuntu-22.04"},
        },
    });

    // Act
    let config = ReleaseConfig::parse(Some(&metadata)).expect("minimal configuration");

    // Assert
    assert_eq!(config.include, Vec::<String>::new());
    assert_eq!(config.installers, BTreeSet::new());
    assert_eq!(config.min_glibc, None);
    assert_eq!(config.profile, "release");
    assert_eq!(config.workflow, WorkflowConfig::default());
}

#[test]
fn parse_rejects_unknown_keys_and_invalid_tables() {
    // Arrange
    let scope_error = "`$.apps.demo-tool.npm-scope` must be an npm scope such as `@example`";
    let cases: Vec<RejectedCase<'_>> = vec![
        (
            "/unknown",
            Some(json!(1)),
            "unknown key `$.unknown`; expected one of: apps, include, installers, min-glibc, \
             profile, targets, workflow",
        ),
        (
            "/workflow",
            Some(json!("generated")),
            "`$.workflow` must be a table",
        ),
        (
            "/workflow/unknown",
            Some(json!(true)),
            "unknown key `$.workflow.unknown`; expected one of: distributions, jobs, \
             node-version, smoke-paths, tool",
        ),
        (
            "/apps/demo-app/npm_scope",
            Some(json!("@demo-org")),
            "unknown key `$.apps.demo-app.npm_scope`; expected one of: npm-scope",
        ),
        ("/apps", None, "missing `[$.apps]`; name at least one app"),
        (
            "/apps",
            Some(json!({})),
            "`[$.apps]` must name at least one app",
        ),
        (
            "/apps",
            Some(json!(["demo-app"])),
            "`$.apps` must be a table",
        ),
        (
            "/apps/demo-app",
            Some(json!("demo-app")),
            "`$.apps.demo-app` must be a table",
        ),
        (
            "/targets",
            Some(json!({})),
            "`[$.targets]` must name at least one target",
        ),
        (
            "/targets/x86_64-pc-windows-msvc",
            Some(json!("windows-latest")),
            "`$.targets.x86_64-pc-windows-msvc` is not a supported target; supported targets: \
             aarch64-apple-darwin, aarch64-unknown-linux-gnu, x86_64-apple-darwin, \
             x86_64-unknown-linux-gnu",
        ),
        (
            "/targets/aarch64-apple-darwin",
            Some(json!("")),
            "`$.targets.aarch64-apple-darwin` must be a nonempty runner label string",
        ),
        (
            "/targets/aarch64-apple-darwin",
            Some(json!(["macos-latest"])),
            "`$.targets.aarch64-apple-darwin` must be a nonempty runner label string",
        ),
    ];
    let scope_cases = ["demo-org", "@Demo", "@-demo"]
        .map(|scope| (json!(scope), format!("{scope_error}, found `{scope}`")));

    // Act & Assert
    assert_rejected(cases);
    let scope_cases = scope_cases
        .iter()
        .map(|(scope, expected)| {
            (
                "/apps/demo-tool/npm-scope",
                Some(scope.clone()),
                expected.as_str(),
            )
        })
        .collect();
    assert_rejected(scope_cases);
}

#[test]
fn parse_rejects_invalid_values_naming_the_key() {
    // Arrange
    let glibc_error = "`$.min-glibc` must be a `MAJOR.MINOR` version such as `2.35`, found";
    let cases: Vec<RejectedCase<'_>> = vec![
        (
            "/apps/demo-tool/npm-scope",
            Some(json!("")),
            "`$.apps.demo-tool.npm-scope` must be a nonempty string",
        ),
        (
            "/include",
            Some(json!("README.md")),
            "`$.include` must be a list of nonempty strings",
        ),
        (
            "/include",
            Some(json!(["README.md", 1])),
            "`$.include` must be a list of nonempty strings",
        ),
        (
            "/include",
            Some(json!(["README.md", "README.md"])),
            "`$.include` lists `README.md` more than once",
        ),
        (
            "/include",
            Some(json!(["../README.md"])),
            "`$.include` entry `../README.md` must be a relative path inside the workspace root",
        ),
        (
            "/include",
            Some(json!(["/etc/passwd"])),
            "`$.include` entry `/etc/passwd` must be a relative path inside the workspace root",
        ),
        (
            "/installers",
            Some(json!(["shell", "msi"])),
            "`$.installers` entry `msi` is not a supported installer; supported installers: \
             shell, npm",
        ),
        (
            "/min-glibc",
            Some(json!(2.35)),
            "`$.min-glibc` must be a nonempty string",
        ),
        (
            "/profile",
            Some(json!("my profile")),
            "`$.profile` must be a Cargo profile name, found `my profile`",
        ),
    ];
    let glibc_cases = ["2", "2.35.1", "+2.35", "2."]
        .map(|version| (json!(version), format!("{glibc_error} `{version}`")));

    // Act & Assert
    assert_rejected(cases);
    let glibc_cases = glibc_cases
        .iter()
        .map(|(version, expected)| ("/min-glibc", Some(version.clone()), expected.as_str()))
        .collect();
    assert_rejected(glibc_cases);
}

#[test]
fn parse_requires_the_release_table() {
    // Arrange
    let other_tool = json!({"other-tool": {}});
    let not_a_table = json!({"release": "dist"});

    // Act
    let absent = ReleaseConfig::parse(None);
    let missing = ReleaseConfig::parse(Some(&other_tool));
    let invalid = ReleaseConfig::parse(Some(&not_a_table));

    // Assert
    let expected =
        Err("missing `[workspace.metadata.release]` in the workspace `Cargo.toml`".to_string());
    assert_eq!(absent, expected);
    assert_eq!(missing, expected);
    assert_eq!(
        invalid,
        Err("`workspace.metadata.release` must be a table".to_string())
    );
}

#[test]
fn select_targets_keeps_the_named_configured_targets() {
    // Arrange
    let metadata = full_metadata();
    let config = ReleaseConfig::parse(Some(&metadata)).expect("full configuration");
    let mut every = config.clone();
    let mut selected = config.clone();
    let mut unconfigured = config.clone();
    let linux = "x86_64-unknown-linux-gnu".to_string();

    // Act
    let every_result = every.select_targets(&[]);
    let selected_result = selected.select_targets(&[linux.clone(), linux]);
    let unconfigured_result = unconfigured.select_targets(&["x86_64-apple-darwin".to_string()]);

    // Assert
    assert_eq!((every_result, every), (Ok(()), config.clone()));
    assert_eq!(selected_result, Ok(()));
    assert_eq!(
        selected.targets,
        BTreeMap::from([(Target::X8664UnknownLinuxGnu, "ubuntu-22.04".to_string())])
    );
    assert_eq!(
        unconfigured_result,
        Err("target `x86_64-apple-darwin` is not configured in \
             `workspace.metadata.release.targets`; configured targets: aarch64-apple-darwin, \
             x86_64-unknown-linux-gnu"
            .to_string())
    );
    assert_eq!(unconfigured, config);
}

#[test]
fn release_values_serialize_as_configuration_spellings() {
    // Arrange
    let values = (
        Installer::ALL,
        GlibcVersion {
            major: 2,
            minor: 17,
        },
    );

    // Act
    let serialized = serde_json::to_value(values).expect("serialize values");

    // Assert
    assert_eq!(serialized, json!([["shell", "npm"], "2.17"]));
}
