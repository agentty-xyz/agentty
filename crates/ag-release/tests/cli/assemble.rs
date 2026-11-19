//! `ag-release build` and `ag-release assemble` command-line contract,
//! exercised for real against a temporary workspace and Git repository.

use std::error::Error;
use std::fs::{self, File};
use std::io;
use std::path::Path;
use std::process::Output;

use flate2::read::GzDecoder;
use serde_json::Value;
use tempfile::tempdir;

use crate::support::{HOST_TARGET, ag_release, committed_demo_workspace, isolate_git};

/// Returns the root manifest of a demo workspace released for the host
/// target only.
fn demo_manifest() -> String {
    format!(
        r#"[workspace]
members = ["demo-app"]
resolver = "2"

[workspace.metadata.release]
include = ["README.md"]

[workspace.metadata.release.targets]
{HOST_TARGET} = "demo-runner"

[workspace.metadata.release.apps.demo-app]
"#
    )
}

/// Runs `ag-release` with `args` in `workspace`, building into `target_dir`.
fn run(workspace: &Path, target_dir: &Path, args: &[&str]) -> io::Result<Output> {
    isolate_git(&mut ag_release(workspace, args))
        .env("CARGO_TARGET_DIR", target_dir)
        .env("SOURCE_DATE_EPOCH", "1700000000")
        .output()
}

/// Returns the standard output of a successful run, or its standard error.
fn stdout(output: Output) -> Result<String, String> {
    let text = |bytes: Vec<u8>| String::from_utf8_lossy(&bytes).into_owned();
    if output.status.success() {
        return Ok(text(output.stdout));
    }

    Err(text(output.stderr))
}

/// Returns the path and mode of every entry in the `.tar.gz` at `path`.
fn archive_entries(path: &Path) -> io::Result<Vec<(String, u32)>> {
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(path)?));

    archive
        .entries()?
        .map(|entry| {
            let entry = entry?;
            let path = String::from_utf8_lossy(&entry.path_bytes()).into_owned();

            Ok((path, entry.header().mode()?))
        })
        .collect()
}

#[test]
fn build_then_assemble_publishes_a_verified_release() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workspace = committed_demo_workspace(&demo_manifest())?;
    let output = tempdir()?;
    let target_dir = output.path().join("target");
    let artifacts = output.path().join("artifacts");
    let out = artifacts.to_str().ok_or("UTF-8 artifacts path")?;
    let build_args = ["build", "--target", HOST_TARGET, "--out-dir", out];
    let assemble_args = ["assemble", "--tag", "v0.1.0", "--artifacts-dir", out];
    let archive = format!("demo-app-{HOST_TARGET}.tar.gz");

    // Act
    let built = stdout(run(workspace.path(), &target_dir, &build_args)?);
    let assembled = stdout(run(workspace.path(), &target_dir, &assemble_args)?);
    fs::remove_file(artifacts.join(format!("{archive}.sha256")))?;
    let incomplete = stdout(run(workspace.path(), &target_dir, &assemble_args)?);

    // Assert
    assert_eq!(
        built,
        Ok(format!("{out}/{archive}\n{out}/{archive}.sha256\n"))
    );
    let directory = format!("demo-app-{HOST_TARGET}");
    assert_eq!(
        archive_entries(&artifacts.join(&archive))?,
        [
            (format!("{directory}/"), 0o755),
            (format!("{directory}/README.md"), 0o644),
            (format!("{directory}/demo-app"), 0o755),
        ]
    );
    let written = [
        "source.tar.gz",
        "source.tar.gz.sha256",
        "sha256.sum",
        "release-manifest.json",
    ];
    assert_eq!(
        assembled,
        Ok(written.map(|name| format!("{out}/{name}\n")).concat())
    );
    let source = archive_entries(&artifacts.join("source.tar.gz"))?;
    assert!(
        source
            .iter()
            .any(|(path, _)| path == "demo-app-0.1.0/demo-app/src/main.rs"),
        "{source:?}"
    );
    let checksums = fs::read_to_string(artifacts.join("sha256.sum"))?;
    let (archive_digest, _) = checksums.split_once("  ").ok_or("checksum line")?;
    let manifest: Value =
        serde_json::from_slice(&fs::read(artifacts.join("release-manifest.json"))?)?;
    assert_eq!(manifest["plan"]["tag"], "v0.1.0");
    assert_eq!(manifest["sha256"][&archive], archive_digest);
    assert_eq!(
        manifest["sha256"].as_object().map(serde_json::Map::len),
        Some(checksums.lines().count())
    );
    assert_eq!(
        incomplete,
        Err(format!(
            "error: `{out}` does not hold every release archive with a matching checksum file:\n  \
             missing `{archive}.sha256`\n"
        ))
    );

    Ok(())
}
