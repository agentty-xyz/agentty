//! Short file descriptions extracted from leading source documentation.
//!
//! `@`-mention lookup rows show these descriptions beside supported files.
//! Each [`FileDescriptionSyntax`] owns one language's extraction rule, so
//! supporting another file type means adding a variant and its extractor.

use std::iter;
use std::path::Path;

/// Maximum number of leading bytes read when describing one file.
pub const FILE_DESCRIPTION_PREFIX_BYTES: u64 = 8 * 1024;

/// Source syntax whose leading documentation describes the whole file.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FileDescriptionSyntax {
    /// Python module docstring; the description is its PEP 257 summary line.
    Python,
    /// Rust inner `//!` documentation; the description is its first
    /// paragraph, matching rustdoc's summary.
    Rust,
}

impl FileDescriptionSyntax {
    /// Returns the syntax for a supported file extension in `path`.
    #[must_use]
    pub fn from_path(path: &str) -> Option<Self> {
        match Path::new(path).extension()?.to_str()? {
            "py" | "pyi" => Some(Self::Python),
            "rs" => Some(Self::Rust),
            _ => None,
        }
    }

    /// Extracts a single-line description from the leading `source` text.
    ///
    /// Returns `None` when the file does not start with documentation.
    #[must_use]
    pub fn describe(self, source: &str) -> Option<String> {
        match self {
            Self::Python => python_description(source),
            Self::Rust => rust_description(source),
        }
    }
}

/// Returns the first paragraph of leading Rust `//!` documentation.
///
/// Blank lines, ordinary line and block comments such as license headers,
/// shebangs, and inner attributes may precede the documentation. Markdown
/// heading markers are removed.
fn rust_description(source: &str) -> Option<String> {
    let mut is_in_block_comment = false;
    let mut paragraph = Vec::new();

    for line in source.lines().map(str::trim) {
        if is_in_block_comment {
            is_in_block_comment = !line.contains("*/");

            continue;
        }

        if paragraph.is_empty()
            && let Some(comment_text) = line.strip_prefix("/*")
            && !comment_text.starts_with('!')
        {
            is_in_block_comment = !comment_text.contains("*/");

            continue;
        }

        if let Some(doc_text) = line.strip_prefix("//!") {
            let doc_text = doc_text.trim().trim_start_matches('#').trim();
            if !doc_text.is_empty() {
                paragraph.push(doc_text);

                continue;
            }
        }

        let is_preamble = line.is_empty() || line.starts_with("//") || line.starts_with("#!");
        if !paragraph.is_empty() || !is_preamble {
            break;
        }
    }

    single_line(&paragraph.join(" "))
}

/// Returns the summary line of a leading Python module docstring.
///
/// Blank lines and `#` comments, including shebang and encoding lines, may
/// precede the docstring. The summary is the first non-blank docstring line.
fn python_description(source: &str) -> Option<String> {
    let mut lines = source.lines();
    let opening_line = lines
        .by_ref()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with('#'))?;
    let literal = opening_line
        .strip_prefix(['r', 'R', 'u', 'U'])
        .filter(|rest| rest.starts_with(['"', '\'']))
        .unwrap_or(opening_line);
    let quote = ["\"\"\"", "'''", "\"", "'"]
        .into_iter()
        .find(|quote| literal.starts_with(quote))?;
    let docstring_lines = iter::once(&literal[quote.len()..]).chain(lines);

    for line in docstring_lines {
        let closing_index = line.find(quote);
        let text = closing_index.map_or(line, |index| &line[..index]);
        if let Some(description) = single_line(text) {
            return Some(description);
        }

        if closing_index.is_some() {
            return None;
        }
    }

    None
}

/// Collapses whitespace runs into single spaces, returning `None` for blank
/// text.
fn single_line(text: &str) -> Option<String> {
    let description = text.split_whitespace().collect::<Vec<_>>().join(" ");

    (!description.is_empty()).then_some(description)
}

#[cfg(test)]
#[path = "file_description_test.rs"]
mod tests;
