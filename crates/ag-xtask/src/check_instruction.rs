//! Check that instruction files reference existing paths, local links, and
//! hooks, that each `AGENTS.md` has a `CLAUDE.md` symlink alias, and that
//! `AGENTS.md` files use no parent-directory-relative paths.

use std::collections::BTreeSet;
use std::io;
use std::path::{Path, PathBuf};

use percent_encoding::percent_decode_str;
use pulldown_cmark::{Event, Parser, Tag};
use regex::Regex;
use tracing::info;

use crate::repository;

/// Validates instructions using read-only Git inventory and filesystem
/// operations.
///
/// # Errors
/// Returns an error for unreadable inputs or broken instruction references.
pub(crate) fn run() -> Result<(), String> {
    let host = RealHost {
        git: PathBuf::from("git"),
    };
    let count = InstructionCheck::new(&host)?.run(Path::new("."))?;
    info!("Instruction integrity passed ({count} files).");

    Ok(())
}

/// External operations used by instruction discovery and validation.
#[cfg_attr(test, mockall::automock)]
trait Host {
    fn inventory(&self, root: &Path) -> Result<Vec<PathBuf>, String>;
    fn read(&self, path: &Path) -> io::Result<String>;
    fn inspect(&self, path: &Path) -> io::Result<Option<Entry>>;
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
}

/// Entry kinds needed to distinguish guides from their aliases and deleted
/// files.
#[derive(Debug, PartialEq)]
enum Entry {
    File,
    Symlink(PathBuf),
    Other,
}

/// Instruction policy, independent of the host filesystem and Git process.
struct InstructionCheck<'host> {
    hook: Regex,
    host: &'host dyn Host,
    inline: Regex,
    literal: Regex,
    scheme: Regex,
}

impl<'host> InstructionCheck<'host> {
    fn new(host: &'host dyn Host) -> Result<Self, String> {
        Ok(Self {
            hook: Self::pattern(r"\bprek\s+run\s+([a-z][a-z0-9-]*)")?,
            host,
            inline: Self::pattern(r"`[^`\n]+`")?,
            literal: Self::pattern(r"^[\w./-]+$")?,
            scheme: Self::pattern(r"^[a-zA-Z][a-zA-Z0-9+.-]*:")?,
        })
    }

    fn pattern(source: &str) -> Result<Regex, String> {
        Regex::new(source).map_err(|error| format!("Invalid instruction pattern: {error}"))
    }

    fn run(&self, root: &Path) -> Result<usize, String> {
        let hook_ids = repository::hook_ids(&self.read(&root.join(".pre-commit-config.yaml"))?)?;
        let documents = self.documents(root)?;
        let canonical_root = self
            .host
            .canonicalize(root)
            .map_err(|error| format!("Failed to resolve repository root: {error}"))?;
        let mut errors = Vec::new();
        if !documents.contains(Path::new("AGENTS.md")) {
            errors.push("AGENTS.md: missing root instruction guide".to_owned());
        }
        for document in &documents {
            let source = root.join(document);
            let text = self.read(&source)?;
            if document.file_name().is_some_and(|name| name == "AGENTS.md") {
                self.check_aliases(root, document, &mut errors)?;
            }
            self.check_text(&canonical_root, document, &text, &hook_ids, &mut errors)?;
        }
        if !errors.is_empty() {
            return Err(errors.join("\n"));
        }

        Ok(documents.len())
    }

    fn read(&self, path: &Path) -> Result<String, String> {
        self.host
            .read(path)
            .map_err(|error| format!("Failed to read {}: {error}", path.display()))
    }

    fn documents(&self, root: &Path) -> Result<BTreeSet<PathBuf>, String> {
        let mut documents = BTreeSet::new();
        for path in self.host.inventory(root)? {
            if (path.file_name().is_some_and(|name| name == "AGENTS.md")
                || path == Path::new("CONTRIBUTING.md")
                || (path.starts_with("skills")
                    && path.extension().is_some_and(|extension| extension == "md")))
                && self.inspect(&root.join(&path))? == Some(Entry::File)
            {
                documents.insert(path);
            }
        }

        Ok(documents)
    }

