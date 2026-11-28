//! Check that `standards.toml` names exports that exist and that every export
//! stays portable: exported instruction sections, skills, ast-grep rules, and
//! the design guide, stylesheets, token file, and text assets name no
//! workspace crate, hook, or repository path that the manifest does not
//! export, no shared rule is anchored to a repository path, no design
//! stylesheet uses Sass or imports another file, no design export loads
//! anything but an exported file beside it, and the protocol schema and its
//! TypeScript types reference nothing outside themselves and name one
//! protocol version.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::ops::Range;
use std::path::{Component, Path, PathBuf};
use std::{io, iter};

use pulldown_cmark::{Event, HeadingLevel, Parser, Tag, TagEnd};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use tracing::info;

use crate::repository;

/// Manifest path, relative to the repository root.
const MANIFEST: &str = "standards.toml";

/// Manifest schema version this check understands. Version 2 added the
/// required `[design]` table, version 3 its `tokens` and `assets` keys, and
/// version 4 the required `[protocol]` table.
const VERSION: u32 = 4;

/// Largest design asset, in bytes, so a consumer copying the design export
/// never vendors a heavy file by accident.
const ASSET_LIMIT: usize = 256 * 1024;

/// Largest protocol file, in bytes, with room for the protocol to grow while
/// keeping a generated file a consumer can still review when it moves its pin.
const PROTOCOL_LIMIT: usize = 512 * 1024;

/// The only JSON Schema dialect an exported protocol schema declares, and
/// only at its root.
const SCHEMA_DIALECT: &str = "https://json-schema.org/draft/2020-12/schema";

/// JSON Schema keywords that give a schema a base URI or a name, so a
/// reference could resolve somewhere other than a JSON pointer into the file.
const SCHEMA_IDENTIFIERS: [&str; 4] = ["$anchor", "$dynamicAnchor", "$id", "$recursiveAnchor"];

/// JSON Schema keywords whose string value references another schema.
const SCHEMA_REFERENCES: [&str; 3] = ["$dynamicRef", "$recursiveRef", "$ref"];

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
    /// Returns whether `path` is a regular file rather than a symbolic link,
    /// directory, or other entry, without following a link.
    fn is_file(&self, path: &Path) -> io::Result<bool>;
    fn read(&self, path: &Path) -> io::Result<String>;
    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>>;
}

/// Manifest policy, independent of the host filesystem and Git process.
struct StandardCheck<'host> {
    at_rule: Regex,
    attribute: Regex,
    directive: Regex,
    hook: Regex,
    host: &'host dyn Host,
    import: Regex,
    inline: Regex,
    lexeme: Regex,
    literal: Regex,
    module_load: Regex,
    protocol_version: Regex,
    script: Regex,
    script_load: Regex,
    tag: Regex,
    url: Regex,
}

