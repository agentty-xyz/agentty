use std::path::PathBuf;

use tokio::sync::mpsc;

use crate::app::session::SessionManager;
use crate::app::{AppEvent, TaskService};
use crate::domain::file_entry::{FileEntry, filter_entries};
use crate::domain::input::{InputState, is_at_mention_query_character};
use crate::domain::session::SessionId;
use crate::presentation::prompt::PromptAtMentionState;

/// Describes how one mode should update its visible `@`-mention state after an
/// input edit or cursor move.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AtMentionSyncAction {
    /// Open the dropdown and start loading entries.
    Activate,
    /// Hide the dropdown because the cursor no longer sits inside an `@` token.
    Dismiss,
    /// Keep the dropdown open and reset its selected row.
    KeepOpen,
}

/// Text replacement derived from the currently highlighted `@`-mention row.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct AtMentionSelection {
    /// Exclusive end character index of the complete active `@query` token.
    pub at_end: usize,
    /// Start character index of the active `@query`.
    pub at_start: usize,
    /// Replacement text inserted into the input.
    pub text: String,
}

/// Returns the next `@`-mention sync action for one input buffer and dropdown
/// state pair.
pub(crate) fn sync_action(
    input: &InputState,
    at_mention_state: Option<&PromptAtMentionState>,
) -> AtMentionSyncAction {
    match (
        input.at_mention_query().is_some(),
        at_mention_state.is_some(),
    ) {
        (true, true) => AtMentionSyncAction::KeepOpen,
        (true, false) => AtMentionSyncAction::Activate,
        (false, _) => AtMentionSyncAction::Dismiss,
    }
}

/// Starts asynchronous loading of `@`-mention entries for one composer root.
///
/// When a fresh cache entry already exists for `lookup_root`, this emits the
/// loaded event immediately and skips the debounced filesystem walk.
pub(crate) fn start_loading_entries(
    event_tx: mpsc::UnboundedSender<AppEvent>,
    lookup_root: PathBuf,
    session_id: SessionId,
    session_manager: &mut SessionManager,
) {
    let cached_entries = session_manager.at_mention_index_for_root(&lookup_root);

    TaskService::spawn_at_mention_entries_task(event_tx, cached_entries, lookup_root, session_id);
}

/// Clears one visible `@`-mention dropdown state.
pub(crate) fn dismiss(at_mention_state: &mut Option<PromptAtMentionState>) {
    *at_mention_state = None;
}

/// Resets the highlighted `@`-mention row to the first visible entry.
pub(crate) fn reset_selection(at_mention_state: &mut PromptAtMentionState) {
    at_mention_state.selected_index = 0;
}

/// Moves the highlighted `@`-mention row up by one item.
pub(crate) fn move_selection_up(at_mention_state: &mut PromptAtMentionState) {
    at_mention_state.selected_index = at_mention_state.selected_index.saturating_sub(1);
}

/// Moves the highlighted `@`-mention row down by one filtered item.
pub(crate) fn move_selection_down(input: &InputState, at_mention_state: &mut PromptAtMentionState) {
    let filtered_count =
        filtered_entries(input, at_mention_state).map_or(0_usize, |entries| entries.len());
    let max_index = filtered_count.saturating_sub(1);

    at_mention_state.selected_index = (at_mention_state.selected_index + 1).min(max_index);
}

/// Returns the replacement text for the highlighted `@`-mention entry, if the
/// input still contains an active `@query`. The replacement spans the whole
/// token, including any suffix after the cursor, and preserves its delimiters.
pub(crate) fn selected_replacement(
    input: &InputState,
    at_mention_state: &PromptAtMentionState,
) -> Option<AtMentionSelection> {
    let (at_start, query) = input.at_mention_query()?;
    let filtered = filter_entries(&at_mention_state.all_entries, &query);
    let clamped_index = at_mention_state
        .selected_index
        .min(filtered.len().saturating_sub(1));
    let at_end = input.cursor
        + input
            .text()
            .chars()
            .skip(input.cursor)
            .take_while(|character| is_at_mention_query_character(*character))
            .count();

    filtered.get(clamped_index).map(|entry| AtMentionSelection {
        at_end,
        at_start,
        text: format_mention_text(entry),
    })
}

/// Returns the filtered `@`-mention entries for the current input query.
fn filtered_entries<'a>(
    input: &InputState,
    at_mention_state: &'a PromptAtMentionState,
) -> Option<Vec<&'a FileEntry>> {
    let (_, query) = input.at_mention_query()?;

    Some(filter_entries(&at_mention_state.all_entries, &query))
}

/// Formats one selected file or directory entry for insertion into the input.
fn format_mention_text(entry: &FileEntry) -> String {
    if entry.is_dir {
        return format!("@{}/ ", entry.path);
    }

    format!("@{} ", entry.path)
}

#[cfg(test)]
#[path = "at_mention_test.rs"]
mod tests;
