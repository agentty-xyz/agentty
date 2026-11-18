//! `assemble` unit tests with mocked host boundaries.

use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use serde_json::{Map, Value, json};

use crate::checksum;
use crate::cli::{AssembleOptions, Host};
use crate::file_system::MockFileSystem;
use crate::process::{MockProcessRunner, ProcessCommand};
use crate::workspace::test_support::{
    MemoryFiles, demo_metadata, lock, memory_file_system, memory_files, metadata_process,
};

/// Every app archive the demo plan expects.
const ARCHIVES: [&str; 4] = [
    "demo-app-aarch64-apple-darwin.tar.gz",
    "demo-app-x86_64-unknown-linux-gnu.tar.gz",
    "demo-tool-aarch64-apple-darwin.tar.gz",
    "demo-tool-x86_64-unknown-linux-gnu.tar.gz",
];

/// `git archive` output the mocked Git returns.
const SOURCE_TAR: &[u8] = b"demo source tar";

/// Returns an artifacts directory holding every demo archive with a matching
/// checksum file.
fn demo_artifacts() -> MemoryFiles {
    let files = memory_files(&[]);
    for archive in ARCHIVES {
        let contents = format!("{archive} contents").into_bytes();
        let line = checksum::line(&checksum::sha256(&contents), archive);
        let mut files = lock(&files);
        files.insert(
            artifact_path(&format!("{archive}.sha256")),
            line.into_bytes(),
        );
        files.insert(artifact_path(archive), contents);
    }

    files
}

fn artifact_path(name: &str) -> PathBuf {
    Path::new("artifacts").join(name)
}

/// Returns a process runner for the demo workspace whose `git archive`
/// answers `result`.
fn git_process(result: Result<Vec<u8>, String>) -> MockProcessRunner {
    let mut process = metadata_process(&demo_metadata());
    let expected = ProcessCommand {
        args: [
            "archive",
            "--format=tar",
            "--prefix=demo-app-1.2.3/",
            "HEAD",
        ]
        .map(OsString::from)
        .to_vec(),
        current_dir: Some(PathBuf::from("/workspace/demo")),
        program: OsString::from("git"),
    };
    process
        .expect_output()
        .withf(move |command| *command == expected)
        .once()
        .return_once(move |_| result);

    process
}

/// Runs `assemble` for `tag` and returns its result with the captured output.
fn run(
    tag: &str,
    process: &MockProcessRunner,
    file_system: &MockFileSystem,
) -> (Result<(), String>, String) {
    let host = Host {
        cargo: OsString::from("cargo"),
        file_system,
        process,
        source_date_epoch: None,
    };
    let options = AssembleOptions {
        artifacts_dir: PathBuf::from("artifacts"),
        tag: tag.to_string(),
    };
    let mut stdout = Vec::new();
    let result = super::run(&host, &options, &mut stdout);

    (result, String::from_utf8(stdout).expect("UTF-8 output"))
}

