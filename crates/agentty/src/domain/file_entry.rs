//! File-entry model shared by file indexing, prompt state, and UI layout.

use std::path::PathBuf;

/// Selects the directory backing an active `@`-mention lookup.
#[must_use]
pub fn at_mention_lookup_root(
    project_working_dir: PathBuf,
    session_folder: Option<PathBuf>,
    has_session_folder: bool,
) -> PathBuf {
    if has_session_folder && let Some(session_folder) = session_folder {
        return session_folder;
    }

    project_working_dir
}

/// A single file or directory entry for `@` mention dropdowns and explorer
/// lists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    /// Whether this entry is a directory.
    pub is_dir: bool,
    /// Relative path from the listing root, for example `src/main.rs`.
    pub path: String,
}

/// Fuzzy-filters entries and returns them sorted by best match.
///
/// Query characters must appear in order (case-insensitive) within the path.
/// Results are ranked by consecutive-character runs, matches at the start of
/// path segments, and a basename-substring bonus. Equal scores prefer
/// shallower and shorter paths before alphabetical order. If the query ends
/// with `/`, directory entries are prioritized before files.
pub fn filter_entries<'a>(entries: &'a [FileEntry], query: &str) -> Vec<&'a FileEntry> {
    if query.is_empty() {
        return entries.iter().collect();
    }

    let query_lower = query.to_lowercase();
    let query_chars: Vec<char> = query_lower.chars().collect();
    let mut scored: Vec<ScoredEntry<'_>> = entries
        .iter()
        .filter_map(|entry| {
            fuzzy_score_for_entry(entry, &query_chars, &query_lower).map(|score| ScoredEntry {
                depth: path_depth(&entry.path),
                entry,
                path_len: entry.path.len(),
                score,
            })
        })
        .collect();

    scored.sort_by(compare_scored_entries);

    let mut filtered: Vec<&FileEntry> = scored.into_iter().map(|entry| entry.entry).collect();
    prioritize_directories_for_trailing_slash(&mut filtered, query);

    filtered
}

/// Cached fuzzy-match metadata used when ordering file mention results.
struct ScoredEntry<'a> {
    /// The number of path separators in the entry path.
    depth: usize,
    /// The underlying file or directory entry.
    entry: &'a FileEntry,
    /// The UTF-8 byte length of the entry path.
    path_len: usize,
    /// The fuzzy-match relevance score.
    score: i32,
}

/// Orders scored fuzzy matches so equally relevant results prefer root-level
/// and shorter paths before falling back to alphabetical ordering.
fn compare_scored_entries(first: &ScoredEntry<'_>, second: &ScoredEntry<'_>) -> std::cmp::Ordering {
    second
        .score
        .cmp(&first.score)
        .then_with(|| first.depth.cmp(&second.depth))
        .then_with(|| first.path_len.cmp(&second.path_len))
        .then(first.entry.path.cmp(&second.entry.path))
}

/// Returns the number of path separators in one relative path.
fn path_depth(path: &str) -> usize {
    path.bytes().filter(|&byte| byte == b'/').count()
}

/// Scores one [`FileEntry`] against the query while treating directories as if
/// their visible path ended with `/`.
fn fuzzy_score_for_entry(
    entry: &FileEntry,
    query_chars: &[char],
    query_lower: &str,
) -> Option<i32> {
    fuzzy_score(&entry.path, query_chars, query_lower, entry.is_dir)
}

/// Reorders fuzzy-match results so trailing-slash queries keep directory
/// entries ahead of files.
fn prioritize_directories_for_trailing_slash(entries: &mut Vec<&FileEntry>, query: &str) {
    if !query.ends_with('/') {
        return;
    }

    entries.sort_by_key(|entry| !entry.is_dir);
}

/// Scores a fuzzy match of `query_chars` against `path`.
///
/// Returns `Some(score)` if all query characters appear in order and `None`
/// otherwise. When `append_trailing_slash` is `true`, scoring behaves as if
/// `path` ended with `/` without allocating a new `String`.
fn fuzzy_score(
    path: &str,
    query_chars: &[char],
    query_lower: &str,
    append_trailing_slash: bool,
) -> Option<i32> {
    let mut state = FuzzyScoreState::new();

    for path_char_orig in path.chars().chain(append_trailing_slash.then_some('/')) {
        if state.query_is_complete(query_chars) {
            break;
        }

        state.score_path_character(path_char_orig, query_chars);
    }

    if state.query_is_complete(query_chars) {
        Some(state.score + basename_match_bonus(path, query_lower))
    } else {
        None
    }
}