impl<'host> StandardCheck<'host> {
    fn new(host: &'host dyn Host) -> Result<Self, String> {
        Ok(Self {
            at_rule: Self::pattern(r"@(?:import|use|forward|mixin|include|extend)\b")?,
            attribute: Self::pattern(
                r#"(?i)(?:^|\s)((?:xlink:)?href|src|srcset|poster|data|ping|on[a-z]+)\s*=\s*("[^"]*"|'[^']*'|[^\s"'>]+)"#,
            )?,
            directive: Self::pattern(r"(?m)^[ \t]*///[ \t]*<")?,
            hook: Self::pattern(r"\bprek\s+run\s+([a-z][a-z0-9-]*)")?,
            host,
            import: Self::pattern(r#"@import\s+["']([^"']*)"#)?,
            inline: Self::pattern(r"`[^`\n]+`")?,
            lexeme: Self::pattern(
                r#""(?:[^"\\\n]|\\.)*"|'(?:[^'\\\n]|\\.)*'|(?s:/\*.*?\*/)|//[^\n]*"#,
            )?,
            literal: Self::pattern(r"^[\w./-]+$")?,
            module_load: Self::pattern(
                r#"(?:\bfrom|\bimport|\brequire)\s*\(?\s*["']([^"']*)["']"#,
            )?,
            protocol_version: Self::pattern(
                r"\bexport\s+type\s+ProtocolVersion\s*=\s*(0|[1-9][0-9]*)\s*;",
            )?,
            script: Self::pattern(r"(?is)<script\b.*?</script>")?,
            script_load: Self::pattern(
                r#"(?:\bfrom|\bimport|\bfetch\s*\()\s*\(?\s*["'`]([^"'`]*)["'`]"#,
            )?,
            tag: Self::pattern(r#"<([a-zA-Z][\w:-]*)\b((?:[^>"']|"[^"]*"|'[^']*')*)>"#)?,
            url: Self::pattern(r#"\burl\(\s*['"]?([^'")\s]*)"#)?,
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
        count += self.check_design(root, &context, &manifest.design, &mut errors)?;
        count += self.check_protocol(root, &inventory, &manifest.protocol, &mut errors)?;
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
        Ok(Self::found(path, self.read_listed(root, path)?, errors))
    }

    /// Returns the content read from an exported `path`, reporting the path
    /// as missing when there is none.
    fn found<T>(path: &Path, content: Option<T>, errors: &mut Vec<String>) -> Option<T> {
        if content.is_none() {
            errors.push(format!("{MANIFEST}: missing path {}", path.display()));
        }

        content
    }

    /// Reads a path, returning `None` when it does not exist, such as a
    /// tracked file deleted from the working tree.
    fn read_listed(&self, root: &Path, path: &Path) -> Result<Option<String>, String> {
        let source = root.join(path);

        Self::existing(&source, self.host.read(&source))
    }

    /// Maps a read of `source` that found nothing to `None`.
    fn existing<T>(source: &Path, read: io::Result<T>) -> Result<Option<T>, String> {
        match read {
            Ok(content) => Ok(Some(content)),
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

    /// Checks the design guide, stylesheets, token file, and assets and
    /// returns how many were checked.
    fn check_design(
        &self,
        root: &Path,
        context: &Context,
        design: &Design,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let mut listed = BTreeSet::new();
        for path in [&design.guide, &design.tokens]
            .into_iter()
            .chain(&design.files)
            .chain(&design.assets)
        {
            if !listed.insert(path) {
                errors.push(format!(
                    "{MANIFEST}: design export {path} is listed more than once"
                ));
            }
        }
        let mut count = 0;
        let guide = Path::new(&design.guide);
        if let Some(text) = self.read_export(root, guide, errors)? {
            self.check_text(context, guide, &text, 0..text.len(), errors);
            count += 1;
        }
        for file in &design.files {
            let path = Path::new(file);
            let Some(text) = self.read_export(root, path, errors)? else {
                continue;
            };
            if path.extension().is_none_or(|extension| extension != "css") {
                errors.push(format!(
                    "{file}: a design stylesheet must be a plain .css file"
                ));
            }
            self.check_text(context, path, &text, 0..text.len(), errors);
            self.check_stylesheet(context, path, &text, errors);
            count += 1;
        }
        let tokens = Path::new(&design.tokens);
        if let Some(text) = self.read_export(root, tokens, errors)? {
            let is_group = serde_json::from_str::<serde_json::Value>(&text)
                .is_ok_and(|value| value.is_object());
            if tokens
                .extension()
                .is_none_or(|extension| extension != "json")
                || !is_group
            {
                errors.push(format!(
                    "{}: the design token file must be a .json file whose root is a token group",
                    design.tokens
                ));
            }
            self.check_text(context, tokens, &text, 0..text.len(), errors);
            count += 1;
        }
        for asset in &design.assets {
            count += usize::from(self.check_asset(root, context, Path::new(asset), errors)?);
        }

        Ok(count)
    }

    /// Reports Sass and import at-rules, and `url()` targets that leave the
    /// export, so the stylesheet works on its own when copied.
    fn check_stylesheet(
        &self,
        context: &Context,
        path: &Path,
        text: &str,
        errors: &mut Vec<String>,
    ) {
        for matched in self.at_rule.find_iter(text) {
            Self::report(
                errors,
                path,
                text,
                matched.start(),
                &format!(
                    "uses {}; an exported stylesheet is plain CSS that stands alone",
                    matched.as_str()
                ),
            );
        }
        let reach = Reach::Siblings("an exported stylesheet");
        for target in self
            .url
            .captures_iter(text)
            .filter_map(|capture| capture.get(1))
        {
            Self::check_target(
                context,
                path,
                text,
                target.start(),
                target.as_str(),
                reach,
                errors,
            );
        }
    }

    /// Checks one design asset and returns whether it exists. Every asset
    /// stays within the size limit; a text asset names nothing only this
    /// workspace has; an HTML page loads only exported files beside it; and
    /// an SVG holds no script and references only itself.
    fn check_asset(
        &self,
        root: &Path,
        context: &Context,
        path: &Path,
        errors: &mut Vec<String>,
    ) -> Result<bool, String> {
        let source = root.join(path);
        let bytes = Self::existing(&source, self.host.read_bytes(&source))?;
        let Some(bytes) = Self::found(path, bytes, errors) else {
            return Ok(false);
        };
        if bytes.len() > ASSET_LIMIT {
            errors.push(format!(
                "{}: {} bytes is over the {ASSET_LIMIT}-byte limit for a design asset",
                path.display(),
                bytes.len()
            ));
        }
        let extension = path
            .extension()
            .and_then(OsStr::to_str)
            .map(str::to_ascii_lowercase);
        let kind = match extension.as_deref() {
            Some("css") => {
                errors.push(format!(
                    "{}: list a stylesheet under files, in load order, not under assets",
                    path.display()
                ));

                return Ok(true);
            }
            Some("htm" | "html") => Some(Reach::Siblings("an exported page")),
            Some("svg") => Some(Reach::Itself),
            _ => None,
        };
        match (String::from_utf8(bytes), kind) {
            (Ok(text), kind) => {
                self.check_text(context, path, &text, 0..text.len(), errors);
                if let Some(reach) = kind {
                    self.check_markup(context, path, &text, reach, errors);
                }
            }
            (Err(_), Some(_)) => errors.push(format!(
                "{}: an HTML or SVG asset must be UTF-8 text",
                path.display()
            )),
            // A binary asset, such as an icon, is only size-bounded.
            (Err(_), None) => {}
        }

        Ok(true)
    }

    /// Reports what an HTML or SVG asset loads beyond `reach`: element
    /// `src`, `srcset`, `href`, `poster`, `data`, and `ping` targets other
    /// than link destinations, stylesheet `url()` and `@import` targets, and
    /// the modules and files its scripts and event handlers load. An SVG may
    /// hold no script or event handler at all.
    fn check_markup(
        &self,
        context: &Context,
        path: &Path,
        text: &str,
        reach: Reach,
        errors: &mut Vec<String>,
    ) {
        let mut scripts: Vec<_> = self
            .script
            .find_iter(text)
            .map(|script| script.range())
            .collect();
        let tags = self
            .tag
            .captures_iter(text)
            .filter_map(|tag| Some((tag.get(1)?, tag.get(2)?)));
        for (name, attributes) in tags {
            if reach == Reach::Itself && name.as_str().eq_ignore_ascii_case("script") {
                Self::report(
                    errors,
                    path,
                    text,
                    name.start(),
                    "holds a script; an exported SVG draws without one",
                );
            }
            let values = self
                .attribute
                .captures_iter(attributes.as_str())
                .filter_map(|attribute| Some((attribute.get(1)?, attribute.get(2)?)));
            for (key, raw) in values {
                let key = key.as_str().to_ascii_lowercase();
                let start = attributes.start() + raw.start();
                let value = raw.as_str().trim_matches(['"', '\'']);
                if key.starts_with("on") {
                    if reach == Reach::Itself {
                        Self::report(
                            errors,
                            path,
                            text,
                            start,
                            &format!("holds a script in {key}; an exported SVG draws without one"),
                        );
                    }
                    scripts.push(start..start + raw.len());
                    continue;
                }
                let is_link = reach != Reach::Itself
                    && key == "href"
                    && name.as_str().eq_ignore_ascii_case("a")
                    && Self::is_navigation(value);
                let targets: Vec<&str> = match key.as_str() {
                    _ if is_link => Vec::new(),
                    "ping" => value.split_whitespace().collect(),
                    "srcset" => value
                        .split(',')
                        .filter_map(|candidate| candidate.split_whitespace().next())
                        .collect(),
                    _ => vec![value.trim()],
                };
                for target in targets {
                    Self::check_target(context, path, text, start, target, reach, errors);
                }
            }
        }
        let loads = self
            .url
            .captures_iter(text)
            .chain(self.import.captures_iter(text))
            .chain(self.script_load.captures_iter(text).filter(|load| {
                scripts
                    .iter()
                    .any(|script| script.contains(&load.get_match().start()))
            }))
            .filter_map(|capture| capture.get(1));
        for target in loads {
            Self::check_target(
                context,
                path,
                text,
                target.start(),
                target.as_str(),
                reach,
                errors,
            );
        }
    }

    /// Returns whether a link destination only navigates: a web or mail
    /// address, or a relative one with no colon, character reference, or
    /// leading space. Anything else, such as a `javascript:` URL however it
    /// is spaced or encoded, counts as a load.
    fn is_navigation(value: &str) -> bool {
        let value = value.to_ascii_lowercase();

        ["http://", "https://", "mailto:"]
            .iter()
            .any(|scheme| value.starts_with(scheme))
            || !(value.contains([':', '&']) || value.starts_with(char::is_whitespace))
    }

    /// Reports `target`, found at `offset` in `path`, unless it is empty, a
    /// fragment, a `data:` URI, or, where `reach` allows, an exported file
    /// beside `path`.
    fn check_target(
        context: &Context,
        path: &Path,
        text: &str,
        offset: usize,
        target: &str,
        reach: Reach,
        errors: &mut Vec<String>,
    ) {
        if target.is_empty() || target.starts_with('#') || target.starts_with("data:") {
            return;
        }
        let problem = match reach {
            Reach::Itself => {
                "an exported SVG references only its own fragments and data: URIs".to_owned()
            }
            Reach::Siblings(export) => {
                if context.is_exported_sibling(path, target) {
                    return;
                }

                format!("{export} loads only data: URIs, fragments, and exported files beside it")
            }
        };
        Self::report(
            errors,
            path,
            text,
            offset,
            &format!("loads {target}; {problem}"),
        );
    }

    /// Checks the protocol schema and its TypeScript types and returns how
    /// many were checked. Each is a regular UTF-8 file that Git lists, within
    /// the size limit, that references nothing outside itself, and both name
    /// the same protocol version. They describe this workspace's own
    /// protocol, so they may name the crate they are generated from.
    fn check_protocol(
        &self,
        root: &Path,
        inventory: &BTreeSet<PathBuf>,
        protocol: &Protocol,
        errors: &mut Vec<String>,
    ) -> Result<usize, String> {
        let schema_path = Path::new(&protocol.schema);
        let types_path = Path::new(&protocol.types);
        let schema =
            self.read_protocol(root, inventory, schema_path, ("schema", "json"), errors)?;
        let types = self.read_protocol(root, inventory, types_path, ("types", "ts"), errors)?;
        let schema_version = schema
            .as_deref()
            .and_then(|text| Self::check_schema(schema_path, text, errors));
        let types_version = types
            .as_deref()
            .and_then(|text| self.check_types(types_path, text, errors));
        if let (Some(schema_version), Some(types_version)) = (schema_version, types_version)
            && schema_version != types_version
        {
            errors.push(format!(
                "{}: ProtocolVersion {types_version} differs from x-protocol-version \
                 {schema_version} in {}",
                protocol.types, protocol.schema
            ));
        }

        Ok(usize::from(schema.is_some()) + usize::from(types.is_some()))
    }

    /// Reads one protocol file, the `kind` of file a consumer expects with
    /// that extension, and returns its text when it is a regular UTF-8 file
    /// that Git lists. Git lists a linked directory as one entry and never
    /// the files reached through it, so the inventory rejects a path behind
    /// one, and an ignored file.
    fn read_protocol(
        &self,
        root: &Path,
        inventory: &BTreeSet<PathBuf>,
        path: &Path,
        (kind, extension): (&str, &str),
        errors: &mut Vec<String>,
    ) -> Result<Option<String>, String> {
        let source = root.join(path);
        let is_file = Self::existing(&source, self.host.is_file(&source))?;
        let Some(is_file) = Self::found(path, is_file, errors) else {
            return Ok(None);
        };
        if !is_file {
            errors.push(format!(
                "{}: a protocol file must be a regular file, not a link or directory",
                path.display()
            ));

            return Ok(None);
        }
        if !inventory.contains(path) {
            errors.push(format!(
                "{}: Git does not list this protocol file; it must not be ignored or lie behind a \
                 linked directory",
                path.display()
            ));

            return Ok(None);
        }
        if path.extension().is_none_or(|actual| actual != extension) {
            errors.push(format!(
                "{}: the protocol {kind} must be a .{extension} file",
                path.display()
            ));
        }
        let bytes = self
            .host
            .read_bytes(&source)
            .map_err(|error| format!("Failed to read {}: {error}", source.display()))?;
        if bytes.len() > PROTOCOL_LIMIT {
            errors.push(format!(
                "{}: {} bytes is over the {PROTOCOL_LIMIT}-byte limit for a protocol file",
                path.display(),
                bytes.len()
            ));
        }
        let Ok(text) = String::from_utf8(bytes) else {
            errors.push(format!(
                "{}: a protocol file must be UTF-8 text",
                path.display()
            ));

            return Ok(None);
        };
        if text.starts_with('\u{feff}') {
            errors.push(format!(
                "{}: starts with a byte order mark; a protocol file is plain UTF-8 text",
                path.display()
            ));

            return Ok(None);
        }

        Ok(Some(text))
    }

    /// Checks that the protocol schema is a JSON object whose references all
    /// point into itself, and returns its `x-protocol-version`.
    fn check_schema(path: &Path, text: &str, errors: &mut Vec<String>) -> Option<u64> {
        let schema = match serde_json::from_str::<Value>(text) {
            Ok(schema) if schema.is_object() => schema,
            Ok(_) => {
                errors.push(format!(
                    "{}: the protocol schema's root must be a schema object",
                    path.display()
                ));

                return None;
            }
            Err(error) => {
                errors.push(format!(
                    "{}: the protocol schema is not valid JSON: {error}",
                    path.display()
                ));

                return None;
            }
        };
        Self::check_keywords(path, &schema, &schema, true, errors);
        let version = schema.get("x-protocol-version").and_then(Value::as_u64);
        if version.is_none() {
            errors.push(format!(
                "{}: the protocol schema must name its version in an integer x-protocol-version",
                path.display()
            ));
        }

        version
    }

    /// Reports the keywords in `value`, the root of `schema` when `is_root`,
    /// that could make a reference leave the file: any identifier or anchor,
    /// a `$schema` other than [`SCHEMA_DIALECT`] at the root, and a string
    /// reference that is not a JSON pointer fragment resolving within
    /// `schema`. Keywords count wherever they appear as object keys.
    fn check_keywords(
        path: &Path,
        schema: &Value,
        value: &Value,
        is_root: bool,
        errors: &mut Vec<String>,
    ) {
        match value {
            Value::Object(members) => {
                for (key, member) in members {
                    let key = key.as_str();
                    let problem = if SCHEMA_IDENTIFIERS.contains(&key) {
                        Some(format!(
                            "holds {key}; an exported protocol schema names no base URI or \
                             anchor, so every reference is a JSON pointer into it"
                        ))
                    } else if key == "$schema"
                        && !(is_root && member.as_str() == Some(SCHEMA_DIALECT))
                    {
                        Some(format!(
                            "declares the dialect {member}; an exported protocol schema declares \
                             only {SCHEMA_DIALECT}, at its root"
                        ))
                    } else {
                        member
                            .as_str()
                            .filter(|target| {
                                SCHEMA_REFERENCES.contains(&key)
                                    && target
                                        .strip_prefix('#')
                                        .and_then(|pointer| schema.pointer(pointer))
                                        .is_none()
                            })
                            .map(|target| {
                                format!(
                                    "references {target}; an exported protocol schema references \
                                     only JSON pointers into itself"
                                )
                            })
                    };
                    if let Some(problem) = problem {
                        errors.push(format!("{}: {problem}", path.display()));
                    }
                    Self::check_keywords(path, schema, member, false, errors);
                }
            }
            Value::Array(items) => {
                for item in items {
                    Self::check_keywords(path, schema, item, false, errors);
                }
            }
            _ => {}
        }
    }

    /// Checks that the protocol types import nothing, through a statement,
    /// an `import()` type, `require`, or a triple-slash directive, and holds
    /// no template literal or unterminated string, and returns the version
    /// their one `ProtocolVersion` declaration names. Comments and the
    /// contents of single-line string literals are ignored, so prose and
    /// literal types may say anything.
    fn check_types(&self, path: &Path, text: &str, errors: &mut Vec<String>) -> Option<u64> {
        for directive in self.directive.find_iter(text) {
            Self::report(
                errors,
                path,
                text,
                directive.start(),
                "holds a triple-slash directive; exported protocol types import nothing",
            );
        }
        let code = self.code(path, text, errors);
        for target in self
            .module_load
            .captures_iter(&code)
            .filter_map(|capture| capture.get(1))
        {
            Self::report(
                errors,
                path,
                text,
                target.start(),
                &format!(
                    "imports {}; exported protocol types import nothing",
                    &text[target.range()]
                ),
            );
        }
        let versions: Vec<_> = self
            .protocol_version
            .captures_iter(&code)
            .filter_map(|capture| capture.get(1))
            .collect();
        let version = match versions.as_slice() {
            [version] => version.as_str().parse().ok(),
            _ => None,
        };
        if version.is_none() {
            errors.push(format!(
                "{}: the protocol types must declare one export type ProtocolVersion = <integer>;",
                path.display()
            ));
        }

        version
    }

    /// Returns TypeScript `text` with every comment and the contents of
    /// every single-line string literal blanked byte for byte, keeping line
    /// breaks and quotes, so offsets into it are offsets into `text`.
    /// Reports any backtick or quote left in the code between them, so a
    /// template literal or an unterminated string cannot hide code.
    fn code(&self, path: &Path, text: &str, errors: &mut Vec<String>) -> String {
        let mut code = String::with_capacity(text.len());
        let mut end = 0;
        for lexeme in self.lexeme.find_iter(text) {
            Self::check_code(path, text, end..lexeme.start(), errors);
            code.push_str(&text[end..lexeme.start()]);
            let quote = usize::from(!lexeme.as_str().starts_with('/'));
            let range = lexeme.start() + quote..lexeme.end() - quote;
            code.push_str(&text[lexeme.start()..range.start]);
            for char in text[range.clone()].chars() {
                if char == '\n' {
                    code.push('\n');
                } else {
                    code.extend(iter::repeat_n(' ', char.len_utf8()));
                }
            }
            code.push_str(&text[range.end..lexeme.end()]);
            end = lexeme.end();
        }
        Self::check_code(path, text, end..text.len(), errors);
        code.push_str(&text[end..]);

        code
    }

    /// Reports each backtick and quote in `text[range]`, code outside every
    /// comment and terminated string literal.
    fn check_code(path: &Path, text: &str, range: Range<usize>, errors: &mut Vec<String>) {
        for (offset, quote) in text[range.clone()].match_indices(['`', '"', '\'']) {
            let problem = if quote == "`" {
                "holds a template literal; template literals are not allowed in the exported types"
            } else {
                "holds an unterminated string; a string literal in the exported types ends on its \
                 own line"
            };
            Self::report(errors, path, text, range.start + offset, problem);
        }
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
        // patterns and rule-test snippets cannot name them either. A name
        // joined to a longer one by a hyphen, or following another label and a
        // dot, as in a reverse-domain namespace such as `org.example.key`, is
        // a different name.
        for matched in context
            .crates
            .iter()
            .flat_map(|crates| crates.find_iter(section))
        {
            let start = range.start + matched.start();
            let end = range.start + matched.end();
            let is_label = text[..start]
                .strip_suffix('.')
                .is_some_and(|before| before.ends_with(|char: char| char.is_ascii_alphanumeric()));
            if !text[..start].ends_with('-') && !text[end..].starts_with('-') && !is_label {
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

    /// Returns whether `target`, a reference from the export at `path`, is the
    /// file name, optionally after `./` and with any query or fragment, of
    /// another export in the same directory, which a consumer copies beside
    /// it.
    fn is_exported_sibling(&self, path: &Path, target: &str) -> bool {
        let target = target.strip_prefix("./").unwrap_or(target);
        let name = target.split(['?', '#']).next().unwrap_or_default();
        let is_bare =
            !name.is_empty() && !name.contains(['/', '\\', ':']) && name != "." && name != "..";

        is_bare
            && path
                .parent()
                .is_some_and(|directory| self.exports.contains(&directory.join(name)))
    }
}

/// What a design export may load besides fragments and `data:` URIs.
#[derive(Clone, Copy, PartialEq)]
enum Reach {
    /// Nothing: an SVG references only itself.
    Itself,
    /// Exported files beside it; the text names the kind of export.
    Siblings(&'static str),
}

/// `standards.toml` schema.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    design: Design,
    files: Vec<String>,
    instructions: Vec<Instructions>,
    lints: Lints,
    protocol: Protocol,
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
            .chain(&self.design.files)
            .chain(&self.design.assets)
            .chain([
                &self.design.guide,
                &self.design.tokens,
                &self.toolchain.file,
                &self.lints.manifest,
                &self.protocol.schema,
                &self.protocol.types,
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

/// The design guide and what a web page copies to follow it: the stylesheets
/// in load order, the machine-readable token file they are generated from,
/// and other files copied byte for byte.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Design {
    assets: Vec<String>,
    files: Vec<String>,
    guide: String,
    tokens: String,
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

/// The remote-control frame schema and the TypeScript types generated from
/// it, which a client of the protocol copies byte for byte.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Protocol {
    schema: String,
    types: String,
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

    fn is_file(&self, path: &Path) -> io::Result<bool> {
        std::fs::symlink_metadata(path).map(|metadata| metadata.file_type().is_file())
    }

    fn read(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }

    fn read_bytes(&self, path: &Path) -> io::Result<Vec<u8>> {
        std::fs::read(path)
    }
}

#[cfg(test)]
#[path = "check_standard_test.rs"]
mod tests;
