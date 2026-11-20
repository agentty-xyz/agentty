//! `prepare`: bumps the version of a release and adds its changelog heading.
//!
//! Every workspace member at the apps' version moves to the new version, in
//! its own `[package]` table or the root `[workspace.package]` it inherits
//! from, and so does each `version` requirement of a path dependency on one
//! of them. Manifests are edited in place at the spans `toml` reports, so
//! formatting and comments survive. `cargo update --workspace` then refreshes
//! `Cargo.lock`, and `CHANGELOG.md` gains the release's heading.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::io::Write;
use std::ops::Range;
use std::path::Path;

use semver::Version;
use toml::Spanned;
use toml::de::{DeTable, DeValue};

use crate::changelog::{self, ChangelogEntry};
use crate::cli::{Host, PrepareOptions};
use crate::process::ProcessCommand;
use crate::workspace::Workspace;

/// Dependency tables of a manifest or of one of its `[target.<cfg>]` tables.
const DEPENDENCY_TABLES: [&str; 3] = ["dependencies", "dev-dependencies", "build-dependencies"];

/// Requirement operators kept in front of a bumped version requirement; the
/// empty operator, Cargo's default caret, comes last.
const REQUIREMENT_OPERATORS: [&str; 4] = ["=", "^", "~", ""];

/// Bumps the workspace to `options.version`, refreshes `Cargo.lock`, adds the
/// changelog heading dated `options.date`, and prints the written paths.
///
/// # Errors
/// Returns an error when the version or date is invalid, the version is not
/// newer than the apps', a manifest or the changelog cannot be read, parsed,
/// or written, or `cargo update` fails.
pub(crate) fn run(
    host: &Host<'_>,
    options: &PrepareOptions,
    stdout: &mut dyn Write,
) -> Result<(), String> {
    let version = Version::parse(
        options
            .version
            .strip_prefix('v')
            .unwrap_or(&options.version),
    )
    .map_err(|error| format!("`{}` is not a semver version: {error}", options.version))?;
    check_date(&options.date)?;
    let workspace = Workspace::load(host.process, host.cargo.clone())?;
    if version <= workspace.version {
        return Err(format!(
            "version {version} must be newer than the apps' version {}",
            workspace.version
        ));
    }
    let current = workspace.version.to_string();
    let bump = VersionBump {
        current: current.clone(),
        new: version.to_string(),
        packages: workspace
            .members
            .iter()
            .filter(|member| member.version == current)
            .map(|member| member.name.clone())
            .collect(),
    };
    let mut manifests = vec![workspace.root.join("Cargo.toml")];
    for member in &workspace.members {
        if !manifests.contains(&member.manifest_path) {
            manifests.push(member.manifest_path.clone());
        }
    }
    let mut edited = Vec::new();
    for path in manifests {
        let text = read_text(host, &path)?;
        let bumped = bump.edit(&text, &path)?;
        if bumped != text {
            edited.push((path, bumped));
        }
    }
    let mut written = Vec::new();
    for (path, text) in edited {
        host.write_file(&path, text.as_bytes())?;
        written.push(path);
    }
    host.process.stream(&ProcessCommand {
        args: ["update", "--workspace"].map(OsString::from).to_vec(),
        current_dir: Some(workspace.root.clone()),
        program: host.cargo.clone(),
    })?;
    written.push(workspace.root.join("Cargo.lock"));
    let changelog_path = workspace.root.join(changelog::FILE);
    if host.is_file(&changelog_path)? {
        let text = read_text(host, &changelog_path)?;
        if let Some(text) = ChangelogEntry::insert_heading(&text, &version, &options.date) {
            host.write_file(&changelog_path, text.as_bytes())?;
            written.push(changelog_path);
        }
    }

    Host::report_written(stdout, &written)
}

/// Accepts a `YYYY-MM-DD` date with a month from 1 to 12 and a day from 1
/// to 31.
fn check_date(date: &str) -> Result<(), String> {
    let number = |part: &str, length: usize, range: Range<u32>| {
        part.len() == length
            && part.bytes().all(|byte| byte.is_ascii_digit())
            && part
                .parse::<u32>()
                .is_ok_and(|value| range.contains(&value))
    };
    let mut parts = date.split('-');
    let valid = matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(year), Some(month), Some(day), None)
            if number(year, 4, 0..10_000) && number(month, 2, 1..13) && number(day, 2, 1..32)
    );
    if !valid {
        return Err(format!(
            "`--date` must be a `YYYY-MM-DD` date, found `{date}`"
        ));
    }

    Ok(())
}

