//! Check that each Rust module file paired with a same-named directory is a
//! router: it declares child modules and re-exports, and defines nothing.

use std::io;
use std::path::{Path, PathBuf};

/// Top-level item keywords a router module may contain.
const ROUTER_ITEMS: [&str; 3] = ["extern", "mod", "use"];

/// Validates module routers under the workspace `crates/` directory.
///
/// # Errors
/// Returns an error when a source directory or file is unreadable, or when a
/// router module defines anything other than module declarations, imports,
/// re-exports, and attributes.
pub(crate) fn run() -> Result<(), String> {
    ModuleRouterCheck {
        file_system: &RealFileSystem,
    }
    .run(Path::new("crates"))
}

/// Filesystem operations used by router discovery and validation.
#[cfg_attr(test, mockall::automock)]
trait FileSystem {
    /// Returns the entries directly inside `path`.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;

    /// Returns whether `path` is a directory, treating a missing path as not
    /// one.
    fn is_dir(&self, path: &Path) -> io::Result<bool>;

    /// Returns the UTF-8 contents of `path`.
    fn read_to_string(&self, path: &Path) -> io::Result<String>;
}

/// Router policy, independent of the host filesystem.
struct ModuleRouterCheck<'host> {
    file_system: &'host dyn FileSystem,
}

