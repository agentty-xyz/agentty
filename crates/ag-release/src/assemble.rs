//! `assemble`: verifies every target's app archives and writes the
//! release-wide artifacts next to them.
//!
//! [`Assembly`] tracks the release files. Generators of further release
//! files, such as installers, read archive checksums from
//! [`Assembly::sha256`] and add their files with [`Assembly::publish`] before
//! [`Assembly::finish`] writes the checksum list and the manifest.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;

use crate::cli::{AssembleOptions, Host};
use crate::npm_package::{self, NpmPackage};
use crate::plan::ReleasePlan;
use crate::process::ProcessCommand;
use crate::shell_installer::ShellInstaller;
use crate::tag::ReleaseTag;
use crate::workspace::Workspace;
use crate::{archive, artifact, checksum};

/// Verifies the archives in `options.artifacts_dir` for the selected targets
/// (every configured target unless `options.targets` names some), writes
/// the configured installers, the source archive, the checksum list, and the
/// manifest into it, and prints the written paths.
///
/// # Errors
/// Returns an error when the tag is invalid, an expected archive or checksum
/// file is missing or mismatched, `git archive` fails, or an output cannot be
/// written.
pub(crate) fn run(
    host: &Host<'_>,
    options: &AssembleOptions,
    stdout: &mut dyn Write,
) -> Result<(), String> {
    let tag = ReleaseTag::parse(&options.tag)?;
    let mut workspace = Workspace::load(host.process, host.cargo.clone())?;
    workspace.config.select_targets(&options.targets)?;
    let plan = ReleasePlan::new(&workspace, Some(&tag))?;
    let mtime = archive::modification_time(host.source_date_epoch.as_deref())?;
    let mut assembly = Assembly::verify(host, &options.artifacts_dir, &plan)?;
    for app in &plan.apps {
        if let Some(name) = &app.shell_installer {
            let script = ShellInstaller::render(&plan, app, &tag.name, &assembly.sha256)?;
            assembly.publish(name, script.as_bytes())?;
        }
        if let (Some(tarball), Some(name)) = (&app.npm_tarball, &app.npm_package) {
            let package = NpmPackage {
                app,
                documents: npm_documents(host, &workspace.root)?,
                name,
                plan: &plan,
                sha256: &assembly.sha256,
                tag: &tag.name,
            };
            let contents = package.to_tar_gz(mtime)?;
            assembly.publish(tarball, &contents)?;
        }
    }
    let source = source_archive(host, &workspace, &plan)?;
    assembly.publish_with_checksum(artifact::SOURCE_ARCHIVE, &source)?;
    let written = assembly.finish()?;

    Host::report_written(stdout, &written)
}

/// Returns each of the npm package's documents that is a file in the
/// workspace `root`, with its contents.
fn npm_documents(host: &Host<'_>, root: &Path) -> Result<Vec<(&'static str, Vec<u8>)>, String> {
    let mut documents = Vec::new();
    for name in npm_package::DOCUMENTS {
        let path = root.join(name);
        if host.is_file(&path)? {
            documents.push((name, host.read_file(&path)?));
        }
    }

    Ok(documents)
}

/// Returns the gzip-compressed `git archive` of `HEAD` in the workspace
/// root, with every path below `<repository name>-<version>/`.
fn source_archive(
    host: &Host<'_>,
    workspace: &Workspace,
    plan: &ReleasePlan,
) -> Result<Vec<u8>, String> {
    let prefix = artifact::source_directory(&plan.repository.name, &plan.version);
    let command = ProcessCommand {
        args: [
            "archive".to_string(),
            "--format=tar".to_string(),
            format!("--prefix={prefix}/"),
            "HEAD".to_string(),
        ]
        .map(OsString::from)
        .to_vec(),
        current_dir: Some(workspace.root.clone()),
        program: OsString::from("git"),
    };

    archive::gzip(&host.process.output(&command)?)
}