/// Returns the UTF-8 contents of the file at `path`.
fn read_text(host: &Host<'_>, path: &Path) -> Result<String, String> {
    String::from_utf8(host.read_file(path)?)
        .map_err(|error| format!("`{}` is not UTF-8: {error}", path.display()))
}

/// One version bump of the packages at the current version.
struct VersionBump {
    current: String,
    new: String,
    /// Names of the packages that move to the new version.
    packages: BTreeSet<String>,
}

impl VersionBump {
    /// Returns the manifest `text` with the bumped package versions and
    /// dependency requirements.
    ///
    /// # Errors
    /// Returns an error naming `path` when `text` is not TOML.
    fn edit(&self, text: &str, path: &Path) -> Result<String, String> {
        let document = DeTable::parse(text)
            .map_err(|error| format!("failed to parse `{}`: {error}", path.display()))?;
        let root = document.get_ref();
        let workspace = Self::table(root, "workspace");
        let mut edits = Vec::new();
        for package in [
            Self::table(root, "package"),
            workspace.and_then(|workspace| Self::table(workspace, "package")),
        ]
        .into_iter()
        .flatten()
        {
            if let Some(version) = Self::entry(package, "version")
                && Self::string(version) == Some(self.current.as_str())
            {
                edits.push((version.span(), self.new.clone()));
            }
        }
        let mut dependency_tables = workspace
            .and_then(|workspace| Self::table(workspace, "dependencies"))
            .into_iter()
            .collect::<Vec<_>>();
        let targets = Self::table(root, "target")
            .into_iter()
            .flat_map(|targets| targets.values())
            .filter_map(|target| match target.get_ref() {
                DeValue::Table(target) => Some(target),
                _ => None,
            });
        for table in std::iter::once(root).chain(targets) {
            dependency_tables.extend(
                DEPENDENCY_TABLES
                    .iter()
                    .filter_map(|name| Self::table(table, name)),
            );
        }
        for dependencies in dependency_tables {
            edits.extend(self.dependency_edits(dependencies));
        }
        edits.sort_by_key(|(span, _)| span.start);
        let mut bumped = text.to_string();
        for (span, version) in edits.into_iter().rev() {
            bumped.replace_range(span, &format!("\"{version}\""));
        }

        Ok(bumped)
    }

    /// Returns the version requirement edits of the path dependencies in
    /// `dependencies` on a bumped package whose requirement names the current
    /// version, keeping its operator.
    fn dependency_edits(&self, dependencies: &DeTable<'_>) -> Vec<(Range<usize>, String)> {
        dependencies
            .iter()
            .filter_map(|(name, specification)| {
                let DeValue::Table(specification) = specification.get_ref() else {
                    return None;
                };
                Self::entry(specification, "path")?;
                let package = Self::entry(specification, "package")
                    .and_then(Self::string)
                    .unwrap_or(name.get_ref());
                if !self.packages.contains(package) {
                    return None;
                }
                let version = Self::entry(specification, "version")?;
                let requirement = Self::string(version)?.trim();
                let operator = REQUIREMENT_OPERATORS.iter().find(|operator| {
                    requirement
                        .strip_prefix(**operator)
                        .is_some_and(|rest| rest.trim() == self.current)
                })?;

                Some((version.span(), format!("{operator}{}", self.new)))
            })
            .collect()
    }

    /// Returns the value at `key` in `table`.
    fn entry<'table, 'input>(
        table: &'table DeTable<'input>,
        key: &str,
    ) -> Option<&'table Spanned<DeValue<'input>>> {
        table
            .iter()
            .find(|(name, _)| name.get_ref() == key)
            .map(|(_, value)| value)
    }

    /// Returns the table at `key` in `table`, if it is one.
    fn table<'table, 'input>(
        table: &'table DeTable<'input>,
        key: &str,
    ) -> Option<&'table DeTable<'input>> {
        match Self::entry(table, key)?.get_ref() {
            DeValue::Table(table) => Some(table),
            _ => None,
        }
    }

    /// Returns `value` when it is a string.
    fn string<'value>(value: &'value Spanned<DeValue<'_>>) -> Option<&'value str> {
        match value.get_ref() {
            DeValue::String(text) => Some(text),
            _ => None,
        }
    }
}

#[cfg(test)]
#[path = "prepare_test.rs"]
mod tests;
