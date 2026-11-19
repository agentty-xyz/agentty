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
use crate::workspace::Workspace;
use crate::workspace::test_support::{
    BrokenPipe, MemoryFiles, demo_metadata, memory_file_system, memory_files, metadata_bytes,
    metadata_process,
};

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

/// Returns the demo workspace that [`demo_metadata`] describes.
fn demo_workspace() -> Workspace {
    Workspace::from_metadata(&metadata_bytes(&demo_metadata())).expect("demo workspace")
}

/// Returns the demo workspace's `README.md` and `LICENSE`, and a
/// package-local `README.md` of `demo-tool`.
fn demo_documents() -> MemoryFiles {
    memory_files(&[
        ("/workspace/demo/README.md", b"# demo\n"),
        ("/workspace/demo/LICENSE", b"license\n"),
        (
            "/workspace/demo/crates/demo-tool/README.md",
            b"# demo-tool\n",
        ),
    ])
}

/// Returns a host on `process` and `file_system`.
fn host<'host>(
    process: &'host MockProcessRunner,
    file_system: &'host MockFileSystem,
) -> Host<'host> {
    Host {
        cargo: OsString::from("cargo"),
        file_system,
        process,
        source_date_epoch: None,
    }
}

/// Runs `command` and returns its result with the captured output.
fn run(
    command: &Command,
    process: &MockProcessRunner,
    file_system: &MockFileSystem,
) -> (Result<(), String>, String) {
    let host = host(process, file_system);
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
    let files = memory_files(&[
        ("/workspace/demo/README.md", b"# demo\n"),
        ("/workspace/demo/crates/demo-app/LICENSE", b"app license\n"),
        (
            "/workspace/demo/crates/demo-tool/LICENSE",
            b"tool license\n",
        ),
    ]);
    let file_system = memory_file_system(&files);

    // Act
    let (result, stdout) = run(&plan(Some("v1.2.3")), &process, &file_system);

    // Assert
    assert_eq!(result, Ok(()));
    assert!(stdout.ends_with("}\n"), "{stdout}");
    let plan: Value = serde_json::from_str(&stdout).expect("plan JSON");
    assert_eq!(plan["tag"], "v1.2.3");
    assert_eq!(plan["apps"][0]["name"], "demo-app");
    assert_eq!(plan["include"], serde_json::json!(["README.md", "LICENSE"]));
}

#[test]
fn app_file_prefers_the_package_directory_over_the_workspace_root() {
    // Arrange
    let process = MockProcessRunner::new();
    let files = demo_documents();
    let file_system = memory_file_system(&files);
    let host = host(&process, &file_system);
    let workspace = demo_workspace();
    let (demo_app, demo_tool) = (&workspace.apps[0], &workspace.apps[1]);

    // Act
    let app_readme = host.app_file(&workspace, demo_app, "README.md");
    let tool_readme = host.app_file(&workspace, demo_tool, "README.md");
    let tool_changelog = host.app_file(&workspace, demo_tool, "CHANGELOG.md");

    // Assert
    assert_eq!(
        app_readme,
        Ok(Some(PathBuf::from("/workspace/demo/README.md")))
    );
    assert_eq!(
        tool_readme,
        Ok(Some(PathBuf::from(
            "/workspace/demo/crates/demo-tool/README.md"
        )))
    );
    assert_eq!(tool_changelog, Ok(None));
}

#[test]
fn include_files_resolve_every_entry_per_app_or_name_the_missing_one() {
    // Arrange
    let process = MockProcessRunner::new();
    let files = demo_documents();
    let file_system = memory_file_system(&files);
    let host = host(&process, &file_system);
    let workspace = demo_workspace();
    let mut with_changelog = demo_workspace();
    with_changelog
        .config
        .include
        .push("CHANGELOG.md".to_string());

    // Act
    let tool = host.include_files(&workspace, &workspace.apps[1]);
    let missing = host.include_files(&with_changelog, &with_changelog.apps[1]);

    // Assert
    assert_eq!(
        tool,
        Ok(vec![
            (
                "README.md",
                PathBuf::from("/workspace/demo/crates/demo-tool/README.md")
            ),
            ("LICENSE", PathBuf::from("/workspace/demo/LICENSE")),
        ])
    );
    assert_eq!(
        missing,
        Err(
            "`workspace.metadata.release.include` entry `CHANGELOG.md` is not a file in the \
             package directory of app `demo-tool` or in the workspace root"
                .to_string()
        )
    );
}

#[test]
fn include_files_of_an_app_packaged_at_the_workspace_root_come_from_the_root() {
    // Arrange
    let process = MockProcessRunner::new();
    let files = memory_files(&[("/workspace/demo/README.md", b"# demo\n")]);
    let file_system = memory_file_system(&files);
    let host = host(&process, &file_system);
    let mut workspace = demo_workspace();
    workspace.apps[0].package_directory = PathBuf::from("/workspace/demo");
    let root_package = &workspace.apps[0];

    // Act
    let readme = host.app_file(&workspace, root_package, "README.md");
    let includes = host.include_files(&workspace, root_package);

    // Assert
    assert_eq!(readme, Ok(Some(PathBuf::from("/workspace/demo/README.md"))));
    assert_eq!(
        includes,
        Err(
            "`workspace.metadata.release.include` entry `LICENSE` is not a file in the package \
             directory of app `demo-app` or in the workspace root"
                .to_string()
        )
    );
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
                     package directory of app `demo-app` or in the workspace root"
                        .to_string()
                ),
                String::new()
            ),
            (
                Err(
                    "failed to inspect `/workspace/demo/crates/demo-app/README.md`: permission \
                     denied"
                        .to_string()
                ),
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
    let host = host(&process, &file_system);

    // Act
    let result = host.run(&plan(None), &mut BrokenPipe);

    // Assert
    assert_eq!(
        result,
        Err("failed to write the release plan: broken pipe".to_string())
    );
}
