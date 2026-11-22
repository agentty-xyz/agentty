//! Check that `standards.toml` names exports that exist and that every export
//! stays portable: exported instruction sections, skills, and ast-grep rules
//! name no workspace crate, hook, or repository path that the manifest does
//! not export, and no shared rule is anchored to a repository path.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::io;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use regex::Regex;
use serde::Deserialize;
use tracing::info;

use crate::repository;

/// Manifest path, relative to the repository root.
const MANIFEST: &str = "standards.toml";

/// Manifest schema version this check understands.
const VERSION: u32 = 1;

/// Hook catalog that names this workspace's hooks.
const CATALOG: &str = ".pre-commit-config.yaml";

/// Root paths exported text may name without exporting them, because every
/// workspace following these standards keeps them: instruction guides and
/// their aliases.
const UNIVERSAL_PATHS: [&str; 2] = ["AGENTS.md", "CLAUDE.md"];

/// Validates the standards manifest using read-only Git inventory and
/// filesystem operations.
///
/// # Errors
/// Returns an error for unreadable inputs, a missing export, or an export
/// that names something only this workspace has.
pub(crate) fn run() -> Result<(), String> {
    let host = RealHost {
        git: PathBuf::from("git"),
    };
    let count = StandardCheck::new(&host)?.run(Path::new("."))?;
    info!("Standards manifest passed ({count} exported documents).");

    Ok(())
}

/// External operations used by manifest validation.
#[cfg_attr(test, mockall::automock)]
trait Host {
    fn inventory(&self, root: &Path) -> Result<Vec<PathBuf>, String>;
    fn read(&self, path: &Path) -> io::Result<String>;
}

/// Manifest policy, independent of the host filesystem and Git process.
struct StandardCheck<'host> {
    hook: Regex,
    host: &'host dyn Host,
    inline: Regex,
    literal: Regex,
}

impl<'host> StandardCheck<'host> {
    fn new(host: &'host dyn Host) -> Result<Self, String> {
        Ok(Self {
            hook: Self::pattern(r"\bprek\s+run\s+([a-z][a-z0-9-]*)")?,
            host,
            inline: Self::pattern(r"`[^`\n]+`")?,
            literal: Self::pattern(r"^[\w./-]+$")?,
        })
    }

    fn pattern(source: &str) -> Result<Regex, String> {
        Regex::new(source).map_err(|error| format!("Invalid standards pattern: {error}"))
    }

    /// Returns the number of exported documents checked.
    fn run(&self, root: &Path) -> Result<usize, String> {
        let source = self.read(root, Path::new(MANIFEST))?;
        let manifest: Manifest = toml::from_str(&source)
            .map_err(|error| format!("{MANIFEST}: invalid manifest: {error}"))?;
        let inventory: BTreeSet<PathBuf> = self.host.inventory(root)?.into_iter().collect();
        let mut errors = Vec::new();
        if manifest.version != VERSION {
            errors.push(format!(
                "{MANIFEST}: unsupported version {}; expected {VERSION}",
                manifest.version
            ));
        }
        for file in &manifest.files {
            self.read_export(root, Path::new(file), &mut errors)?;
        }
        self.check_toolchain(root, &manifest.toolchain.file, &mut errors)?;
        let crates = self.crates(root, &inventory, &manifest.lints.manifest, &mut errors)?;
        let context = Context {
            crates: Self::crate_pattern(&crates)?,
            exports: manifest.exports(),
            hooks: repository::hook_ids(&self.read(root, Path::new(CATALOG))?)?,
            roots: inventory
                .iter()
                .filter_map(|path| path.components().next())
                .map(|component| PathBuf::from(component.as_os_str()))
                .collect(),
        };
        let mut count = 0;
        for skill in &manifest.skills {
            count += self.check_skill(root, &inventory, &context, skill, &mut errors)?;
        }
        count += self.check_rules(root, &inventory, &context, &manifest.rules, &mut errors)?;
        for instructions in &manifest.instructions {
            count += self.check_instructions(root, &context, instructions, &mut errors)?;
        }
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }

