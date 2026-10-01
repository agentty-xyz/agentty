//! File/hunk-aware review partitioning and validation against captured
//! evidence.

use std::collections::{BTreeSet, VecDeque};

use ag_git::{DiffFile, hunk_starts};
use ag_protocol::{FocusedReview, FocusedReviewSide};

use super::diff_prompt;

/// Packs complete files and hunks before subdividing an oversized hunk.
pub(super) fn chunks(input: &str, limit: usize) -> VecDeque<String> {
    let files = DiffFile::parse(input);
    if files.is_empty() || files.iter().map(|file| file.text.len()).sum::<usize>() != input.len() {
        return diff_prompt::chunks(input, limit);
    }
    let mut output = VecDeque::new();
    let mut current = String::new();
    for file in files {
        for part in file_chunks(file.text, limit) {
            if current.len() + part.len() > limit && !current.is_empty() {
                output.push_back(std::mem::take(&mut current));
            }
            current.push_str(&part);
        }
    }
    output.push_back(current);

    output
}

/// Returns the unique file identities in a captured diff or its fragments.
pub(super) fn paths(diff: &str) -> BTreeSet<String> {
    DiffFile::parse(diff)
        .into_iter()
        .map(|file| file.new_path)
        .filter(|path| !path.is_empty())
        .collect()
}

/// Counts file sections whose identities cannot be recovered from the snapshot.
/// Keep these sections in coverage totals rather than silently omitting them.
pub(super) fn unresolved_files(diff: &str) -> usize {
    DiffFile::parse(diff)
        .iter()
        .filter(|file| file.new_path.is_empty())
        .count()
}

/// Resolves typed citations, clearing invalid or ambiguous model line numbers.
/// Returns the number of findings without a verified location.
pub(super) fn anchor(review: &mut FocusedReview, diff: &str) -> usize {
    let files = DiffFile::parse(diff);
    let mut unanchored = 0;
    for suggestion in &mut review.suggestions {
        let Some(evidence) = &mut suggestion.evidence else {
            unanchored += 1;
            continue;
        };
        let old = evidence.side == FocusedReviewSide::Old;
        let matches: Vec<_> = files
            .iter()
            .filter(|file| (if old { &file.old_path } else { &file.new_path }) == &evidence.path)
            .flat_map(|file| file.source_ranges(&evidence.existing_code, old))
            .collect();
        let claimed = (evidence.start_line, evidence.end_line);
        let resolved = if matches.contains(&claimed) {
            Some(claimed)
        } else if matches.len() == 1 {
            matches.first().copied()
        } else {
            None
        };
        let (start_line, end_line) = resolved.unwrap_or_default();
        suggestion.resolve_evidence_range(start_line, end_line);
        if resolved.is_none() {
            unanchored += 1;
        }
    }

    unanchored
}

fn file_chunks(file: &str, limit: usize) -> VecDeque<String> {
    if file.len() <= limit {
        return VecDeque::from([file.to_string()]);
    }
    let Some(first_hunk) = file.find("\n@@ ").map(|offset| offset + 1) else {
        return diff_prompt::chunks(file, limit);
    };
    let prefix = &file[..first_hunk];
    // Pathological metadata or source lines still use the byte-preserving
    // fallback; never invent line positions for a split source line.
    if prefix.len() + 128 >= limit
        || file[first_hunk..]
            .split_inclusive('\n')
            .any(|line| line.len() + prefix.len() + 128 > limit)
    {
        return diff_prompt::chunks(file, limit);
    }
    let mut parts = VecDeque::new();
    let mut current = prefix.to_string();
    let mut starts = Vec::new();
    let mut offset = first_hunk;
    for line in file[first_hunk..].split_inclusive('\n') {
        if line.starts_with("@@ ") {
            starts.push(offset);
        }
        offset += line.len();
    }
    for (index, start) in starts.iter().enumerate() {
        let hunk = &file[*start..starts.get(index + 1).copied().unwrap_or(file.len())];
        if prefix.len() + hunk.len() > limit {
            if current.len() > prefix.len() {
                parts.push_back(std::mem::take(&mut current));
            }
            parts.extend(split_hunk(prefix, hunk, limit));
            current = prefix.to_string();
        } else {
            if current.len() + hunk.len() > limit {
                parts.push_back(std::mem::take(&mut current));
                current.push_str(prefix);
            }
            current.push_str(hunk);
        }
    }
    if current.len() > prefix.len() {
        parts.push_back(current);
    }

    parts
}

fn split_hunk(prefix: &str, hunk: &str, limit: usize) -> VecDeque<String> {
    let Some((header, body)) = hunk.split_once('\n') else {
        return diff_prompt::chunks(&format!("{prefix}{hunk}"), limit);
    };
    let Some((mut old_start, mut new_start)) = hunk_starts(header) else {
        return diff_prompt::chunks(&format!("{prefix}{hunk}"), limit);
    };
    let mut parts = VecDeque::new();
    let mut remaining = body;
    while !remaining.is_empty() {
        let mut length = 0;
        let mut old_count = 0;
        let mut new_count = 0;
        for line in remaining.split_inclusive('\n') {
            if prefix.len() + 128 + length + line.len() > limit {
                break;
            }
            length += line.len();
            let context = line.starts_with(' ') || matches!(line, "\n" | "\r\n");
            old_count += u32::from(line.starts_with('-') || context);
            new_count += u32::from(line.starts_with('+') || context);
        }
        parts.push_back(format!(
            "{prefix}@@ -{old_start},{old_count} +{new_start},{new_count} @@\n{}",
            &remaining[..length],
        ));
        old_start = old_start.saturating_add(old_count);
        new_start = new_start.saturating_add(new_count);
        remaining = &remaining[length..];
    }

    parts
}

#[cfg(test)]
#[path = "review_diff_test.rs"]
mod tests;
