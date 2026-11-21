//! `prepare` unit tests with mocked host boundaries.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::{VersionBump, check_date, run};
use crate::cli::{Host, PrepareOptions};
use crate::process::{MockProcessRunner, ProcessCommand};
use crate::workspace::test_support::{
    MemoryFiles, demo_metadata, lock, memory_file_system, memory_files, metadata_process,
};

/// Root manifest of the demo workspace at version 1.2.3.
const ROOT_MANIFEST: &str = r#"[workspace]
members = ["crates/*"]

[workspace.package]
# The release version.
version = "1.2.3"

[workspace.dependencies]
demo-lib = { path = "crates/demo-lib", version = "1.2.3" }
demo-tool = { path = "crates/demo-tool", version = "=1.2.3", default-features = false }
renamed = { package = "demo-app", path = "crates/demo-app", version = "^ 1.2.3" }
loose = { package = "demo-app", path = "crates/demo-app", version = "1.2" }
external = { version = "1.2.3" }
other = { path = "vendor/other", version = "1.2.3" }
plain = "1.2.3"
"#;

/// Member manifest with its own version and dependency tables.
const MEMBER_MANIFEST: &str = r#"[package]
name = "demo-app"
version = '1.2.3'

[dependencies]
demo-lib = { path = "../demo-lib", version = "~1.2.3" }

[target.'cfg(unix)'.dev-dependencies.demo-tool]
path = "../demo-tool"
version = "1.2.3"

[target.'cfg(unix)'.build-dependencies]
demo-lib = { workspace = true }
"#;

/// Changelog of the demo workspace with unreleased notes.
const CHANGELOG: &str = "# Changelog\n\n## [Unreleased]\n\n- Demo.\n\n## [v1.2.3] - 2026-01-01\n";

/// Returns the bump from 1.2.3 to 1.3.0 of every demo package.
fn bump() -> VersionBump {
    VersionBump {
        current: "1.2.3".to_string(),
        new: "1.3.0".to_string(),
        packages: ["demo-app", "demo-lib", "demo-tool"]
            .map(String::from)
            .into(),
    }
}

/// Returns the demo workspace's files: the root and member manifests and,
/// when `changelog` is given, `CHANGELOG.md`.
fn demo_files(changelog: Option<&str>) -> MemoryFiles {
    let member = MEMBER_MANIFEST.as_bytes();
    let mut files = vec![
        ("/workspace/demo/Cargo.toml", ROOT_MANIFEST.as_bytes()),
        (
            "/workspace/demo/crates/demo-lib/Cargo.toml",
            b"[package]\nname = \"demo-lib\"\n",
        ),
        ("/workspace/demo/crates/demo-app/Cargo.toml", member),
        (
            "/workspace/demo/crates/demo-tool/Cargo.toml",
            b"[package]\nversion.workspace = true\n",
        ),
    ];
    if let Some(changelog) = changelog {
        files.push(("/workspace/demo/CHANGELOG.md", changelog.as_bytes()));
    }

    memory_files(&files)
}

/// Returns a process runner for the demo workspace whose
/// `cargo update --workspace` answers `result`.
fn update_process(result: Result<(), String>) -> MockProcessRunner {
    let mut process = metadata_process(&demo_metadata());
    let expected = ProcessCommand {
        args: ["update", "--workspace"].map(OsString::from).to_vec(),
        current_dir: Some(PathBuf::from("/workspace/demo")),
        program: OsString::from("cargo"),
    };
    process
        .expect_stream()
        .withf(move |command| *command == expected)
        .once()
        .return_once(move |_| result);

    process
}

/// Runs `prepare` and returns its result with the captured output.
fn run_prepare(
    version: &str,
    date: &str,
    process: &MockProcessRunner,
    files: &MemoryFiles,
) -> (Result<(), String>, String) {
    let file_system = memory_file_system(files);
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system: &file_system,
        process,
        source_date_epoch: None,
    };
    let options = PrepareOptions {
        date: date.to_string(),
        version: version.to_string(),
    };
    let mut stdout = Vec::new();
    let result = run(&host, &options, &mut stdout);

    (result, String::from_utf8(stdout).expect("UTF-8 output"))
}

/// Returns the text of the file at `path` in `files`.
fn text(files: &MemoryFiles, path: &str) -> String {
    String::from_utf8(lock(files)[Path::new(path)].clone()).expect("UTF-8 file")
}