        Ok(count)
    }

    fn read(&self, root: &Path, path: &Path) -> Result<String, String> {
        let source = root.join(path);

        self.host
            .read(&source)
            .map_err(|error| format!("Failed to read {}: {error}", source.display()))
    }

    /// Reads a path the manifest names, reporting it as missing instead of
    /// failing when it does not exist.
    fn read_export(
        &self,
        root: &Path,
        path: &Path,
        errors: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        let text = self.read_listed(root, path)?;
        if text.is_none() {
            errors.push(format!("{MANIFEST}: missing path {}", path.display()));
        }

        Ok(text)
    }

    /// Reads a path, returning `None` when it does not exist, such as a
    /// tracked file deleted from the working tree.
    fn read_listed(&self, root: &Path, path: &Path) -> Result<Option<String>, String> {
        let source = root.join(path);
        match self.host.read(&source) {
            Ok(text) => Ok(Some(text)),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(None)
            }
            Err(error) => Err(format!("Failed to read {}: {error}", source.display())),
        }
    }

    fn check_toolchain(
        &self,
        root: &Path,
        file: &str,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        if let Some(text) = self.read_export(root, Path::new(file), errors)?
            && !toml::from_str::<toml::Table>(&text).is_ok_and(|table| {
                table
                    .get("toolchain")
                    .and_then(|toolchain| toolchain.get("channel"))
                    .is_some_and(toml::Value::is_str)
            })
        {
            errors.push(format!("{file}: missing [toolchain] channel"));
        }

        Ok(())
    }

    /// Returns the package and library names of the workspace members that
    /// the lint manifest's `[workspace]` table declares.
    fn crates(
        &self,
        root: &Path,
        inventory: &BTreeSet<PathBuf>,
        manifest: &str,
        errors: &mut Vec<String>,
    ) -> Result<BTreeSet<String>, String> {
        let mut crates = BTreeSet::new();
        let Some(text) = self.read_export(root, Path::new(manifest), errors)? else {
            return Ok(crates);
        };
        let workspace = match toml::from_str::<WorkspaceManifest>(&text) {
            Ok(parsed) => parsed.workspace,
            Err(error) => {
                errors.push(format!("{manifest}: invalid workspace manifest: {error}"));

                return Ok(crates);
            }
        };
        if workspace.lints.is_none() {
            errors.push(format!("{manifest}: missing [workspace.lints]"));
        }
        for member in &workspace.members {
            let mut matched = false;
            for path in inventory.iter().filter(|path| {
                path.file_name().is_some_and(|name| name == "Cargo.toml")
                    && path
                        .parent()
                        .is_some_and(|directory| Self::matches_member(member, directory))
            }) {
                let Some(text) = self.read_listed(root, path)? else {
                    continue;
                };
                matched = true;
                match toml::from_str::<MemberManifest>(&text) {
                    Ok(member) => {
                        crates.insert(member.package.name.replace('-', "_"));
                        crates.insert(member.package.name);
                    }
                    Err(error) => {
                        errors.push(format!(
                            "{}: invalid member manifest: {error}",
                            path.display()
                        ));
                    }
                }
            }
            if !matched {
                errors.push(format!(
                    "{manifest}: workspace member {member} matches no manifest"
                ));
            }
        }

        Ok(crates)
    }

    /// Returns a pattern matching any of `crates` as a whole identifier, or
    /// `None` when there are none to match.
    fn crate_pattern(crates: &BTreeSet<String>) -> Result<Option<Regex>, String> {
        if crates.is_empty() {
            return Ok(None);
        }
        let mut names: Vec<_> = crates.iter().map(|name| regex::escape(name)).collect();
        names.sort_by_key(|name| Reverse(name.len()));

        Self::pattern(&format!(r"\b(?:{})\b", names.join("|"))).map(Some)
    }

    /// Returns whether `directory` matches a workspace member pattern whose
    /// `*` components match any single component.
    fn matches_member(pattern: &str, directory: &Path) -> bool {
        let mut components = directory.components();
        for expected in pattern.split('/') {
            match components.next() {
                Some(Component::Normal(actual)) if expected == "*" || actual == expected => {}
                _ => return false,
            }
        }

        components.next().is_none()
    }

    fn check_skill(
        &self,
        root: &Path,
        inventory: &BTreeSet<PathBuf>,
        context: &Context,
        skill: &str,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let directory = Path::new("skills").join(skill);
        if self
            .read_export(root, &directory.join("SKILL.md"), errors)?
            .is_none()
        {
            return Ok(0);
        }

        self.check_listed(root, inventory, context, &directory, errors)
    }

    /// Checks every existing file under `directory` and returns their count.
    fn check_listed(
        &self,
        root: &Path,
        inventory: &BTreeSet<PathBuf>,
        context: &Context,
        directory: &Path,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let mut count = 0;
        for path in inventory.iter().filter(|path| path.starts_with(directory)) {
            if let Some(text) = self.read_listed(root, path)? {
                self.check_text(context, path, &text, 0..text.len(), errors);
                count += 1;
            }
        }

        Ok(count)
    }

    fn check_rules(
        &self,
        root: &Path,
        inventory: &BTreeSet<PathBuf>,
        context: &Context,
        rules: &Rules,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let directory = Path::new(&rules.directory);
        let tests = Path::new(&rules.tests);
        let mut ids = BTreeSet::new();
        for path in inventory.iter().filter(|path| path.starts_with(directory)) {
            let Some(text) = self.read_listed(root, path)? else {
                continue;
            };
            self.check_text(context, path, &text, 0..text.len(), errors);
            match serde_yaml_ng::from_str::<RuleFile>(&text) {
                Ok(rule) => {
                    Self::check_rule_scope(path, &rule, errors);
                    if path.file_stem().is_none_or(|stem| *stem != *rule.id) {
                        errors.push(format!(
                            "{}: rule id {} must match the file name",
                            path.display(),
                            rule.id
                        ));
                    }
                    ids.insert(rule.id);
                }
                Err(error) => errors.push(format!("{}: invalid rule: {error}", path.display())),
            }
        }
        if ids.is_empty() {
            errors.push(format!(
                "{MANIFEST}: rules directory {} holds no rules",
                directory.display()
            ));
        }
        let mut tested = BTreeSet::new();
        for path in inventory.iter().filter(|path| path.starts_with(tests)) {
            let Some(text) = self.read_listed(root, path)? else {
                continue;
            };
            self.check_text(context, path, &text, 0..text.len(), errors);
            let id = path
                .file_name()
                .and_then(OsStr::to_str)
                .and_then(|name| name.strip_suffix("-test.yml"));
            match id {
                Some(id) if ids.contains(id) => {
                    tested.insert(id.to_owned());
                }
                _ => errors.push(format!(
                    "{}: tests no rule in {}",
                    path.display(),
                    directory.display()
                )),
            }
        }
        for id in ids.difference(&tested) {
            errors.push(format!(
                "{}: rule {id} has no {id}-test.yml",
                tests.display()
            ));
        }

        Ok(ids.len() + tested.len())
    }

    /// Reports `files` and `ignores` globs that start at a concrete
    /// repository path instead of matching by pattern.
    fn check_rule_scope(path: &Path, rule: &RuleFile, errors: &mut Vec<String>) {
        for glob in rule.files.iter().chain(&rule.ignores) {
            let first = glob.split('/').next().unwrap_or_default();
            if !first.contains(['*', '?', '[', '{']) {
                errors.push(format!(
                    "{}: glob {glob} starts at the repository path {first}; match shared rules by \
                     pattern, such as **/src/**/*.rs, or move the rule out of the shared directory",
                    path.display()
                ));
            }
        }
    }

    fn check_instructions(
        &self,
        root: &Path,
        context: &Context,
        instructions: &Instructions,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let document = Path::new(&instructions.file);
        let Some(text) = self.read_export(root, document, errors)? else {
            return Ok(0);
        };
        let headings = Self::headings(&text);
        let mut count = 0;
        for section in &instructions.sections {
            let matching: Vec<_> = headings
                .iter()
                .enumerate()
                .filter(|(_, heading)| heading.title == *section)
                .collect();
            let [(index, heading)] = matching.as_slice() else {
                let problem = if matching.is_empty() {
                    "missing"
                } else {
                    "ambiguous"
                };
                errors.push(format!(
                    "{}: {problem} section heading {section}",
                    document.display()
                ));
                continue;
            };
            let end = headings[index + 1..]
                .iter()
                .find(|next| next.level <= heading.level)
                .map_or(text.len(), |next| next.start);
            self.check_text(context, document, &text, heading.start..end, errors);
            count += 1;
        }

        Ok(count)
    }

    fn headings(text: &str) -> Vec<Heading> {
        let mut headings = Vec::new();
        let mut current = None;
        for (event, range) in Parser::new(text).into_offset_iter() {
            match event {
                Event::Start(Tag::Heading { level, .. }) => {
                    current = Some(Heading {
                        level,
                        start: range.start,
                        title: String::new(),
                    });
                }
                Event::Text(part) | Event::Code(part) => {
                    if let Some(heading) = current.as_mut() {
                        heading.title.push_str(&part);
                    }
                }
                Event::End(TagEnd::Heading(_)) => headings.extend(current.take()),
                _ => {}
            }
        }

        headings
    }

    /// Reports workspace crates, hooks, and unexported repository paths that
    /// `text[range]` names.
    fn check_text(
        &self,
        context: &Context,
        document: &Path,
        text: &str,
        range: Range<usize>,
        errors: &mut Vec<String>,
    ) {
        let section = &text[range.clone()];
        // Crate names count anywhere, not only in code spans, so rule
        // patterns and rule-test snippets cannot name them either.
        for matched in context
            .crates
            .iter()
            .flat_map(|crates| crates.find_iter(section))
        {
            let start = range.start + matched.start();
            let end = range.start + matched.end();
            if !text[..start].ends_with('-') && !text[end..].starts_with('-') {
                Self::report(
                    errors,
                    document,
                    text,
                    start,
                    &format!("names workspace crate {}", matched.as_str()),
                );
            }
        }
        for matched in self.inline.find_iter(section) {
            let start = range.start + matched.start();
            let end = range.start + matched.end();
            if text[..start].ends_with('`') || text[end..].starts_with('`') {
                continue;
            }
            let target = &text[start + 1..end - 1];
            let problem = if context.hooks.contains(target) {
                format!("names workspace hook {target}")
            } else if self.literal.is_match(target) && context.is_unexported_path(target) {
                format!("names {target}, which {MANIFEST} does not export")
            } else {
                continue;
            };
            Self::report(errors, document, text, start, &problem);
        }
        for matched in self
            .hook
            .captures_iter(section)
            .filter_map(|capture| capture.get(1))
            .filter(|matched| context.hooks.contains(matched.as_str()))
        {
            Self::report(
                errors,
                document,
                text,
                range.start + matched.start(),
                &format!("names workspace hook {}", matched.as_str()),
            );
        }
    }

    fn report(errors: &mut Vec<String>, document: &Path, text: &str, offset: usize, message: &str) {
        let line = text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1;
        errors.push(format!("{}:{line}: {message}", document.display()));
    }
}

