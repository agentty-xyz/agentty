//! `build`: compiles every app for one target and writes each app's archive
//! and checksum file.

use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

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
    let includes = workspace
        .apps
        .iter()
        .map(|app| host.include_files(&workspace, app))
        .collect::<Result<Vec<_>, String>>()?;
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
    for (app, includes) in workspace.apps.iter().zip(&includes) {
        let contents = app_archive(host, app, includes, target, &binary_dir)?.to_tar_gz(mtime)?;
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

/// Returns `app`'s archive below its top-level directory: its binaries and
/// its `include` entries, each read from the source file it resolved to.
fn app_archive(
    host: &Host<'_>,
    app: &App,
    includes: &[(&str, PathBuf)],
    target: Target,
    binary_dir: &Path,
) -> Result<Archive, String> {
    let mut archive = Archive::new(artifact::archive_directory(&app.name, target));
    for binary in &app.binaries {
        let contents = host.read_file(&binary_dir.join(binary))?;
        archive.add_file(binary, FileMode::Executable, contents)?;
    }
    for (include, source) in includes {
        archive.add_file(include, FileMode::Regular, host.read_file(source)?)?;
    }

    Ok(archive)
}

#[cfg(test)]
#[path = "build_test.rs"]
mod tests;