/// Tracks fuzzy-match scoring state while walking a candidate path.
struct FuzzyScoreState {
    prev_matched: bool,
    prev_path_char: Option<char>,
    query_index: usize,
    score: i32,
}

impl FuzzyScoreState {
    /// Creates an empty scoring state for one candidate path.
    fn new() -> Self {
        Self {
            prev_matched: false,
            prev_path_char: None,
            query_index: 0,
            score: 0,
        }
    }

    /// Returns whether every query character has already matched.
    fn query_is_complete(&self, query_chars: &[char]) -> bool {
        self.query_index >= query_chars.len()
    }

    /// Scores one original path character, including lowercase expansions.
    fn score_path_character(&mut self, path_char_orig: char, query_chars: &[char]) {
        let mut matched = false;
        for path_char in path_char_orig.to_lowercase() {
            if self.query_is_complete(query_chars) {
                break;
            }

            matched = self.score_lowercase_character(path_char, query_chars);
        }

        self.prev_matched = matched;
        self.prev_path_char = Some(path_char_orig);
    }

    /// Scores one lowercase path character against the next query character.
    fn score_lowercase_character(&mut self, path_char: char, query_chars: &[char]) -> bool {
        if path_char != query_chars[self.query_index] {
            self.prev_matched = false;

            return false;
        }

        self.score += 1;
        self.add_consecutive_match_bonus();
        self.add_segment_start_bonus();
        self.query_index += 1;
        self.prev_matched = true;

        true
    }

    /// Adds a score bonus when the previous path character was also matched.
    fn add_consecutive_match_bonus(&mut self) {
        if self.prev_matched {
            self.score += 3;
        }
    }

    /// Adds a score bonus when a match starts a visible path segment.
    fn add_segment_start_bonus(&mut self) {
        if self.prev_path_char.is_none_or(is_segment_boundary) {
            self.score += 5;
        }
    }
}

/// Returns whether a path character marks the start of a new fuzzy segment.
fn is_segment_boundary(path_char: char) -> bool {
    matches!(path_char, '/' | '.' | '_' | '-')
}

/// Returns an extra score when the query text is found in the basename.
fn basename_match_bonus(path: &str, query: &str) -> i32 {
    if query.is_empty() || query.contains('/') {
        return 0;
    }

    let normalized_path = path.trim_end_matches('/');
    let basename = normalized_path
        .rsplit('/')
        .next()
        .unwrap_or(normalized_path);
    let basename_lower = basename.to_lowercase();
    let basename_stem = basename_lower.split('.').next().unwrap_or("");

    if basename_stem == query {
        return 60;
    }

    if basename_lower.starts_with(query) {
        return 45;
    }

    if basename_lower.contains(query) {
        return 30;
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filter_entries_ranks_exact_basename_before_deeper_match() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "crates/ag-git/src/setting/lib.rs".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "crates/agentty/src/app/setting.rs".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "setting");

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].path, "crates/agentty/src/app/setting.rs");
    }

    #[test]
    fn filter_entries_prioritizes_directories_for_trailing_slash() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "src/aaa.rs".to_string(),
            },
            FileEntry {
                is_dir: true,
                path: "src/zzz".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "src/");

        // Assert
        assert_eq!(filtered.len(), 2);
        assert!(filtered[0].is_dir);
        assert_eq!(filtered[0].path, "src/zzz");
    }

    #[test]
    fn filter_entries_returns_all_entries_for_empty_query() {
        // Arrange
        let entries = vec![
            FileEntry {
                is_dir: false,
                path: "a.txt".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "b.txt".to_string(),
            },
        ];

        // Act
        let filtered = filter_entries(&entries, "");

        // Assert
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn fuzzy_score_rejects_query_characters_in_the_wrong_order() {
        // Arrange & Act
        let score = fuzzy_score("abc.txt", &['c', 'b'], "cb", false);

        // Assert
        assert!(score.is_none());
    }

    #[test]
    fn fuzzy_score_stops_after_matching_lowercase_expansion() {
        // Arrange
        let query_chars = ['i'];

        // Act
        let score = fuzzy_score("İ", &query_chars, "i", false);

        // Assert
        assert!(score.is_some());
    }

    #[test]
    fn basename_match_bonus_scores_contains_match() {
        // Arrange & Act
        let score = basename_match_bonus("src/my_setting_helper.rs", "setting");

        // Assert
        assert_eq!(score, 30);
    }
}
