//! `notes`: the title and Markdown body of a release's GitHub release.
//!
//! The body holds the release's `CHANGELOG.md` entry, then for each app its
//! install commands, a download table, and how to verify the artifacts'
//! GitHub attestations. Each app's section is rendered from
//! `src/template/release_note.md.j2`.

use std::io::Write;
use std::path::Path;

use askama::Template;
use semver::Version;

use crate::changelog::ChangelogEntry;
use crate::cli::{Host, NotesOptions};
use crate::plan::{AppPlan, ReleasePlan};
use crate::tag::ReleaseTag;
use crate::workspace::Workspace;

/// Changelog read from the workspace root.
const CHANGELOG: &str = "CHANGELOG.md";

/// Prints the release notes of `options.tag`, or only their title with
/// `options.title`.
///
/// # Errors
/// Returns an error when the tag is invalid or names another version than
/// the apps, `CHANGELOG.md` is unreadable or not UTF-8, or the output cannot
/// be written.
pub(crate) fn run(
    host: &Host<'_>,
    options: &NotesOptions,
    stdout: &mut dyn Write,
) -> Result<(), String> {
    let tag = ReleaseTag::parse(&options.tag)?;
    let workspace = Workspace::load(host.process, host.cargo.clone())?;
    let plan = ReleasePlan::new(&workspace, Some(&tag))?;
    let note = ReleaseNote {
        entry: changelog_entry(host, &workspace.root, &tag.version)?,
        plan: &plan,
        tag: &tag.name,
    };
    let text = if options.title {
        format!("{}\n", note.title())
    } else {
        note.body()
    };

    stdout
        .write_all(text.as_bytes())
        .and_then(|()| stdout.flush())
        .map_err(|error| format!("failed to write the release notes: {error}"))
}

/// Returns the entry for `version` in the workspace `root`'s changelog, or
/// `None` when the changelog is missing or has no such entry.
fn changelog_entry(
    host: &Host<'_>,
    root: &Path,
    version: &Version,
) -> Result<Option<ChangelogEntry>, String> {
    let path = root.join(CHANGELOG);
    if !host.is_file(&path)? {
        return Ok(None);
    }
    let changelog = String::from_utf8(host.read_file(&path)?)
        .map_err(|error| format!("`{}` is not UTF-8: {error}", path.display()))?;

    Ok(ChangelogEntry::find(&changelog, version))
}

/// The GitHub release notes of one release.
struct ReleaseNote<'release> {
    /// The release's changelog entry, if any.
    entry: Option<ChangelogEntry>,
    plan: &'release ReleasePlan,
    /// Release tag as given.
    tag: &'release str,
}

impl ReleaseNote<'_> {
    /// Returns the release title: `<tag> - <date>` when the changelog entry
    /// has a date, and `<tag>` otherwise.
    fn title(&self) -> String {
        match self.entry.as_ref().and_then(|entry| entry.date.as_deref()) {
            Some(date) => format!("{} - {date}", self.tag),
            None => self.tag.to_string(),
        }
    }

    /// Returns the Markdown body: `## Release Notes` with the changelog entry
    /// when it has notes, then one section per app.
    fn body(&self) -> String {
        let release_notes = self
            .entry
            .as_ref()
            .filter(|entry| !entry.notes.is_empty())
            .map_or_default(|entry| format!("## Release Notes\n\n{}\n\n", entry.notes));
        let sections = self
            .plan
            .apps
            .iter()
            .map(|app| format!("{}\n", AppNote::new(self.plan, app, self.tag)))
            .collect::<Vec<_>>();

        format!("{release_notes}{}", sections.join("\n"))
    }
}

/// One app's section of the release notes.
#[derive(Template)]
#[template(path = "release_note.md.j2", escape = "none")]
struct AppNote {
    /// Download table rows, macOS before Linux.
    downloads: Vec<Download>,
    /// `<app> <version>`.
    heading: String,
    /// npm install command when the npm installer is enabled.
    npm_command: Option<String>,
    /// `<owner>/<name>` of the GitHub repository.
    repository: String,
    repository_url: String,
    /// Shell installer command when the shell installer is enabled.
    shell_command: Option<String>,
}

impl AppNote {
    /// Returns `app`'s section for the release `tag` of `plan`.
    fn new(plan: &ReleasePlan, app: &AppPlan, tag: &str) -> Self {
        let download_url = plan.download_url(tag);
        let mut archives = app.archives.iter().collect::<Vec<_>>();
        archives.sort_by_key(|archive| (archive.target.uname_system(), archive.target));
        let repository = &plan.repository;

        Self {
            downloads: archives
                .into_iter()
                .map(|archive| Download {
                    checksum_url: format!("{download_url}/{}", archive.checksum),
                    file: archive.name.clone(),
                    file_url: format!("{download_url}/{}", archive.name),
                    platform: archive.target.display_name(),
                })
                .collect(),
            heading: format!("{} {}", app.name, plan.version),
            npm_command: app
                .npm_package
                .as_ref()
                .map(|package| format!("npm install {package}@{}", plan.version)),
            repository: format!("{}/{}", repository.owner, repository.name),
            repository_url: repository.url.clone(),
            shell_command: app.shell_installer.as_ref().map(|installer| {
                format!("curl --proto '=https' --tlsv1.2 -LsSf {download_url}/{installer} | sh")
            }),
        }
    }
}

/// One row of an app's download table.
struct Download {
    checksum_url: String,
    file: String,
    file_url: String,
    /// Platform display name of the archive's target.
    platform: &'static str,
}

#[cfg(test)]
#[path = "release_note_test.rs"]
mod tests;