    fn inspect(&self, path: &Path) -> Result<Option<Entry>, String> {
        self.host
            .inspect(path)
            .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))
    }

    fn check_aliases(
        &self,
        root: &Path,
        document: &Path,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        let alias = document.with_file_name("CLAUDE.md");
        if self.inspect(&root.join(&alias))? != Some(Entry::Symlink(PathBuf::from("AGENTS.md"))) {
            errors.push(format!(
                "{}: must be a symlink to AGENTS.md",
                alias.display()
            ));
        }

        Ok(())
    }

    fn check_text(
        &self,
        root: &Path,
        document: &Path,
        text: &str,
        hook_ids: &BTreeSet<String>,
        errors: &mut Vec<String>,
    ) -> Result<(), String> {
        let is_guide = document.file_name().is_some_and(|name| name == "AGENTS.md");
        for matched in self.inline.find_iter(text) {
            let target = &text[matched.start() + 1..matched.end() - 1];
            let preceding = text[..matched.start()].ends_with('`');
            let following = text[matched.end()..].starts_with('`');
            if is_guide
                && !preceding
                && !following
                && self.literal.is_match(target)
                && Self::is_parent_relative(target)
            {
                Self::report_parent_relative(errors, document, text, matched.start(), target);
                continue;
            }
            if !preceding
                && !following
                && self.is_root_reference(target)
                && self.resolve(&root.join(target))?.is_none()
            {
                Self::report(
                    errors,
                    document,
                    text,
                    matched.start(),
                    &format!("missing path {target}"),
                );
            }
        }
        for (event, range) in Parser::new(text).into_offset_iter() {
            let Event::Start(Tag::Link { dest_url, .. } | Tag::Image { dest_url, .. }) = event
            else {
                continue;
            };
            let link = dest_url.as_ref();
            let path = link.split(['?', '#']).next().unwrap_or_default();
            if path.is_empty() || link.starts_with("//") || self.scheme.is_match(link) {
                continue;
            }
            if is_guide && Self::is_parent_relative(path) {
                Self::report_parent_relative(errors, document, text, range.start, link);
                continue;
            }
            let decoded = percent_decode_str(path).decode_utf8_lossy();
            let target = root.join(document).with_file_name(decoded.as_ref());
            if !self
                .resolve(&target)?
                .is_some_and(|target| target.starts_with(root))
            {
                Self::report(
                    errors,
                    document,
                    text,
                    range.start,
                    &format!("invalid local link {link}"),
                );
            }
        }
        for matched in self
            .hook
            .captures_iter(text)
            .filter_map(|capture| capture.get(1))
        {
            if !hook_ids.contains(matched.as_str()) {
                Self::report(
                    errors,
                    document,
                    text,
                    matched.start(),
                    &format!("unknown hook {}", matched.as_str()),
                );
            }
        }

        Ok(())
    }

    fn is_root_reference(&self, target: &str) -> bool {
        const PREFIXES: &[&str] = &["crates/", "docs/", "skills/", ".github/", "container/"];
        const FILES: &[&str] = &[
            "AGENTS.md",
            "CLAUDE.md",
            "README.md",
            "CONTRIBUTING.md",
            "Cargo.toml",
            "Cargo.lock",
            "rust-toolchain.toml",
            ".pre-commit-config.yaml",
        ];

        self.literal.is_match(target)
            && !target.contains("NNN_")
            && (FILES.contains(&target) || PREFIXES.iter().any(|prefix| target.starts_with(prefix)))
    }

    /// Returns whether a slash-separated path contains a `..` component.
    fn is_parent_relative(path: &str) -> bool {
        path.contains('/') && path.split('/').any(|component| component == "..")
    }

    fn report_parent_relative(
        errors: &mut Vec<String>,
        document: &Path,
        text: &str,
        offset: usize,
        target: &str,
    ) {
        Self::report(
            errors,
            document,
            text,
            offset,
            &format!("parent-relative path {target}; write it from the repository root"),
        );
    }

    fn resolve(&self, path: &Path) -> Result<Option<PathBuf>, String> {
        match self.host.canonicalize(path) {
            Ok(path) => Ok(Some(path)),
            Err(error) if Self::is_missing(&error) => Ok(None),
            Err(error) => Err(format!("Failed to resolve {}: {error}", path.display())),
        }
    }

    fn is_missing(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
        )
    }

    fn report(errors: &mut Vec<String>, document: &Path, text: &str, offset: usize, message: &str) {
        let line = text[..offset].bytes().filter(|byte| *byte == b'\n').count() + 1;
        errors.push(format!("{}:{line}: {message}", document.display()));
    }
}

/// Host adapter; policy stays in `InstructionCheck`.
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

    fn inspect(&self, path: &Path) -> io::Result<Option<Entry>> {
        match std::fs::symlink_metadata(path) {
            Ok(metadata) if metadata.is_symlink() => {
                std::fs::read_link(path).map(|target| Some(Entry::Symlink(target)))
            }
            Ok(metadata) if metadata.is_file() => Ok(Some(Entry::File)),
            Ok(_) => Ok(Some(Entry::Other)),
            Err(error) if InstructionCheck::is_missing(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
}

#[cfg(test)]
#[path = "check_instruction_test.rs"]
mod tests;
