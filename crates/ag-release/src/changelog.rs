//! Release entries of a `CHANGELOG.md`.
//!
//! The scan is line-based and keeps an entry's Markdown byte for byte: an
//! entry starts at a level-2 ATX heading naming the release and runs to the
//! next level-1 or level-2 heading outside a fenced code block.

use semver::Version;

/// The changelog's file name in the workspace root.
pub(crate) const FILE: &str = "CHANGELOG.md";

/// The changelog entry of one release.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChangelogEntry {
    /// Text after ` - ` in the heading, normally the release date.
    pub(crate) date: Option<String>,
    /// The entry's Markdown below its heading, without surrounding blank
    /// lines or trailing whitespace.
    pub(crate) notes: String,
}

impl ChangelogEntry {
    /// Returns the first entry of `changelog` whose heading names `version`.
    ///
    /// A matching heading is `## <label>` or `## <label> - <date>`, where
    /// `<label>` is `v<version>` or `<version>`, optionally in brackets.
    pub(crate) fn find(changelog: &str, version: &Version) -> Option<Self> {
        let labels = [format!("v{version}"), version.to_string()];
        let mut fence: Option<Fence> = None;
        let mut entry: Option<(Option<String>, usize)> = None;
        let mut offset = 0;
        for line in changelog.split_inclusive('\n') {
            let line_start = offset;
            offset += line.len();
            let text = line.trim_end_matches(['\n', '\r']);
            if let Some(open) = &fence {
                if open.is_closed_by(text) {
                    fence = None;
                }
                continue;
            }
            fence = Fence::open(text);
            let Some((level, heading)) = Self::heading(text) else {
                continue;
            };
            if let Some((date, notes_start)) = entry {
                return Some(Self::new(date, &changelog[notes_start..line_start]));
            }
            if level == 2
                && let Some(suffix) = Self::heading_suffix(heading, &labels)
            {
                let date = suffix
                    .strip_prefix(" - ")
                    .map(|date| date.trim().to_string());
                entry = Some((date, offset));
            }
        }

        entry.map(|(date, notes_start)| Self::new(date, &changelog[notes_start..]))
    }

    /// Returns `changelog` with a `<version> - <date>` level-2 heading added,
    /// or `None` when it already has an entry for `version`.
    ///
    /// The heading goes directly below an `## Unreleased` heading, so the
    /// unreleased notes become the release's, else above the first level-2
    /// heading, else at the end. It spells the version like the newest
    /// release heading, with or without brackets and the `v` prefix, and as
    /// `[v<version>]` when there is none.
    pub(crate) fn insert_heading(changelog: &str, version: &Version, date: &str) -> Option<String> {
        if Self::find(changelog, version).is_some() {
            return None;
        }
        let mut fence: Option<Fence> = None;
        let mut unreleased_end = None;
        let mut first_heading = None;
        let mut style = None;
        let mut offset = 0;
        for line in changelog.split_inclusive('\n') {
            let line_start = offset;
            offset += line.len();
            let text = line.trim_end_matches(['\n', '\r']);
            if let Some(open) = &fence {
                if open.is_closed_by(text) {
                    fence = None;
                }
                continue;
            }
            fence = Fence::open(text);
            let Some((2, heading)) = Self::heading(text) else {
                continue;
            };
            first_heading.get_or_insert(line_start);
            let label = heading.split(" - ").next().unwrap_or_default();
            let bracketed = label.starts_with('[') && label.ends_with(']');
            let label = label.trim_start_matches('[').trim_end_matches(']');
            if label.eq_ignore_ascii_case("unreleased") {
                unreleased_end.get_or_insert(offset);
            } else if style.is_none() {
                let prefixed = label.starts_with('v');
                if Version::parse(label.trim_start_matches('v')).is_ok() {
                    style = Some((bracketed, prefixed));
                }
            }
        }
        let (bracketed, prefixed) = style.unwrap_or((true, true));
        let label = format!("{}{version}", if prefixed { "v" } else { "" });
        let label = if bracketed {
            format!("[{label}]")
        } else {
            label
        };
        let heading = format!("## {label} - {date}\n");

        Some(match (unreleased_end, first_heading) {
            (Some(end), _) => {
                let separator = if changelog[..end].ends_with('\n') {
                    "\n"
                } else {
                    "\n\n"
                };
                format!(
                    "{}{separator}{heading}{}",
                    &changelog[..end],
                    &changelog[end..]
                )
            }
            (None, Some(start)) => {
                format!("{}{heading}\n{}", &changelog[..start], &changelog[start..])
            }
            (None, None) => {
                let separator = match changelog {
                    "" => "",
                    text if text.ends_with('\n') => "\n",
                    _ => "\n\n",
                };
                format!("{changelog}{separator}{heading}")
            }
        })
    }

    /// Returns the level and text of a level-1 or level-2 ATX heading.
    fn heading(line: &str) -> Option<(usize, &str)> {
        let content = strip_indent(line)?;
        let level = content.bytes().take_while(|byte| *byte == b'#').count();
        let rest = &content[level..];
        let separated = rest.is_empty() || rest.starts_with([' ', '\t']);

        ((1..=2).contains(&level) && separated).then(|| (level, rest.trim()))
    }

    /// Returns the text after the label of a heading that names one of
    /// `labels`, plain or in brackets: empty, or ` - ` and a date.
    fn heading_suffix<'heading>(
        heading: &'heading str,
        labels: &[String],
    ) -> Option<&'heading str> {
        labels
            .iter()
            .flat_map(|label| [format!("[{label}]"), label.clone()])
            .find_map(|label| {
                let suffix = heading.strip_prefix(&label)?;
                let dated = suffix
                    .strip_prefix(" - ")
                    .is_some_and(|date| !date.trim().is_empty());

                (suffix.is_empty() || dated).then_some(suffix)
            })
    }

    /// Returns the entry for `section`, the text between its heading and the
    /// next heading.
    fn new(date: Option<String>, section: &str) -> Self {
        let blank_prefix = section
            .split_inclusive('\n')
            .take_while(|line| line.trim().is_empty())
            .map(str::len)
            .sum::<usize>();

        Self {
            date,
            notes: section[blank_prefix..].trim_end().to_string(),
        }
    }
}

/// An open fenced code block: its fence character and length.
struct Fence {
    character: char,
    length: usize,
}

impl Fence {
    /// Returns the fence that `line` opens, if any: three or more backticks
    /// or tildes, where a backtick fence's info string has no backtick.
    fn open(line: &str) -> Option<Self> {
        let content = strip_indent(line)?;
        let character = content
            .chars()
            .next()
            .filter(|first| ['`', '~'].contains(first))?;
        let length = content
            .chars()
            .take_while(|next| *next == character)
            .count();
        let info = &content[length..];
        let valid = length >= 3 && !(character == '`' && info.contains('`'));

        valid.then_some(Self { character, length })
    }

    /// Returns whether `line` closes this fence: at least as many of its
    /// characters and nothing else.
    fn is_closed_by(&self, line: &str) -> bool {
        strip_indent(line).is_some_and(|content| {
            let length = content
                .chars()
                .take_while(|next| *next == self.character)
                .count();

            length >= self.length && content[length..].trim().is_empty()
        })
    }
}

/// Returns `line` without up to three leading spaces, or `None` when it is
/// indented further, which makes it an indented code block line.
fn strip_indent(line: &str) -> Option<&str> {
    let content = line.trim_start_matches(' ');

    (line.len() - content.len() <= 3).then_some(content)
}

#[cfg(test)]
#[path = "changelog_test.rs"]
mod tests;