#[test]
fn assemble_writes_installers_the_source_archive_checksum_list_and_manifest() {
    // Arrange
    let process = git_process(Ok(SOURCE_TAR.to_vec()));
    let files = demo_artifacts();
    let file_system = memory_file_system(&files);

    // Act
    let (result, stdout) = run("v1.2.3", &process, &file_system);

    // Assert
    assert_eq!(result, Ok(()));
    let installers = ["demo-app-installer.sh", "demo-tool-installer.sh"];
    let written = [
        installers[0],
        installers[1],
        "source.tar.gz",
        "source.tar.gz.sha256",
        "sha256.sum",
        "release-manifest.json",
    ];
    assert_eq!(
        stdout,
        written.map(|name| format!("artifacts/{name}\n")).concat()
    );
    let files = lock(&files);
    let read = |name: &str| files[&artifact_path(name)].clone();
    let mut source = Vec::new();
    GzDecoder::new(read("source.tar.gz").as_slice())
        .read_to_end(&mut source)
        .expect("decompress source archive");
    assert_eq!(source, SOURCE_TAR);
    let digest = |name: &str| checksum::sha256(&read(name));
    assert_eq!(
        read("source.tar.gz.sha256"),
        checksum::line(&digest("source.tar.gz"), "source.tar.gz").into_bytes()
    );
    let installer = String::from_utf8(read(installers[1])).expect("UTF-8 installer");
    assert!(installer.contains(&format!(
        "_sha256='{}'\n",
        digest("demo-tool-x86_64-unknown-linux-gnu.tar.gz")
    )));
    let mut published = [&ARCHIVES[..], &installers, &["source.tar.gz"]].concat();
    published.sort_unstable();
    assert_eq!(
        String::from_utf8(read("sha256.sum")).expect("UTF-8 checksum list"),
        published
            .iter()
            .map(|name| checksum::line(&digest(name), name))
            .collect::<String>()
    );
    let manifest = read("release-manifest.json");
    assert!(manifest.ends_with(b"}\n"));
    let manifest: Value = serde_json::from_slice(&manifest).expect("manifest JSON");
    assert_eq!(manifest["plan"]["tag"], "v1.2.3");
    assert_eq!(manifest["plan"]["apps"][1]["name"], "demo-tool");
    let expected_sha256 = published
        .iter()
        .map(|name| (name.to_string(), json!(digest(name))))
        .collect::<Map<_, _>>();
    assert_eq!(manifest["sha256"], Value::Object(expected_sha256));
}

#[test]
fn assemble_lists_every_missing_or_mismatched_archive() {
    // Arrange
    let process = metadata_process(&demo_metadata());
    let files = demo_artifacts();
    {
        let mut files = lock(&files);
        files.remove(&artifact_path(ARCHIVES[0]));
        files.remove(&artifact_path(&format!("{}.sha256", ARCHIVES[1])));
        files.insert(artifact_path(ARCHIVES[3]), b"rebuilt".to_vec());
    }
    let file_system = memory_file_system(&files);

    // Act
    let (result, stdout) = run("v1.2.3", &process, &file_system);

    // Assert
    assert_eq!(
        result,
        Err(
            "`artifacts` does not hold every release archive with a matching checksum file:\n  \
             missing `demo-app-aarch64-apple-darwin.tar.gz`\n  missing \
             `demo-app-x86_64-unknown-linux-gnu.tar.gz.sha256`\n  \
             `demo-tool-x86_64-unknown-linux-gnu.tar.gz.sha256` does not match \
             `demo-tool-x86_64-unknown-linux-gnu.tar.gz`"
                .to_string()
        )
    );
    assert_eq!(stdout, "");
}

#[test]
fn assemble_reports_tag_inspection_and_git_failures() {
    // Arrange
    let no_process = MockProcessRunner::new();
    let mismatched_tag = metadata_process(&demo_metadata());
    let uninspectable = metadata_process(&demo_metadata());
    let failing_git = git_process(Err("`git archive` failed".to_string()));
    let mut unreadable = MockFileSystem::new();
    unreadable
        .expect_is_file()
        .returning(|_| Err(io::Error::from(io::ErrorKind::PermissionDenied)));
    let files = demo_artifacts();

    // Act
    let invalid = run("release", &no_process, &unreadable);
    let mismatched = run("v9.9.9", &mismatched_tag, &unreadable);
    let inspection = run("v1.2.3", &uninspectable, &unreadable);
    let git = run("v1.2.3", &failing_git, &memory_file_system(&files));

    // Assert
    assert!(
        invalid
            .0
            .expect_err("invalid tag")
            .starts_with("release tag `release` must be `v<semver>` or `<semver>`:")
    );
    assert_eq!(
        mismatched.0,
        Err(
            "release tag `v9.9.9` names version 9.9.9, but the apps have version 1.2.3".to_string()
        )
    );
    assert_eq!(
        inspection.0,
        Err(
            "failed to inspect `artifacts/demo-app-aarch64-apple-darwin.tar.gz`: permission denied"
                .to_string()
        )
    );
    assert_eq!(git.0, Err("`git archive` failed".to_string()));
    assert!(!lock(&files).contains_key(&artifact_path("sha256.sum")));
}