#[test]
fn edit_bumps_versions_and_path_requirements_in_place() {
    // Arrange
    let bump = bump();

    // Act
    let root = bump.edit(ROOT_MANIFEST, Path::new("Cargo.toml"));
    let member = bump.edit(MEMBER_MANIFEST, Path::new("member/Cargo.toml"));

    // Assert
    let expected_root = ROOT_MANIFEST
        .replace(
            "# The release version.\nversion = \"1.2.3\"",
            "# The release version.\nversion = \"1.3.0\"",
        )
        .replace(
            "\"crates/demo-lib\", version = \"1.2.3\"",
            "\"crates/demo-lib\", version = \"1.3.0\"",
        )
        .replace("\"=1.2.3\"", "\"=1.3.0\"")
        .replace("\"^ 1.2.3\"", "\"^1.3.0\"");
    assert_eq!(root, Ok(expected_root));
    let expected_member = MEMBER_MANIFEST
        .replace("version = '1.2.3'", "version = \"1.3.0\"")
        .replace("\"~1.2.3\"", "\"~1.3.0\"")
        .replace(
            "\"../demo-tool\"\nversion = \"1.2.3\"",
            "\"../demo-tool\"\nversion = \"1.3.0\"",
        );
    assert_eq!(member, Ok(expected_member));
}

#[test]
fn edit_leaves_other_packages_and_inherited_versions_alone() {
    // Arrange
    let bump = VersionBump {
        packages: BTreeSet::from(["demo-unrelated".to_string()]),
        ..bump()
    };
    let inherited =
        "[package]\nversion.workspace = true\n\n[workspace.package]\nversion = \"1.0.0\"\n";

    // Act
    let member = bump.edit(MEMBER_MANIFEST, Path::new("Cargo.toml"));
    let unchanged = bump.edit(inherited, Path::new("Cargo.toml"));

    // Assert
    assert_eq!(
        member,
        Ok(MEMBER_MANIFEST.replace("version = '1.2.3'", "version = \"1.3.0\""))
    );
    assert_eq!(unchanged, Ok(inherited.to_string()));
}

#[test]
fn edit_ignores_values_where_tables_are_expected() {
    // Arrange
    let bump = bump();
    let manifest = "workspace = 1\ndependencies = \"demo-lib\"\n\n[target]\nlinux = true\n";

    // Act
    let edited = bump.edit(manifest, Path::new("Cargo.toml"));

    // Assert
    assert_eq!(edited, Ok(manifest.to_string()));
}

#[test]
fn edit_rejects_a_manifest_that_is_not_toml() {
    // Arrange
    let bump = bump();

    // Act
    let edited = bump.edit("[package\n", Path::new("/demo/Cargo.toml"));

    // Assert
    let error = edited.expect_err("invalid TOML");
    assert!(
        error.starts_with("failed to parse `/demo/Cargo.toml`: "),
        "{error}"
    );
}

#[test]
fn run_bumps_manifests_refreshes_the_lock_file_and_adds_the_changelog_heading() {
    // Arrange
    let process = update_process(Ok(()));
    let files = demo_files(Some(CHANGELOG));

    // Act
    let (result, stdout) = run_prepare("v1.3.0", "2026-02-03", &process, &files);

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(
        stdout,
        "/workspace/demo/Cargo.toml\n/workspace/demo/crates/demo-app/Cargo.toml\n/workspace/demo/\
         Cargo.lock\n/workspace/demo/CHANGELOG.md\n"
    );
    assert!(text(&files, "/workspace/demo/Cargo.toml").contains("\nversion = \"1.3.0\"\n"));
    assert!(text(&files, "/workspace/demo/crates/demo-app/Cargo.toml").contains("\"~1.3.0\""));
    assert_eq!(
        text(&files, "/workspace/demo/CHANGELOG.md"),
        "# Changelog\n\n## [Unreleased]\n\n## [v1.3.0] - 2026-02-03\n\n- Demo.\n\n## [v1.2.3] - \
         2026-01-01\n"
    );
}

#[test]
fn run_edits_a_root_package_manifest_once() {
    // Arrange
    let mut metadata = demo_metadata();
    metadata["packages"][1]["manifest_path"] = json!("/workspace/demo/Cargo.toml");
    let mut process = metadata_process(&metadata);
    process.expect_stream().once().return_once(|_| Ok(()));
    let files = demo_files(None);

    // Act
    let (result, stdout) = run_prepare("1.3.0", "2026-02-03", &process, &files);

    // Assert
    assert_eq!(result, Ok(()));
    assert_eq!(
        stdout,
        "/workspace/demo/Cargo.toml\n/workspace/demo/Cargo.lock\n"
    );
}

