//! Command dispatch unit tests with mocked host boundaries.

use std::ffi::OsString;
use std::io::{self, Write};
use std::path::Path;

use clap::Parser;
use serde_json::Value;

use crate::cli::{Cli, Command, Host, PlanOptions};
use crate::file_system::MockFileSystem;
use crate::process::MockProcessRunner;
use crate::workspace::test_support::{demo_metadata, metadata_bytes};

/// Returns a process runner that answers one `cargo metadata` call with the
/// demo workspace.
fn demo_process() -> MockProcessRunner {
    let mut process = MockProcessRunner::new();
    process
        .expect_output()
        .once()
        .return_once(|_| Ok(metadata_bytes(&demo_metadata())));

    process
}

/// Returns a file system on which every `is_file` call answers `result`.
fn file_system(result: fn(&Path) -> io::Result<bool>) -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    file_system.expect_is_file().returning(result);

    file_system
}

fn plan(tag: Option<&str>) -> Command {
    Command::Plan(PlanOptions {
        tag: tag.map(ToString::to_string),
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
    };
    let mut stdout = Vec::new();
    let result = host.run(command, &mut stdout);

    (result, String::from_utf8(stdout).expect("UTF-8 output"))
}

/// Writer whose every write fails.
struct BrokenPipe;

impl Write for BrokenPipe {
    fn write(&mut self, _buffer: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }
}

#[test]
fn cli_parses_plan_with_an_optional_tag() {
    // Arrange
    let arguments = [
        vec!["ag-release", "plan"],
        vec!["ag-release", "plan", "--tag", "v1.2.3"],
    ];

    // Act
    let tags = arguments.map(|arguments| match Cli::parse_from(arguments).command {
        Command::Plan(options) => options.tag,
    });

    // Assert
    assert_eq!(tags, [None, Some("v1.2.3".to_string())]);
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
    };

    // Act
    let result = host.run(&plan(None), &mut BrokenPipe);

    // Assert
    assert_eq!(
        result,
        Err("failed to write the release plan: broken pipe".to_string())
    );
}
