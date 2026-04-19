use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::app::AppEvent;
use crate::domain::input::InputState;
use crate::infra::file_index::{self, FileEntry};
use crate::ui::state::prompt::PromptAtMentionState;

/// Delay applied before a fresh `@`-mention filesystem walk starts.
const AT_MENTION_LOAD_DEBOUNCE: Duration = Duration::from_millis(75);
/// Monotonic counter used to distinguish stale and current load tasks.
static NEXT_AT_MENTION_REQUEST_ID: AtomicU64 = AtomicU64::new(1);
/// Per-session debounced file-index tasks keyed by session identifier.
static PENDING_AT_MENTION_LOADS: LazyLock<Mutex<HashMap<String, PendingAtMentionLoad>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

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
    /// End character index of the active `@query`.
    pub cursor: usize,
    /// Replacement text inserted into the input.
    pub text: String,
    /// Start character index of the active `@query`.
    pub at_start: usize,
}

/// Tracks one debounced background file-index load for a session.
struct PendingAtMentionLoad {
    /// Background task that sleeps, walks, and publishes the latest entries.
    handle: JoinHandle<()>,
    /// Monotonic identifier used to ignore stale completions.
    request_id: u64,
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
pub(crate) fn start_loading_entries(
    event_tx: mpsc::UnboundedSender<AppEvent>,
    lookup_root: PathBuf,
    session_id: String,
) {
    let request_id = NEXT_AT_MENTION_REQUEST_ID.fetch_add(1, Ordering::Relaxed);
    let tracked_session_id = session_id.clone();
    let task_session_id = session_id.clone();
    let handle = tokio::spawn(async move {
        tokio::time::sleep(AT_MENTION_LOAD_DEBOUNCE).await;

        let entries = tokio::task::spawn_blocking(move || file_index::list_files(&lookup_root))
            .await
            .unwrap_or_default();

        // Fire-and-forget: receiver may be dropped during shutdown.
        let _ = event_tx.send(AppEvent::AtMentionEntriesLoaded {
            entries,
            session_id,
        });

        finish_pending_load(&task_session_id, request_id);
    });

    track_pending_load(tracked_session_id, request_id, handle);
}

/// Aborts and removes any pending debounced load for one session.
pub(crate) fn clear_pending_load(session_id: &str) {
    if let Ok(mut pending_loads) = PENDING_AT_MENTION_LOADS.lock()
        && let Some(pending_load) = pending_loads.remove(session_id)
    {
        pending_load.handle.abort();
    }
}

/// Stores the latest pending load task for one session and aborts any stale
/// debounced predecessor.
fn track_pending_load(session_id: String, request_id: u64, handle: JoinHandle<()>) {
    if let Ok(mut pending_loads) = PENDING_AT_MENTION_LOADS.lock()
        && let Some(previous_task) =
            pending_loads.insert(session_id, PendingAtMentionLoad { handle, request_id })
    {
        previous_task.handle.abort();
    }
}

/// Clears a pending task entry when the completing request is still current.
fn finish_pending_load(session_id: &str, request_id: u64) {
    if let Ok(mut pending_loads) = PENDING_AT_MENTION_LOADS.lock()
        && pending_loads
            .get(session_id)
            .is_some_and(|task| task.request_id == request_id)
    {
        pending_loads.remove(session_id);
    }
}

/// Returns the directory that should back one active `@`-mention lookup.
///
/// Materialized sessions index their worktree folder. Unstarted draft sessions
/// have no worktree yet, so the active project working directory is used until
/// the deferred worktree exists.
pub(crate) fn lookup_root(
    project_working_dir: PathBuf,
    session_folder: Option<PathBuf>,
    has_session_folder: bool,
) -> PathBuf {
    if has_session_folder && let Some(session_folder) = session_folder {
        return session_folder;
    }

    project_working_dir
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
/// input still contains an active `@query`.
pub(crate) fn selected_replacement(
    input: &InputState,
    at_mention_state: &PromptAtMentionState,
) -> Option<AtMentionSelection> {
    let (at_start, query) = input.at_mention_query()?;
    let filtered = file_index::filter_entries(&at_mention_state.all_entries, &query);
    let clamped_index = at_mention_state
        .selected_index
        .min(filtered.len().saturating_sub(1));

    filtered.get(clamped_index).map(|entry| AtMentionSelection {
        at_start,
        cursor: input.cursor,
        text: format_mention_text(entry),
    })
}

/// Returns the filtered `@`-mention entries for the current input query.
fn filtered_entries<'a>(
    input: &InputState,
    at_mention_state: &'a PromptAtMentionState,
) -> Option<Vec<&'a FileEntry>> {
    let (_, query) = input.at_mention_query()?;