/// Release files of one `assemble` run.
pub(crate) struct Assembly<'run> {
    /// SHA-256 of every release file so far by file name: each verified app
    /// archive, then each published file. The checksum list and the manifest
    /// cover exactly these files.
    pub(crate) sha256: BTreeMap<String, String>,
    /// Artifacts directory that every file is read from and written to.
    directory: PathBuf,
    host: &'run Host<'run>,
    plan: &'run ReleasePlan,
    /// Paths written so far, in write order.
    written: Vec<PathBuf>,
}

impl<'run> Assembly<'run> {
    /// Checks that `directory` holds every app archive `plan` names, each
    /// with a checksum file that matches it, and records their SHA-256.
    ///
    /// # Errors
    /// Returns an error listing every missing file and mismatched checksum
    /// file, or naming a file that cannot be inspected or read.
    pub(crate) fn verify(
        host: &'run Host<'run>,
        directory: &Path,
        plan: &'run ReleasePlan,
    ) -> Result<Self, String> {
        let mut sha256 = BTreeMap::new();
        let mut problems = Vec::new();
        for archive in plan.apps.iter().flat_map(|app| &app.archives) {
            let archive_path = directory.join(&archive.name);
            let checksum_path = directory.join(&archive.checksum);
            let mut complete = true;
            for (name, path) in [
                (&archive.name, &archive_path),
                (&archive.checksum, &checksum_path),
            ] {
                if !host.is_file(path)? {
                    problems.push(format!("missing `{name}`"));
                    complete = false;
                }
            }
            if !complete {
                continue;
            }
            let digest = checksum::sha256(&host.read_file(&archive_path)?);
            if !checksum::records(&host.read_file(&checksum_path)?, &digest) {
                problems.push(format!(
                    "`{}` does not match `{}`",
                    archive.checksum, archive.name
                ));
            }
            sha256.insert(archive.name.clone(), digest);
        }
        if !problems.is_empty() {
            return Err(format!(
                "`{}` does not hold every release archive with a matching checksum file:\n  {}",
                directory.display(),
                problems.join("\n  ")
            ));
        }

        Ok(Self {
            sha256,
            directory: directory.to_path_buf(),
            host,
            plan,
            written: Vec::new(),
        })
    }

    /// Writes `contents` as the release file `name` and records its SHA-256,
    /// which it returns, for the checksum list and the manifest.
    ///
    /// # Errors
    /// Returns an error when the file cannot be written.
    pub(crate) fn publish(&mut self, name: &str, contents: &[u8]) -> Result<String, String> {
        self.write(name, contents)?;
        let digest = checksum::sha256(contents);
        self.sha256.insert(name.to_string(), digest.clone());

        Ok(digest)
    }

    /// Publishes `contents` as `name` together with its `sha256sum`-compatible
    /// checksum file.
    ///
    /// # Errors
    /// Returns an error when either file cannot be written.
    pub(crate) fn publish_with_checksum(
        &mut self,
        name: &str,
        contents: &[u8],
    ) -> Result<(), String> {
        let digest = self.publish(name, contents)?;

        self.write(
            &artifact::checksum(name),
            checksum::line(&digest, name).as_bytes(),
        )
    }

    /// Writes the checksum list and the manifest, and returns every path this
    /// assembly wrote.
    ///
    /// # Errors
    /// Returns an error when either file cannot be written.
    pub(crate) fn finish(mut self) -> Result<Vec<PathBuf>, String> {
        let checksums = self
            .sha256
            .iter()
            .map(|(name, digest)| checksum::line(digest, name))
            .collect::<String>();
        self.write(artifact::CHECKSUM_LIST, checksums.as_bytes())?;
        let manifest = json!({"plan": self.plan, "sha256": self.sha256});
        self.write(artifact::MANIFEST, format!("{manifest:#}\n").as_bytes())?;

        Ok(self.written)
    }

    /// Writes `contents` to `name` in the artifacts directory.
    fn write(&mut self, name: &str, contents: &[u8]) -> Result<(), String> {
        let path = self.directory.join(name);
        self.host.write_file(&path, contents)?;
        self.written.push(path);

        Ok(())
    }
}

#[cfg(test)]
#[path = "assemble_test.rs"]
mod tests;
