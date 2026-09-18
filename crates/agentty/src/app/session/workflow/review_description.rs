//! Preservation of remote review-request descriptions without trusting markers.

use std::collections::HashMap;

/// Keeps the entire remote body byte-for-byte and appends only new candidate
/// lines. Remote markers and checksums cannot establish generated ownership.
/// Callers validate preservation in the candidate before rendering.
pub(super) fn preserve_description(current: &str, candidate: &str) -> String {
    let mut current_lines = HashMap::<&str, usize>::new();
    for line in current.lines().map(str::trim) {
        *current_lines.entry(line).or_default() += 1;
    }
    let additions = candidate
        .lines()
        .filter(|line| {
            if let Some(count) = current_lines
                .get_mut(line.trim())
                .filter(|count| **count > 0)
            {
                *count -= 1;
                false
            } else {
                true
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let additions = additions.trim();
    if additions.is_empty() {
        return current.to_string();
    }

    format!("{current}\n\n{additions}")
}

#[cfg(test)]
#[path = "review_description_test.rs"]
mod tests;
