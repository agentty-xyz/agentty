//! `build` unit tests with mocked host boundaries.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;

use super::profile_directory;
use crate::checksum;
use crate::cli::{BuildOptions, Host};
use crate::file_system::MockFileSystem;
use crate::process::{MockProcessRunner, ProcessCommand};
use crate::workspace::test_support::{
    MemoryFiles, demo_metadata, lock, memory_file_system, memory_files, metadata_process,
};

/// Directory the demo `dist` profile builds `x86_64-unknown-linux-gnu`
/// binaries into.
const BINARY_DIR: &str = "/workspace/demo/target/x86_64-unknown-linux-gnu/dist";

/// Returns the demo workspace's build inputs: every binary and include file.
fn demo_inputs() -> MemoryFiles {
    memory_files(&[
        ("/workspace/demo/README.md", b"# demo\n"),
        ("/workspace/demo/LICENSE", b"license\n"),
        (&format!("{BINARY_DIR}/demo-app"), b"demo-app binary"),
        (&format!("{BINARY_DIR}/demo-helper"), b"demo-helper binary"),
        (&format!("{BINARY_DIR}/demo-tool"), b"demo-tool binary"),
    ])
}

/// Returns build options for `target` with an optional output directory.
fn options(target: &str, out_dir: Option<&str>) -> BuildOptions {
    BuildOptions {
        out_dir: out_dir.map(PathBuf::from),
        profile: None,
        target: target.to_string(),
    }
}

/// Returns the `cargo build` call the demo workspace expects.
fn demo_cargo_build() -> ProcessCommand {
    cargo_build_with_profile("dist")
}

/// Returns the demo workspace's `cargo build` call with `profile`.
fn cargo_build_with_profile(profile: &str) -> ProcessCommand {
    ProcessCommand {
        args: [
            "build",
            "--locked",
            "--profile",
            profile,
            "--target",
            "x86_64-unknown-linux-gnu",
            "--package",
            "demo-app",
            "--package",
            "demo-tool",
            "--bins",
        ]
        .map(OsString::from)
        .to_vec(),
        current_dir: Some(PathBuf::from("/workspace/demo")),
        program: OsString::from("cargo"),
    }
}

/// Returns a process runner for the demo workspace whose `cargo build`
/// answers `result`.
fn build_process(result: Result<(), String>) -> MockProcessRunner {
    let mut process = metadata_process(&demo_metadata());
    process
        .expect_stream()
        .withf(|command| *command == demo_cargo_build())
        .once()
        .return_once(move |_| result);

    process
}

/// Runs `build` and returns its result with the captured output.
fn run(
    options: &BuildOptions,
    process: &MockProcessRunner,
    file_system: &MockFileSystem,
    source_date_epoch: Option<&str>,
) -> (Result<(), String>, String) {
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system,
        process,
        source_date_epoch: source_date_epoch.map(OsString::from),
    };
    let mut stdout = Vec::new();
    let result = super::run(&host, options, &mut stdout);

    (result, String::from_utf8(stdout).expect("UTF-8 output"))
}

/// Returns the paths and modification times of a `.tar.gz` archive's entries.
fn entries(archive: &[u8]) -> Vec<(String, u64)> {
    let mut tar = tar::Archive::new(GzDecoder::new(archive));

    tar.entries()
        .expect("entries")
        .map(|entry| {
            let mut entry = entry.expect("entry");
            let mut contents = Vec::new();
            entry.read_to_end(&mut contents).expect("contents");
            let header = entry.header();

            (
                String::from_utf8_lossy(&header.path_bytes()).into_owned(),
                header.mtime().expect("mtime"),
            )
        })
        .collect()
}

#[test]
fn build_runs_one_cargo_build_and_writes_each_app_archive_with_its_checksum() {
    // Arrange
    let process = build_process(Ok(()));
    let files = demo_inputs();
    let file_system = memory_file_system(&files);

    // Act
    let (result, stdout) = run(
        &options("x86_64-unknown-linux-gnu", None),
        &process,
        &file_system,
        Some("1700000000"),
    );

    // Assert
    assert_eq!(result, Ok(()));
    let out = "/workspace/demo/target/ag-release.out";
    let archive = |app: &str| format!("{out}/{app}-x86_64-unknown-linux-gnu.tar.gz");
    assert_eq!(
        stdout,
        format!(
            "{0}\n{0}.sha256\n{1}\n{1}.sha256\n",
            archive("demo-app"),
            archive("demo-tool")
        )
    );
    let files = lock(&files);
    let demo_app = &files[Path::new(&archive("demo-app"))];
    assert_eq!(
        files[Path::new(&format!("{}.sha256", archive("demo-app")))],
        checksum::line(
            &checksum::sha256(demo_app),
            "demo-app-x86_64-unknown-linux-gnu.tar.gz"
        )
        .into_bytes()
    );
    let directory = "demo-app-x86_64-unknown-linux-gnu";
    assert_eq!(
        entries(demo_app),
        ["/", "/LICENSE", "/README.md", "/demo-app", "/demo-helper"]
            .map(|path| (format!("{directory}{path}"), 1_700_000_000))
    );
    let demo_tool = &files[Path::new(&archive("demo-tool"))];
    assert_eq!(entries(demo_tool).len(), 4);
}