impl ModuleRouterCheck<'_> {
    fn run(&self, crates: &Path) -> Result<(), String> {
        let mut violations = Vec::new();
        for member in self.read_dir(crates)? {
            let sources = member.join("src");
            if self.is_dir(&sources)? {
                self.check_directory(&sources, &mut violations)?;
            }
        }

        if violations.is_empty() {
            return Ok(());
        }

        Err(format!(
            "Module router violations:\n{}",
            violations.join("\n")
        ))
    }

    /// Checks every router module below `directory`, skipping test files and
    /// test-module directories.
    fn check_directory(
        &self,
        directory: &Path,
        violations: &mut Vec<String>,
    ) -> Result<(), String> {
        for path in self.read_dir(directory)? {
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            if self.is_dir(&path)? {
                if !name.ends_with("_test") {
                    self.check_directory(&path, violations)?;
                }
                continue;
            }
            let Some(stem) = name.strip_suffix(".rs") else {
                continue;
            };
            if stem.ends_with("_test") || !self.is_dir(&directory.join(stem))? {
                continue;
            }
            let text = self
                .file_system
                .read_to_string(&path)
                .map_err(|error| format!("Failed to read {}: {error}", path.display()))?;
            violations.extend(Self::definitions(&text).into_iter().map(|kind| {
                format!(
                    "{}: router module `{name}` defines `{kind}`; move it into a child module \
                     under `{stem}/`",
                    path.display()
                )
            }));
        }

        Ok(())
    }

    /// Returns the leading keyword of each top-level statement that is not a
    /// module declaration, import, re-export, or attribute.
    fn definitions(text: &str) -> Vec<String> {
        Self::code_definitions(&Self::code(text))
    }

    /// Returns [`Self::definitions`] for code without comments or literals,
    /// descending into inline modules, which may only group re-exports.
    fn code_definitions(code: &str) -> Vec<String> {
        Self::statements(code)
            .iter()
            .flat_map(|statement| {
                let item = Self::strip_visibility(Self::strip_attributes(statement));
                if item.is_empty() {
                    return Vec::new();
                }
                let end = item
                    .find(|character: char| {
                        !(character.is_alphanumeric() || "_!".contains(character))
                    })
                    .unwrap_or(item.len());
                let keyword = if end == 0 {
                    &item[..item.chars().next().map_or(0, char::len_utf8)]
                } else {
                    &item[..end]
                };
                if keyword == "mod"
                    && let (Some(start), Some(end)) = (item.find('{'), item.rfind('}'))
                {
                    return Self::code_definitions(&item[start + 1..end]);
                }
                if ROUTER_ITEMS.contains(&keyword) {
                    return Vec::new();
                }

                vec![keyword.to_string()]
            })
            .collect()
    }

    /// Replaces comments and string and character literals with spaces, so
    /// their delimiters cannot affect statement splitting.
    fn code(text: &str) -> String {
        let characters = text.chars().collect::<Vec<_>>();
        let mut code = String::with_capacity(text.len());
        let mut index = 0;
        while index < characters.len() {
            let rest = &characters[index..];
            let skipped = match rest {
                ['/', '/', ..] => Some(
                    rest.iter()
                        .position(|character| *character == '\n')
                        .unwrap_or(rest.len()),
                ),
                ['/', '*', ..] => Some(Self::block_comment_length(rest)),
                ['"', ..] => Some(Self::string_length(rest)),
                ['r', '#' | '"', ..] => Self::raw_string_length(rest),
                ['\'', '\\', _, ..] => rest[3..]
                    .iter()
                    .position(|character| *character == '\'')
                    .map(|end| end + 4),
                ['\'', _, '\'', ..] => Some(3),
                _ => None,
            };
            if let Some(length) = skipped {
                code.push(' ');
                index += length;
            } else {
                code.push(characters[index]);
                index += 1;
            }
        }

        code
    }

    /// Returns the length of a possibly nested block comment, or of the rest of
    /// the text when it is unterminated.
    fn block_comment_length(rest: &[char]) -> usize {
        let mut depth = 0;
        let mut index = 0;
        while index < rest.len() {
            match &rest[index..] {
                ['/', '*', ..] => {
                    depth += 1;
                    index += 2;
                }
                ['*', '/', ..] => {
                    depth -= 1;
                    index += 2;
                    if depth == 0 {
                        return index;
                    }
                }
                _ => index += 1,
            }
        }

        rest.len()
    }

    /// Returns the length of a string literal starting at its opening quote.
    fn string_length(rest: &[char]) -> usize {
        let mut index = 1;
        while index < rest.len() {
            match rest[index] {
                '\\' => index += 2,
                '"' => return index + 1,
                _ => index += 1,
            }
        }

        rest.len()
    }

    /// Returns the length of a raw string literal starting at `r`, or `None`
    /// when `r` begins an identifier instead.
    fn raw_string_length(rest: &[char]) -> Option<usize> {
        let hashes = rest[1..]
            .iter()
            .take_while(|character| **character == '#')
            .count();
        if rest.get(1 + hashes) != Some(&'"') {
            return None;
        }
        let body = 2 + hashes;
        let closing = std::iter::once('"')
            .chain(std::iter::repeat_n('#', hashes))
            .collect::<Vec<_>>();

        Some(
            rest[body..]
                .windows(closing.len())
                .position(|window| window == closing.as_slice())
                .map_or(rest.len(), |end| body + end + closing.len()),
        )
    }

    /// Splits code into top-level statements ending at `;` or at the brace
    /// that closes a top-level block, dropping the lone `;` that follows a
    /// braced statement such as `use crate::{a, b};`.
    fn statements(code: &str) -> Vec<String> {
        let mut statements = Vec::new();
        let mut current = String::new();
        let mut depth = 0_usize;
        for character in code.chars() {
            current.push(character);
            match character {
                '(' | '[' | '{' => depth += 1,
                ')' | ']' => depth = depth.saturating_sub(1),
                '}' => {
                    depth = depth.saturating_sub(1);
                    if depth == 0 {
                        statements.push(std::mem::take(&mut current));
                    }
                }
                ';' if depth == 0 => statements.push(std::mem::take(&mut current)),
                _ => {}
            }
        }
        statements.push(current);

        statements
            .into_iter()
            .map(|statement| statement.trim().to_string())
            .filter(|statement| !statement.is_empty() && statement != ";")
            .collect()
    }

    /// Removes leading outer and inner attributes from a statement.
    fn strip_attributes(statement: &str) -> &str {
        let mut rest = statement.trim_start();
        while let Some(attribute) = rest.strip_prefix('#') {
            let attribute = attribute.trim_start();
            let attribute = attribute
                .strip_prefix('!')
                .unwrap_or(attribute)
                .trim_start();
            let Some(length) = Self::bracket_length(attribute) else {
                return rest;
            };
            rest = attribute[length..].trim_start();
        }

        rest
    }

    /// Returns the byte length of a balanced `[...]` group at the start of
    /// `text`.
    fn bracket_length(text: &str) -> Option<usize> {
        if !text.starts_with('[') {
            return None;
        }
        let mut depth = 0_usize;
        for (index, character) in text.char_indices() {
            match character {
                '[' => depth += 1,
                ']' => {
                    depth -= 1;
                    if depth == 0 {
                        return Some(index + 1);
                    }
                }
                _ => {}
            }
        }

        None
    }

    /// Removes a leading `pub` or restricted visibility such as `pub(crate)`.
    fn strip_visibility(item: &str) -> &str {
        let Some(rest) = item.strip_prefix("pub") else {
            return item;
        };
        if rest.starts_with(|character: char| character.is_alphanumeric() || character == '_') {
            return item;
        }
        let rest = rest.trim_start();
        if rest.starts_with('(')
            && let Some(end) = rest.find(')')
        {
            return rest[end + 1..].trim_start();
        }

        rest
    }

    fn read_dir(&self, directory: &Path) -> Result<Vec<PathBuf>, String> {
        let mut entries = self
            .file_system
            .read_dir(directory)
            .map_err(|error| format!("Failed to read {}: {error}", directory.display()))?;
        entries.sort();

        Ok(entries)
    }

    fn is_dir(&self, path: &Path) -> Result<bool, String> {
        self.file_system
            .is_dir(path)
            .map_err(|error| format!("Failed to inspect {}: {error}", path.display()))
    }
}

/// Host adapter; router decisions remain in `ModuleRouterCheck`.
struct RealFileSystem;

impl FileSystem for RealFileSystem {
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        std::fs::read_dir(path)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect()
    }

    fn is_dir(&self, path: &Path) -> io::Result<bool> {
        match std::fs::metadata(path) {
            Ok(metadata) => Ok(metadata.is_dir()),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                Ok(false)
            }
            Err(error) => Err(error),
        }
    }

    fn read_to_string(&self, path: &Path) -> io::Result<String> {
        std::fs::read_to_string(path)
    }
}

#[cfg(test)]
#[path = "check_module_router_test.rs"]
mod tests;
