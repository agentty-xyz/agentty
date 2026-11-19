//! Command dispatch unit tests with mocked host boundaries.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::Value;

use crate::cli::{
    AssembleOptions, BuildOptions, Cli, Command, Host, NotesOptions, PlanOptions, WorkflowOptions,
};
use crate::file_system::MockFileSystem;
use crate::process::MockProcessRunner;
use crate::workspace::test_support::{BrokenPipe, demo_metadata, metadata_process};

/// Returns a process runner that answers one `cargo metadata` call with the
/// demo workspace.
fn demo_process() -> MockProcessRunner {
    metadata_process(&demo_metadata())
}

/// Returns a file system on which every `is_file` call answers `result`.
fn file_system(result: fn(&Path) -> io::Result<bool>) -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    file_system.expect_is_file().returning(result);

    file_system
}

fn plan(tag: Option<&str>) -> Command {
    plan_targets(tag, &[])
}

fn plan_targets(tag: Option<&str>, targets: &[&str]) -> Command {
    Command::Plan(PlanOptions {
        tag: tag.map(ToString::to_string),
        targets: targets.iter().map(ToString::to_string).collect(),
    })
}

/// Runs `command` and returns its result with the captured output.
fn run(
    command: &Command,
    process: &MockProcessRunner,
    file_system: &MockFileSystem,
) -> (Result<(), String>, String) {
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system,
        process,
        source_date_epoch: None,
    };
    let mut stdout = Vec::new();
    let result = host.run(command, &mut stdout);

    (result, String::from_utf8(stdout).expect("UTF-8 output"))
}

#[test]
fn cli_parses_every_command() {
    // Arrange
    let arguments = [
        vec!["ag-release", "plan"],
        vec!["ag-release", "plan", "--tag", "v1.2.3"],
        vec!["ag-release", "plan", "--target", "a", "--target", "b"],
        vec!["ag-release", "build", "--target", "aarch64-apple-darwin"],
        vec![
            "ag-release",
            "build",
            "--target",
            "aarch64-apple-darwin",
            "--out-dir",
            "dist",
            "--profile",
            "dev",
        ],
        vec![
            "ag-release",
            "assemble",
            "--tag",
            "v1.2.3",
            "--artifacts-dir",
            "artifacts",
        ],
        vec![
            "ag-release",
            "assemble",
            "--tag",
            "v1.2.3",
            "--artifacts-dir",
            "artifacts",
            "--target",
            "a",
        ],
        vec!["ag-release", "notes", "--tag", "v1.2.3"],
        vec!["ag-release", "notes", "--tag", "v1.2.3", "--title"],
        vec!["ag-release", "workflow"],
        vec!["ag-release", "workflow", "--check"],
    ];

    // Act
    let commands = arguments.map(|arguments| match Cli::parse_from(arguments).command {
        Command::Plan(PlanOptions { tag, targets }) => format!("plan {tag:?} {targets:?}"),
        Command::Build(BuildOptions {
            out_dir,
            profile,
            target,
        }) => format!("build {target} {out_dir:?} {profile:?}"),
        Command::Assemble(AssembleOptions {
            artifacts_dir,
            tag,
            targets,
        }) => format!("assemble {tag} {} {targets:?}", artifacts_dir.display()),
        Command::Notes(NotesOptions { tag, title }) => format!("notes {tag} {title}"),
        Command::Workflow(WorkflowOptions { check }) => format!("workflow {check}"),
    });

    // Assert
    assert_eq!(
        commands,
        [
            "plan None []",
            "plan Some(\"v1.2.3\") []",
            "plan None [\"a\", \"b\"]",
            "build aarch64-apple-darwin None None",
            "build aarch64-apple-darwin Some(\"dist\") Some(\"dev\")",
            "assemble v1.2.3 artifacts []",
            "assemble v1.2.3 artifacts [\"a\"]",
            "notes v1.2.3 false",
            "notes v1.2.3 true",
            "workflow false",
            "workflow true",
        ]
    );
}

#[test]
fn report_written_prints_one_path_per_line_and_reports_write_failures() {
    // Arrange
    let paths = [
        PathBuf::from("out/a.tar.gz"),
        PathBuf::from("out/a.tar.gz.sha256"),
    ];
    let mut stdout = Vec::new();

    // Act
    let printed = Host::report_written(&mut stdout, &paths);
    let failed = Host::report_written(&mut BrokenPipe, &paths);

    // Assert
    assert_eq!(printed, Ok(()));
    assert_eq!(
        String::from_utf8(stdout).expect("UTF-8 output"),
        "out/a.tar.gz\nout/a.tar.gz.sha256\n"
    );
    assert_eq!(
        failed,
        Err("failed to write the command output: broken pipe".to_string())
    );
}