#[test]
fn build_writes_into_the_requested_out_dir() {
    // Arrange
    let process = build_process(Ok(()));
    let files = demo_inputs();
    let file_system = memory_file_system(&files);

    // Act
    let (result, stdout) = run(
        &options("x86_64-unknown-linux-gnu", Some("dist")),
        &process,
        &file_system,
        None,
    );

    // Assert
    assert_eq!(result, Ok(()));
    assert!(
        stdout.starts_with("dist/demo-app-x86_64-unknown-linux-gnu.tar.gz\n"),
        "{stdout}"
    );
    assert!(lock(&files).contains_key(Path::new("dist/demo-tool-x86_64-unknown-linux-gnu.tar.gz")));
}

#[test]
fn build_uses_the_requested_profile_and_its_output_directory() {
    // Arrange
    let mut process = metadata_process(&demo_metadata());
    process
        .expect_stream()
        .withf(|command| *command == cargo_build_with_profile("dev"))
        .once()
        .return_once(|_| Ok(()));
    let debug_dir = "/workspace/demo/target/x86_64-unknown-linux-gnu/debug";
    let files = memory_files(&[
        ("/workspace/demo/README.md", b"# demo\n"),
        ("/workspace/demo/LICENSE", b"license\n"),
        (&format!("{debug_dir}/demo-app"), b"demo-app binary"),
        (&format!("{debug_dir}/demo-helper"), b"demo-helper binary"),
        (&format!("{debug_dir}/demo-tool"), b"demo-tool binary"),
    ]);
    let file_system = memory_file_system(&files);
    let options = BuildOptions {
        profile: Some("dev".to_string()),
        ..options("x86_64-unknown-linux-gnu", Some("dist"))
    };

    // Act
    let (result, _) = run(&options, &process, &file_system, None);

    // Assert
    assert_eq!(result, Ok(()));
    assert!(lock(&files).contains_key(Path::new("dist/demo-app-x86_64-unknown-linux-gnu.tar.gz")));
}

#[test]
fn build_rejects_invalid_inputs_before_running_cargo() {
    // Arrange
    let cases = [
        (
            "x86_64-apple-darwin",
            demo_inputs(),
            None,
            "target `x86_64-apple-darwin` is not configured in \
             `workspace.metadata.release.targets`; configured targets: aarch64-apple-darwin, \
             x86_64-unknown-linux-gnu",
        ),
        (
            "x86_64-unknown-linux-gnu",
            memory_files(&[]),
            None,
            "`workspace.metadata.release.include` entry `README.md` is not a file in the \
             workspace root",
        ),
        (
            "x86_64-unknown-linux-gnu",
            demo_inputs(),
            Some("soon"),
            "`SOURCE_DATE_EPOCH` must be a nonnegative integer number of seconds, found `soon`",
        ),
    ];

    for (target, files, source_date_epoch, expected) in cases {
        let process = metadata_process(&demo_metadata());
        let file_system = memory_file_system(&files);

        // Act
        let (result, stdout) = run(
            &options(target, None),
            &process,
            &file_system,
            source_date_epoch,
        );

        // Assert
        assert_eq!(result, Err(expected.to_string()));
        assert_eq!(stdout, "");
    }
}

#[test]
fn build_reports_cargo_read_and_write_failures() {
    // Arrange
    let failed_build = build_process(Err("`cargo build` failed".to_string()));
    let missing_binary = build_process(Ok(()));
    let unwritable = build_process(Ok(()));
    let inputs = demo_inputs();
    let without_binary = demo_inputs();
    lock(&without_binary).remove(Path::new(&format!("{BINARY_DIR}/demo-helper")));
    let mut read_only = MockFileSystem::new();
    read_only.expect_is_file().returning(|_| Ok(true));
    read_only.expect_read().returning(|_| Ok(Vec::new()));
    read_only
        .expect_write()
        .returning(|_, _| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    let options = options("x86_64-unknown-linux-gnu", Some("dist"));

    // Act
    let build = run(&options, &failed_build, &memory_file_system(&inputs), None);
    let read = run(
        &options,
        &missing_binary,
        &memory_file_system(&without_binary),
        None,
    );
    let write = run(&options, &unwritable, &read_only, None);

    // Assert
    assert_eq!(build.0, Err("`cargo build` failed".to_string()));
    assert_eq!(
        read.0,
        Err(format!(
            "failed to read `{BINARY_DIR}/demo-helper`: entity not found"
        ))
    );
    assert_eq!(
        write.0,
        Err(
            "failed to write `dist/demo-app-x86_64-unknown-linux-gnu.tar.gz`: permission denied"
                .to_string()
        )
    );
}

#[test]
fn profile_directory_follows_cargo_output_layout() {
    // Arrange
    let profiles = ["dev", "test", "release", "bench", "dist"];

    // Act
    let directories = profiles.map(profile_directory);

    // Assert
    assert_eq!(
        directories,
        ["debug", "debug", "release", "release", "dist"]
    );
}
