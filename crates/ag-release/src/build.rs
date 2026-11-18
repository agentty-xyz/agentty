//! `build`: compiles every app for one target and writes each app's archive
//! and checksum file.

use std::ffi::OsString;
use std::io::Write;
use std::path::Path;

use crate::archive::{self, Archive, FileMode};
use crate::cli::{BuildOptions, Host};
use crate::process::ProcessCommand;
use crate::target::Target;
use crate::workspace::{App, Workspace};
use crate::{artifact, checksum};

/// Default output directory inside Cargo's target directory. Cargo profile
/// names cannot contain `.`, so it never collides with a profile's output.
const DEFAULT_OUT_DIR: &str = "ag-release.out";

/// Builds the configured apps for `options.target` with one `cargo build`
/// call, using `options.profile` instead of the configured profile when
/// given, then writes `<app>-<target>.tar.gz` and its `.sha256` file per app
/// and prints the written paths.
///
/// # Errors
/// Returns an error when the target is not configured, an input is missing,
/// `SOURCE_DATE_EPOCH` is invalid, the build fails, or an output cannot be
/// written.
pub(crate) fn run(
    host: &Host<'_>,
    options: &BuildOptions,
    stdout: &mut dyn Write,
) -> Result<(), String> {
    let mut workspace = Workspace::load(host.process, host.cargo.clone())?;
    let target = workspace.config.target(&options.target)?;
    if let Some(profile) = &options.profile {
        workspace.config.profile.clone_from(profile);
    }
    host.check_include(&workspace)?;
    let mtime = archive::modification_time(host.source_date_epoch.as_deref())?;
    host.process
        .stream(&cargo_build(host.cargo.clone(), &workspace, target))?;

    let out_dir = options
        .out_dir
        .clone()
        .unwrap_or_else(|| workspace.target_directory.join(DEFAULT_OUT_DIR));
    let binary_dir = workspace
        .target_directory
        .join(target.triple())
        .join(profile_directory(&workspace.config.profile));
    let mut written = Vec::new();
    for app in &workspace.apps {
        let contents = app_archive(host, &workspace, app, target, &binary_dir)?.to_tar_gz(mtime)?;
        let name = artifact::archive(&app.name, target);
        let checksum_name = artifact::checksum(&name);
        let checksum_line = checksum::line(&checksum::sha256(&contents), &name);
        for (file, file_contents) in [
            (name, contents.as_slice()),
            (checksum_name, checksum_line.as_bytes()),
        ] {
            let path = out_dir.join(file);
            host.write_file(&path, file_contents)?;
            written.push(path);
        }
    }

    Host::report_written(stdout, &written)
}

/// Returns the `cargo build` call that builds every app's binaries.
fn cargo_build(cargo: OsString, workspace: &Workspace, target: Target) -> ProcessCommand {
    let mut args = [
        "build",
        "--locked",
        "--profile",
        &workspace.config.profile,
        "--target",
        target.triple(),
    ]
    .map(OsString::from)
    .to_vec();
    for app in &workspace.apps {
        args.extend([OsString::from("--package"), OsString::from(&app.name)]);
    }
    args.push(OsString::from("--bins"));

    ProcessCommand {
        args,
        current_dir: Some(workspace.root.clone()),
        program: cargo,
    }
}

/// Returns the directory below `<target-dir>/<triple>/` that holds
/// `profile`'s output; the built-in `dev` and `test` profiles share `debug`
/// and `bench` uses `release`.
fn profile_directory(profile: &str) -> &str {
    match profile {
        "dev" | "test" => "debug",
        "bench" => "release",
        other => other,
    }
}

/// Returns `app`'s archive: its binaries and the configured `include` files
/// below the archive's top-level directory.
fn app_archive(
    host: &Host<'_>,
    workspace: &Workspace,
    app: &App,
    target: Target,
    binary_dir: &Path,
) -> Result<Archive, String> {
    let mut archive = Archive::new(artifact::archive_directory(&app.name, target));
    let binaries = app
        .binaries
        .iter()
        .map(|binary| (binary, binary_dir.join(binary), FileMode::Executable));
    let includes = workspace
        .config
        .include
        .iter()
        .map(|include| (include, workspace.root.join(include), FileMode::Regular));
    for (archive_path, source, mode) in binaries.chain(includes) {
        archive.add_file(archive_path, mode, host.read_file(&source)?)?;
    }

    Ok(archive)
}

#[cfg(test)]
#[path = "build_test.rs"]
mod tests;
