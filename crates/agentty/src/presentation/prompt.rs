//! Prompt composer presentation state: re-exported composer types and the
//! `@`-mention dropdown with its file-description cache.

use std::collections::HashMap;

pub use crate::domain::composer::{
    PendingImagePaste, PromptAttachment, PromptAttachmentState, PromptComposerState,
    PromptComposerSubmission, PromptGraphState, PromptHistoryState, PromptSlashCommandSet,
    PromptSlashStage, PromptSlashState, PromptSuggestionItem, PromptSuggestionList,
    PromptSuggestionSelection, apply_prompt_delete_range, build_prompt_slash_suggestion_list,
    current_line_delete_range, drain_prompt_submission, expand_delete_range_to_image_tokens,
    image_token_ranges, insert_prompt_character, insert_prompt_local_image_at, insert_prompt_text,
    needs_space_after_image_placeholder, prompt_slash_option_count, resolve_prompt_slash_selection,
};
use crate::domain::file_description::FileDescriptionSyntax;
use crate::domain::file_entry::{FileEntry, filter_entries, visible_entry_window};

/// UI state for prompt `@` file and directory mention selection.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PromptAtMentionState {
    /// Cached list of all files and directories in the session directory.
    pub all_entries: Vec<FileEntry>,
    /// Currently selected index in the filtered dropdown.
    pub selected_index: usize,
    /// Dropdown window whose descriptions were requested most recently.
    described_window: Option<DescribedWindow>,
    /// File descriptions keyed by entry path; `None` marks a requested path
    /// that is still loading or has no description. The cache lives as long
    /// as the open dropdown and holds only rows that were visible.
    file_descriptions: HashMap<String, Option<String>>,
}

impl PromptAtMentionState {
    /// Creates a new at-mention state with the given file entries.
    #[must_use]
    pub fn new(all_entries: Vec<FileEntry>) -> Self {
        Self {
            all_entries,
            described_window: None,
            file_descriptions: HashMap::new(),
            selected_index: 0,
        }
    }

    /// Returns the loaded description for one file path, if any.
    #[must_use]
    pub fn file_description(&self, path: &str) -> Option<&str> {
        self.file_descriptions.get(path)?.as_deref()
    }

    /// Stores descriptions loaded for previously requested paths.
    pub fn apply_file_descriptions(
        &mut self,
        descriptions: impl IntoIterator<Item = (String, Option<String>)>,
    ) {
        self.file_descriptions.extend(descriptions);
    }

    /// Returns visible files whose descriptions have not been requested yet
    /// and marks them as requested.
    ///
    /// Only files with a [`FileDescriptionSyntax`] qualify. Repeated calls
    /// with an unchanged query, selection, and entry count return nothing
    /// without filtering the index again.
    pub fn take_description_requests(&mut self, query: &str, max_visible: usize) -> Vec<String> {
        let window = DescribedWindow {
            entry_count: self.all_entries.len(),
            query: query.to_string(),
            selected_index: self.selected_index,
        };
        if self.described_window.as_ref() == Some(&window) {
            return Vec::new();
        }

        let filtered = filter_entries(&self.all_entries, query);
        let visible_range = visible_entry_window(self.selected_index, filtered.len(), max_visible);
        let requested_paths: Vec<String> = filtered[visible_range]
            .iter()
            .filter(|entry| {
                !entry.is_dir
                    && FileDescriptionSyntax::from_path(&entry.path).is_some()
                    && !self.file_descriptions.contains_key(&entry.path)
            })
            .map(|entry| entry.path.clone())
            .collect();

        self.described_window = Some(window);
        self.file_descriptions
            .extend(requested_paths.iter().map(|path| (path.clone(), None)));

        requested_paths
    }
}

/// Query, selection, and index size that determine the visible dropdown rows.
#[derive(Clone, Debug, Eq, PartialEq)]
struct DescribedWindow {
    /// Number of indexed entries when the window was requested.
    entry_count: usize,
    /// Active `@` query text.
    query: String,
    /// Highlighted filtered row.
    selected_index: usize,
}

#[cfg(test)]
#[path = "prompt_test.rs"]
mod tests;