/// Facts about this workspace that exported text must not name.
struct Context {
    crates: Option<Regex>,
    exports: Vec<PathBuf>,
    hooks: BTreeSet<String>,
    roots: BTreeSet<PathBuf>,
}

impl Context {
    /// Returns whether `target` is a repository path, judged by its first
    /// component, that is neither exported nor universal.
    fn is_unexported_path(&self, target: &str) -> bool {
        let path = Path::new(target.trim_start_matches("./"));

        path.components()
            .next()
            .is_some_and(|first| self.roots.contains(Path::new(first.as_os_str())))
            && !UNIVERSAL_PATHS
                .iter()
                .any(|universal| path == Path::new(universal))
            && !self.exports.iter().any(|export| path.starts_with(export))
    }
}

/// `standards.toml` schema.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    files: Vec<String>,
    instructions: Vec<Instructions>,
    lints: Lints,
    rules: Rules,
    skills: Vec<String>,
    toolchain: Toolchain,
    version: u32,
}

impl Manifest {
    /// Returns every path a consumer copies or reads, so exported text may
    /// name it or anything below it.
    fn exports(&self) -> Vec<PathBuf> {
        self.files
            .iter()
            .chain([
                &self.toolchain.file,
                &self.lints.manifest,
                &self.rules.directory,
                &self.rules.tests,
            ])
            .map(PathBuf::from)
            .chain(
                self.skills
                    .iter()
                    .map(|skill| Path::new("skills").join(skill)),
            )
            .collect()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Instructions {
    file: String,
    sections: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Lints {
    manifest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Rules {
    directory: String,
    tests: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Toolchain {
    file: String,
}

/// Root manifest structure that names the members and shared lints.
#[derive(Deserialize)]
struct WorkspaceManifest {
    workspace: Workspace,
}

#[derive(Deserialize)]
struct Workspace {
    lints: Option<toml::Table>,
    #[serde(default)]
    members: Vec<String>,
}

#[derive(Deserialize)]
struct MemberManifest {
    package: Package,
}

#[derive(Deserialize)]
struct Package {
    name: String,
}

/// ast-grep rule fields that decide its identity and path scope.
#[derive(Deserialize)]
struct RuleFile {
    #[serde(default)]
    files: Vec<String>,
    id: String,
    #[serde(default)]
    ignores: Vec<String>,
}

/// A Markdown heading and where its section starts.
struct Heading {
    level: HeadingLevel,
    start: usize,
    title: String,
}

/// Host adapter; policy stays in `StandardCheck`.
struct RealHost {
    git: PathBuf,
}

impl Host for RealHost {
    fn inventory(&self, root: &Path) -> Result<Vec<PathBuf>, String> {
        repository::git_files(&self.git, root)
    }

    fn read(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

#[cfg(test)]
#[path = "check_standard_test.rs"]
mod tests;