    Some(file_index::filter_entries(
        &at_mention_state.all_entries,
        &query,
    ))
}

/// Formats one selected file or directory entry for insertion into the input.
fn format_mention_text(entry: &FileEntry) -> String {
    if entry.is_dir {
        return format!("@{}/ ", entry.path);
    }

    format!("@{} ", entry.path)
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn test_sync_action_requests_activation_for_new_query() {
        // Arrange
        let input = InputState::with_text("@src".to_string());

        // Act
        let action = sync_action(&input, None);

        // Assert
        assert_eq!(action, AtMentionSyncAction::Activate);
    }

    #[test]
    fn test_move_selection_down_clamps_to_last_filtered_entry() {
        // Arrange
        let input = InputState::with_text("@src".to_string());
        let mut at_mention_state = PromptAtMentionState::new(vec![
            FileEntry {
                is_dir: true,
                path: "src".to_string(),
            },
            FileEntry {
                is_dir: false,
                path: "src/lib.rs".to_string(),
            },
        ]);
        at_mention_state.selected_index = 99;

        // Act
        move_selection_down(&input, &mut at_mention_state);

        // Assert
        assert_eq!(at_mention_state.selected_index, 1);
    }

    #[test]
    fn test_selected_replacement_formats_directory_with_trailing_slash() {
        // Arrange
        let input = InputState::with_text("@src".to_string());
        let at_mention_state = PromptAtMentionState::new(vec![FileEntry {
            is_dir: true,
            path: "src".to_string(),
        }]);

        // Act
        let selection =
            selected_replacement(&input, &at_mention_state).expect("expected directory selection");

        // Assert
        assert_eq!(
            selection,
            AtMentionSelection {
                at_start: 0,
                cursor: 4,
                text: "@src/ ".to_string(),
            }
        );
    }

    #[test]
    fn test_lookup_root_prefers_materialized_session_folder() {
        // Arrange
        let project_working_dir = PathBuf::from("/project");
        let session_folder = PathBuf::from("/project/.agentty/session");

        // Act
        let lookup_root = lookup_root(project_working_dir, Some(session_folder.clone()), true);

        // Assert
        assert_eq!(lookup_root, session_folder);
    }

    #[test]
    fn test_lookup_root_falls_back_to_project_working_dir_without_session_folder() {
        // Arrange
        let project_working_dir = PathBuf::from("/project");
        let session_folder = PathBuf::from("/project/.agentty/session");

        // Act
        let lookup_root = lookup_root(project_working_dir.clone(), Some(session_folder), false);

        // Assert
        assert_eq!(lookup_root, project_working_dir);
    }

    #[tokio::test]
    async fn test_start_loading_entries_aborts_stale_debounced_loads() {
        // Arrange
        let temp_dir = TempDir::new().expect("create temp dir");
        let event_session_id = "session-1".to_string();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();
        std::fs::write(temp_dir.path().join("first.txt"), "").expect("write first file");

        start_loading_entries(
            event_tx.clone(),
            temp_dir.path().to_path_buf(),
            event_session_id.clone(),
        );

        std::fs::write(temp_dir.path().join("second.txt"), "").expect("write second file");

        // Act
        start_loading_entries(
            event_tx,
            temp_dir.path().to_path_buf(),
            event_session_id.clone(),
        );
        let next_event = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .expect("at-mention event should arrive")
            .expect("event channel should stay open");

        // Assert
        match next_event {
            AppEvent::AtMentionEntriesLoaded {
                entries,
                session_id,
            } => {
                assert_eq!(session_id, event_session_id);
                assert!(entries.iter().any(|entry| entry.path == "second.txt"));
            }
            _ => unreachable!("expected at-mention entries event"),
        }

        let extra_event = tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await;
        assert!(!matches!(
            extra_event,
            Ok(Some(AppEvent::AtMentionEntriesLoaded { .. }))
        ));
    }

    #[tokio::test]
    async fn test_clear_pending_load_aborts_pending_task_for_session() {
        // Arrange
        let temp_dir = TempDir::new().expect("create temp dir");
        let session_id = "session-1".to_string();
        let (event_tx, mut event_rx) = mpsc::unbounded_channel();

        start_loading_entries(event_tx, temp_dir.path().to_path_buf(), session_id.clone());

        // Act
        clear_pending_load(&session_id);

        // Assert
        let next_event = tokio::time::timeout(Duration::from_millis(250), event_rx.recv()).await;
        assert!(!matches!(
            next_event,
            Ok(Some(AppEvent::AtMentionEntriesLoaded { .. }))
        ));
    }
}