#[test]
fn run_without_a_changelog_or_with_its_entry_writes_only_the_manifests() {
    // Arrange
    let with_entry = "# Changelog\n\n## 1.3.0 - 2026-02-01\n";
    let cases = [(None, None), (Some(with_entry), Some(with_entry))];

    for (changelog, expected) in cases {
        let process = update_process(Ok(()));
        let files = demo_files(changelog);

        // Act
        let (result, stdout) = run_prepare("1.3.0", "2026-02-03", &process, &files);

        // Assert
        assert_eq!(result, Ok(()));
        assert!(stdout.ends_with("/workspace/demo/Cargo.lock\n"), "{stdout}");
        let changelog = lock(&files)
            .get(Path::new("/workspace/demo/CHANGELOG.md"))
            .map(|contents| String::from_utf8(contents.clone()).expect("UTF-8"));
        assert_eq!(changelog.as_deref(), expected);
    }
}

#[test]
fn run_rejects_invalid_versions_and_dates_before_touching_files() {
    // Arrange
    let cases = [
        (
            "1.3",
            "2026-02-03",
            "`1.3` is not a semver version: unexpected end of input while parsing minor version \
             number",
        ),
        (
            "1.2.3",
            "2026-02-03",
            "version 1.2.3 must be newer than the apps' version 1.2.3",
        ),
        (
            "v1.0.0",
            "2026-02-03",
            "version 1.0.0 must be newer than the apps' version 1.2.3",
        ),
        (
            "1.3.0",
            "2026-2-3",
            "`--date` must be a `YYYY-MM-DD` date, found `2026-2-3`",
        ),
    ];

    for (version, date, expected) in cases {
        let process = if version == "1.3" || date != "2026-02-03" {
            MockProcessRunner::new()
        } else {
            metadata_process(&demo_metadata())
        };
        let files = demo_files(Some(CHANGELOG));

        // Act
        let (result, stdout) = run_prepare(version, date, &process, &files);

        // Assert
        assert_eq!(result, Err(expected.to_string()), "{version} {date}");
        assert_eq!(stdout, "");
        assert_eq!(text(&files, "/workspace/demo/Cargo.toml"), ROOT_MANIFEST);
    }
}

#[test]
fn run_reports_unreadable_manifests_and_cargo_update_failures() {
    // Arrange
    let non_utf8 = demo_files(None);
    lock(&non_utf8).insert(
        PathBuf::from("/workspace/demo/crates/demo-lib/Cargo.toml"),
        vec![0xff],
    );
    let invalid = demo_files(None);
    lock(&invalid).insert(
        PathBuf::from("/workspace/demo/crates/demo-tool/Cargo.toml"),
        b"[".to_vec(),
    );
    let updatable = demo_files(None);
    let failing_update = update_process(Err("`cargo update --workspace` failed".to_string()));

    // Act
    let decoding = run_prepare(
        "1.3.0",
        "2026-02-03",
        &metadata_process(&demo_metadata()),
        &non_utf8,
    );
    let parsing = run_prepare(
        "1.3.0",
        "2026-02-03",
        &metadata_process(&demo_metadata()),
        &invalid,
    );
    let updating = run_prepare("1.3.0", "2026-02-03", &failing_update, &updatable);

    // Assert
    assert_eq!(
        decoding.0,
        Err(
            "`/workspace/demo/crates/demo-lib/Cargo.toml` is not UTF-8: invalid utf-8 sequence of \
             1 bytes from index 0"
                .to_string()
        )
    );
    assert_eq!(text(&non_utf8, "/workspace/demo/Cargo.toml"), ROOT_MANIFEST);
    let parse_error = parsing.0.expect_err("invalid manifest");
    assert!(
        parse_error.starts_with("failed to parse `/workspace/demo/crates/demo-tool/Cargo.toml`"),
        "{parse_error}"
    );
    assert_eq!(text(&invalid, "/workspace/demo/Cargo.toml"), ROOT_MANIFEST);
    assert_eq!(
        updating,
        (
            Err("`cargo update --workspace` failed".to_string()),
            String::new()
        )
    );
}

#[test]
fn check_date_accepts_only_calendar_shaped_dates() {
    // Arrange
    let dates = [
        ("2026-11-20", true),
        ("0001-01-31", true),
        ("2026-13-01", false),
        ("2026-00-10", false),
        ("2026-01-32", false),
        ("2026-01-1", false),
        ("26-01-10", false),
        ("2026-01-10-01", false),
        ("2026-+1-10", false),
        ("", false),
    ];

    for (date, valid) in dates {
        // Act
        let checked = check_date(date);

        // Assert
        assert_eq!(checked.is_ok(), valid, "{date}");
    }
}
