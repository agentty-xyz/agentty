//! `ag-release build` and `ag-release assemble` command-line contract,
//! exercised for real against a temporary workspace and Git repository, and
//! `build` against this repository with a `cargo` that compiles nothing.

use std::error::Error;
use std::fs::{self, File};
use std::io::{self, Read};
use std::path::Path;
use std::process::Output;

use flate2::read::GzDecoder;
use serde_json::Value;
use tempfile::tempdir;

use crate::support::{
    HOST_TARGET, ag_release, committed_demo_workspace, isolate_git, write_executable,
};

/// Members of the demo workspace, each a released app.
const DEMO_APPS: [&str; 2] = ["demo-app", "demo-tool"];

/// Package-local `README.md` of `demo-tool`, which takes precedence over the
/// workspace root's.
const DEMO_TOOL_README: &str = "# demo-tool\n";

/// Returns the root manifest of a two-app demo workspace released for the
/// host target only, with npm packages.
fn demo_manifest() -> String {
    format!(
        r#"[workspace]
members = ["demo-app", "demo-tool"]
resolver = "2"

[workspace.metadata.release]
include = ["README.md"]
installers = ["npm"]

[workspace.metadata.release.targets]
{HOST_TARGET} = "demo-runner"

[workspace.metadata.release.apps.demo-app]
[workspace.metadata.release.apps.demo-tool]
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

/// Returns the contents of the entry `entry` in the `.tar.gz` at `path`.
fn archive_file(path: &Path, entry: &str) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut archive = tar::Archive::new(GzDecoder::new(File::open(path)?));
    for file in archive.entries()? {
        let mut file = file?;
        if file.path_bytes().as_ref() == entry.as_bytes() {
            let mut contents = Vec::new();
            file.read_to_end(&mut contents)?;

            return Ok(contents);
        }
    }

    Err(format!("`{}` has no entry `{entry}`", path.display()).into())
}

#[test]
fn build_then_assemble_publishes_a_verified_release() -> Result<(), Box<dyn Error>> {
    // Arrange
    let workspace = committed_demo_workspace(&demo_manifest(), &DEMO_APPS)?;
    fs::write(
        workspace.path().join("demo-tool/README.md"),
        DEMO_TOOL_README,
    )?;
    let output = tempdir()?;
    let target_dir = output.path().join("target");
    let artifacts = output.path().join("artifacts");
    let out = artifacts.to_str().ok_or("UTF-8 artifacts path")?;
    let build_args = ["build", "--target", HOST_TARGET, "--out-dir", out];
    let assemble_args = ["assemble", "--tag", "v0.1.0", "--artifacts-dir", out];
    let archive = format!("demo-app-{HOST_TARGET}.tar.gz");
    let tool_archive = format!("demo-tool-{HOST_TARGET}.tar.gz");

    // Act
    let built = stdout(run(workspace.path(), &target_dir, &build_args)?);
    let assembled = stdout(run(workspace.path(), &target_dir, &assemble_args)?);
    fs::remove_file(artifacts.join(format!("{archive}.sha256")))?;
    let incomplete = stdout(run(workspace.path(), &target_dir, &assemble_args)?);

    // Assert
    assert_eq!(
        built,
        Ok(format!(
            "{out}/{archive}\n{out}/{archive}.sha256\n{out}/{tool_archive}\n{out}/{tool_archive}.\
             sha256\n"
        ))
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
    let root_readme = fs::read(workspace.path().join("README.md"))?;
    assert_eq!(
        archive_file(&artifacts.join(&archive), &format!("{directory}/README.md"))?,
        root_readme
    );
    assert_eq!(
        archive_file(
            &artifacts.join(&tool_archive),
            &format!("demo-tool-{HOST_TARGET}/README.md")
        )?,
        DEMO_TOOL_README.as_bytes()
    );
    let app_package = artifacts.join("demo-app-npm-package.tar.gz");
    let tool_package = artifacts.join("demo-tool-npm-package.tar.gz");
    assert_eq!(
        archive_file(&app_package, "package/README.md")?,
        root_readme
    );
    assert_eq!(
        archive_file(&tool_package, "package/README.md")?,
        DEMO_TOOL_README.as_bytes()
    );
    let written = [
        "demo-app-npm-package.tar.gz",
        "demo-tool-npm-package.tar.gz",
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
    let archive_digest = checksums
        .lines()
        .find_map(|line| line.strip_suffix(&format!("  {archive}")))
        .ok_or("archive checksum line")?;
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

#[test]
fn build_takes_this_repository_readme_of_each_app_from_its_package_first()
-> Result<(), Box<dyn Error>> {
    // Arrange
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = tempdir()?;
    let target_dir = output.path().join("target");
    let artifacts = output.path().join("artifacts");
    let out = artifacts.to_str().ok_or("UTF-8 artifacts path")?;
    // A `cargo` whose `build` does nothing, so nothing compiles and the
    // archives hold placeholder binaries.
    let cargo = output.path().join("cargo");
    write_executable(
        &cargo,
        &format!(
            "if [ \"$1\" = build ]; then exit 0; fi\nexec '{}' \"$@\"",
            env!("CARGO")
        ),
    )?;
    let plan: Value = serde_json::from_str(&stdout(ag_release(&root, &["plan"]).output()?)?)?;
    let binary_dir = target_dir.join(HOST_TARGET).join("stub");
    fs::create_dir_all(&binary_dir)?;
    for app in plan["apps"].as_array().ok_or("plan apps")? {
        for binary in app["binaries"].as_array().ok_or("app binaries")? {
            let binary = binary.as_str().ok_or("binary name")?;
            fs::write(binary_dir.join(binary), "placeholder binary")?;
        }
    }
    let build_args = [
        "build",
        "--target",
        HOST_TARGET,
        "--profile",
        "stub",
        "--out-dir",
        out,
    ];

    // Act
    let built = ag_release(&root, &build_args)
        .env("CARGO", &cargo)
        .env("CARGO_TARGET_DIR", &target_dir)
        .output()?;

    // Assert
    stdout(built)?;
    let readme = |app: &str| {
        archive_file(
            &artifacts.join(format!("{app}-{HOST_TARGET}.tar.gz")),
            &format!("{app}-{HOST_TARGET}/README.md"),
        )
    };
    let package_readme = fs::read(root.join("crates/testty/README.md"))?;
    let root_readme = fs::read(root.join("README.md"))?;
    assert_ne!(package_readme, root_readme);
    assert_eq!(readme("testty")?, package_readme);
    assert_eq!(readme("agentty")?, root_readme);

    Ok(())
}
