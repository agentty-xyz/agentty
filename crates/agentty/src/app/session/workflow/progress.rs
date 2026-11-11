//! Transient turn loader text derived from provider status and reasoning.

/// Maximum bytes retained from the newest reasoning line while it streams.
const REASONING_BUFFER_BYTE_LIMIT: usize = 4096;
/// Maximum characters shown from the newest reasoning line.
const REASONING_PREVIEW_CHAR_LIMIT: usize = 160;

/// Loader text for one turn.
///
/// Shows the newest reasoning line when the provider streams reasoning and
/// otherwise the latest status label. Each label starts a new reasoning
/// stream, so reasoning from an earlier step never outlives a later status.
#[derive(Debug, Default)]
pub(super) struct TurnProgress {
    label: Option<String>,
    reasoning: String,
}

impl TurnProgress {
    /// Records one status label and returns whether it was non-blank.
    pub(super) fn observe_label(&mut self, label: &str) -> bool {
        let label = label.trim();
        if label.is_empty() {
            return false;
        }

        self.label = Some(label.to_string());
        self.reasoning.clear();

        true
    }

    /// Appends one verbatim reasoning fragment, retaining only the newest
    /// non-blank line.
    pub(super) fn observe_reasoning(&mut self, fragment: &str) {
        self.reasoning.push_str(fragment);
        self.retain_latest_reasoning_line();
    }

    /// Returns the current loader text, if any was observed.
    pub(super) fn message(&self) -> Option<String> {
        self.reasoning_preview().or_else(|| self.label.clone())
    }

    /// Drops completed reasoning lines and bounds the line still streaming.
    fn retain_latest_reasoning_line(&mut self) {
        let content = self.reasoning.trim_end();
        if content.is_empty() {
            self.reasoning.clear();

            return;
        }

        let line_start = content.rfind('\n').map_or(0, |index| index + 1);
        self.reasoning.drain(..line_start);

        let excess_bytes = self
            .reasoning
            .len()
            .saturating_sub(REASONING_BUFFER_BYTE_LIMIT);
        if excess_bytes > 0 {
            let cut = self
                .reasoning
                .char_indices()
                .map(|(index, _)| index)
                .find(|index| *index >= excess_bytes)
                .unwrap_or(self.reasoning.len());
            self.reasoning.drain(..cut);
        }
    }

    /// Returns the newest reasoning line as one compact row.
    ///
    /// Markdown emphasis markers are removed because providers commonly bold
    /// reasoning headings, and long lines keep their newest words visible.
    fn reasoning_preview(&self) -> Option<String> {
        let preview = self
            .reasoning
            .replace("**", "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        if preview.is_empty() {
            return None;
        }

        let char_count = preview.chars().count();
        if char_count <= REASONING_PREVIEW_CHAR_LIMIT {
            return Some(preview);
        }

        let tail = preview
            .chars()
            .skip(char_count - (REASONING_PREVIEW_CHAR_LIMIT - 1))
            .collect::<String>();

        Some(format!("…{}", tail.trim_start()))
    }
}

#[cfg(test)]
#[path = "progress_test.rs"]
mod tests;