#[test]
fn plan_prints_the_plan_json_after_checking_include_files() {
    // Arrange
    let process = demo_process();
    let mut file_system = MockFileSystem::new();
    for name in ["README.md", "LICENSE"] {
        file_system
            .expect_is_file()
            .withf(move |path| path == Path::new("/workspace/demo").join(name))
            .once()
            .returning(|_| Ok(true));
    }

    // Act
    let (result, stdout) = run(&plan(Some("v1.2.3")), &process, &file_system);

    // Assert
    assert_eq!(result, Ok(()));
    assert!(stdout.ends_with("}\n"), "{stdout}");
    let plan: Value = serde_json::from_str(&stdout).expect("plan JSON");
    assert_eq!(plan["tag"], "v1.2.3");
    assert_eq!(plan["apps"][0]["name"], "demo-app");
}

#[test]
fn plan_narrows_to_the_requested_configured_targets() {
    // Arrange
    let process = [demo_process(), demo_process()];
    let file_system = file_system(|_| Ok(true));
    let narrowed = plan_targets(None, &["x86_64-unknown-linux-gnu"]);
    let unconfigured = plan_targets(None, &["x86_64-apple-darwin"]);

    // Act
    let (narrowed_result, stdout) = run(&narrowed, &process[0], &file_system);
    let (unconfigured_result, _) = run(&unconfigured, &process[1], &file_system);

    // Assert
    assert_eq!(narrowed_result, Ok(()));
    let plan: Value = serde_json::from_str(&stdout).expect("plan JSON");
    assert_eq!(
        plan["matrix"]["include"],
        serde_json::json!([{"runner": "ubuntu-22.04", "target": "x86_64-unknown-linux-gnu"}])
    );
    assert_eq!(
        plan["apps"][0]["archives"][0]["target"],
        "x86_64-unknown-linux-gnu"
    );
    assert_eq!(
        plan["apps"][0]["archives"].as_array().map(Vec::len),
        Some(1)
    );
    assert!(
        unconfigured_result
            .expect_err("unconfigured target")
            .starts_with("target `x86_64-apple-darwin` is not configured")
    );
}

#[test]
fn plan_rejects_missing_or_unreadable_include_files() {
    // Arrange
    let process = [demo_process(), demo_process()];
    let file_systems = [
        file_system(|_| Ok(false)),
        file_system(|_| Err(io::Error::from(io::ErrorKind::PermissionDenied))),
    ];

    // Act
    let results = [0, 1].map(|index| run(&plan(None), &process[index], &file_systems[index]));

    // Assert
    assert_eq!(
        results,
        [
            (
                Err(
                    "`workspace.metadata.release.include` entry `README.md` is not a file in the \
                     workspace root"
                        .to_string()
                ),
                String::new()
            ),
            (
                Err("failed to inspect `/workspace/demo/README.md`: permission denied".to_string()),
                String::new()
            ),
        ]
    );
}

#[test]
fn plan_rejects_invalid_or_mismatched_tags() {
    // Arrange
    let no_process = MockProcessRunner::new();
    let mismatched_process = demo_process();
    let file_system = file_system(|_| Ok(true));

    // Act
    let invalid = run(&plan(Some("release")), &no_process, &file_system);
    let mismatched = run(&plan(Some("v9.9.9")), &mismatched_process, &file_system);

    // Assert
    assert!(
        invalid
            .0
            .expect_err("invalid tag")
            .starts_with("release tag `release` must be `v<semver>` or `<semver>`:")
    );
    assert_eq!(
        mismatched,
        (
            Err(
                "release tag `v9.9.9` names version 9.9.9, but the apps have version 1.2.3"
                    .to_string()
            ),
            String::new()
        )
    );
}

#[test]
fn plan_reports_output_write_failures() {
    // Arrange
    let process = demo_process();
    let file_system = file_system(|_| Ok(true));
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system: &file_system,
        process: &process,
        source_date_epoch: None,
    };

    // Act
    let result = host.run(&plan(None), &mut BrokenPipe);

    // Assert
    assert_eq!(
        result,
        Err("failed to write the release plan: broken pipe".to_string())
    );
}
