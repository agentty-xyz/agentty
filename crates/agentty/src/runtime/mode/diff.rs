use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;

use crate::app::{App, AppEvent};
use crate::presentation::app_mode::{
    AppMode, DiffPreview, DiffPreviewUnavailableReason, DiffRestoreTarget, DiffScrollCache,
    HelpContext, ViewportRect,
};
use crate::runtime::EventResult;
use crate::ui::component::file_explorer::FileExplorer;
use crate::ui::{RenderCacheStore, page};

/// Handles key input while the app is in `AppMode::Diff`.
///
/// File selection via `j`/`k` wraps around between the first and last file
/// explorer entries. Leaving diff mode restores the prior composer or question
/// snapshot when present; otherwise it rebuilds session view with any cached
/// focused review output for the same session.
pub(crate) fn handle_with_cache(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) -> EventResult {
    if handle_help_key(app, key) {
        return EventResult::Continue;
    }

    if handle_exit_key(app, key) {
        return EventResult::Continue;
    }

    handle_navigation_key(app, render_cache_store, content_area, key);

    EventResult::Continue
}

#[cfg(test)]
fn handle(app: &mut App, content_area: Rect, key: KeyEvent) -> EventResult {
    handle_with_cache(app, &RenderCacheStore::default(), content_area, key)
}

/// Runs `git diff` for the session with `session_id`, returning the diff text.
///
/// Returns `None` when the session is not loaded or the worktree diff is empty,
/// so callers can treat the diff shortcut as unavailable for unchanged
/// sessions. Git failures surface as a `Failed to run git diff:` message rather
/// than `None`, so the diff view still opens to report the error.
pub(crate) async fn session_diff(app: &App, session_id: &str) -> Option<String> {
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)?;

    let session_folder = session.folder.clone();
    let base_branch = session.base_branch.clone();

    let diff = app
        .services
        .git_client()
        .diff(session_folder, base_branch)
        .await
        .unwrap_or_else(|error| format!("Failed to run git diff: {error}"));

    if diff.trim().is_empty() {
        return None;
    }

    Some(diff)
}

/// Enters `AppMode::Diff` for `session_id` with a preloaded `diff`.
///
/// `restore` records the originating page so leaving the diff returns there;
/// `None` falls back to session view.
pub(crate) fn enter_diff_mode(
    app: &mut App,
    session_id: &str,
    diff: String,
    restore: Option<DiffRestoreTarget>,
) {
    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index: 0,
        preview: DiffPreview::default(),
        restore: restore.map(Box::new),
        scroll_cache: None,
        session_id: session_id.into(),
        scroll_offset: 0,
    };
}

/// Opens diff help while preserving the current diff-mode snapshot.
fn handle_help_key(app: &mut App, key: KeyEvent) -> bool {
    if key.code != KeyCode::Char('?') {
        return false;
    }

    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::Diff {
        diff,
        file_explorer_selected_index,
        preview,
        restore,
        session_id,
        scroll_offset,
        ..
    } = mode
    {
        app.mode = AppMode::Help {
            context: HelpContext::Diff {
                diff,
                file_explorer_selected_index,
                preview,
                restore,
                session_id,
                scroll_offset,
            },
            scroll_offset: 0,
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Leaves diff mode and restores the originating view or question state.
fn handle_exit_key(app: &mut App, key: KeyEvent) -> bool {
    if !matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
        return false;
    }

    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    if let AppMode::Diff {
        restore,
        session_id,
        ..
    } = mode
    {
        app.mode = if let Some(restore) = restore {
            restore.into_mode()
        } else {
            AppMode::View {
                session_id,
                scroll_offset: None,
            }
        };
    } else {
        app.mode = mode;
    }

    true
}

/// Applies file-selection and scroll navigation keys in diff mode.
fn handle_navigation_key(
    app: &mut App,
    render_cache_store: &RenderCacheStore,
    content_area: Rect,
    key: KeyEvent,
) {
    let mode = std::mem::replace(&mut app.mode, AppMode::List);
    let AppMode::Diff {
        diff,
        mut file_explorer_selected_index,
        mut preview,
        restore,
        mut scroll_cache,
        mut scroll_offset,
        session_id,
    } = mode
    else {
        app.mode = mode;

        return;
    };

    let mut selection_changed = false;
    match key.code {
        KeyCode::Char(character @ ('j' | 'k')) if is_plain_char_key(key, character) => {
            let content = render_cache_store.diff_layout_cache().content(&diff);
            let new_index = selected_index_after_key(
                character,
                file_explorer_selected_index,
                content.item_count(),
            );

            if file_explorer_selected_index != new_index {
                file_explorer_selected_index = new_index;
                selection_changed = true;
                scroll_cache = None;
                scroll_offset = 0;
            }
        }
        KeyCode::Down | KeyCode::Char('J' | 'j')
            if key.code == KeyCode::Down || is_shift_char_key(key, 'j') =>
        {
            let max_scroll_offset = diff_max_scroll_offset(
                &diff,
                content_area,
                file_explorer_selected_index,
                &mut scroll_cache,
                render_cache_store.diff_layout_cache(),
                render_cache_store.markdown_render_cache(),
                &preview,
            );
            scroll_offset = scroll_offset
                .min(max_scroll_offset)
                .saturating_add(1)
                .min(max_scroll_offset);
        }
        KeyCode::Up | KeyCode::Char('K' | 'k')
            if key.code == KeyCode::Up || is_shift_char_key(key, 'k') =>
        {
            let max_scroll_offset = diff_max_scroll_offset(
                &diff,
                content_area,
                file_explorer_selected_index,
                &mut scroll_cache,
                render_cache_store.diff_layout_cache(),
                render_cache_store.markdown_render_cache(),
                &preview,
            );
            scroll_offset = scroll_offset.min(max_scroll_offset).saturating_sub(1);
        }
        KeyCode::Char('p') if is_plain_char_key(key, 'p') => {
            if let Some(updated_preview) = toggle_selected_preview(
                app,
                render_cache_store.diff_layout_cache(),
                &diff,
                file_explorer_selected_index,
                &preview,
                &session_id,
            ) {
                preview = updated_preview;
                scroll_cache = None;
                scroll_offset = 0;
            }
        }
        _ => {}
    }

    if selection_changed && preview.is_enabled() {
        preview = start_selected_preview_load(
            app,
            render_cache_store.diff_layout_cache(),
            &diff,
            file_explorer_selected_index,
            &preview,
            &session_id,
        );
    }

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index,
        preview,
        restore,
        scroll_cache,
        scroll_offset,
        session_id,
    };
}

/// Returns the wrapped explorer selection for a plain `j` or `k` key.
fn selected_index_after_key(character: char, current_index: usize, item_count: usize) -> usize {
    if character == 'j' {
        return FileExplorer::next_selected_index(current_index, item_count);
    }

    FileExplorer::previous_selected_index(current_index, item_count)
}

/// Toggles preview for the selected row, ignoring unsupported toggle-on keys.
fn toggle_selected_preview(
    app: &App,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    diff: &str,
    selected_index: usize,
    preview: &DiffPreview,
    session_id: &str,
) -> Option<DiffPreview> {
    if preview.is_enabled() {
        return Some(preview.disabled());
    }
    selected_markdown_path(diff, selected_index, diff_layout_cache)?;

    Some(start_selected_preview_load(
        app,
        diff_layout_cache,
        diff,
        selected_index,
        preview,
        session_id,
    ))
}

/// Returns the selected markdown path from the cached diff tree.
fn selected_markdown_path(
    diff: &str,
    selected_index: usize,
    diff_layout_cache: &page::diff::DiffLayoutCache,
) -> Option<String> {
    diff_layout_cache
        .content(diff)
        .selected_markdown_path(selected_index)
        .map(str::to_string)
}

/// Starts a bounded background read for the active markdown selection.
fn start_selected_preview_load(
    app: &App,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    diff: &str,
    selected_index: usize,
    preview: &DiffPreview,
    session_id: &str,
) -> DiffPreview {
    let request_id = preview.next_request_id();
    let Some(path) = selected_markdown_path(diff, selected_index, diff_layout_cache) else {
        return DiffPreview::Unsupported { request_id };
    };
    let Some(session_folder) = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map(|session| session.folder.clone())
    else {
        return DiffPreview::Unavailable {
            path,
            reason: DiffPreviewUnavailableReason::LoadFailed(
                "Session worktree is unavailable".to_string(),
            ),
            request_id,
        };
    };

    let event_sender = app.services.event_sender();
    let git_client = app.services.git_client();
    let loaded_path = path.clone();
    let loaded_session_id = session_id.into();
    tokio::spawn(async move {
        let result = git_client
            .read_worktree_file(session_folder, loaded_path.clone())
            .await
            .map_err(|error| error.to_string());
        let _ = event_sender.send(AppEvent::DiffPreviewLoaded {
            path: loaded_path,
            request_id,
            result,
            session_id: loaded_session_id,
        });
    });

    DiffPreview::Loading { path, request_id }
}

/// Returns true when the key event is a plain character key with no
/// modifiers.
fn is_plain_char_key(key: KeyEvent, character: char) -> bool {
    key.code == KeyCode::Char(character) && key.modifiers == KeyModifiers::NONE
}

/// Returns true when the key event is a shifted character key, accepting both
/// uppercase and lowercase char payloads emitted by terminals.
fn is_shift_char_key(key: KeyEvent, character: char) -> bool {
    let lowercase_character = character.to_ascii_lowercase();
    let uppercase_character = character.to_ascii_uppercase();

    key.modifiers == KeyModifiers::SHIFT
        && matches!(
            key.code,
            KeyCode::Char(pressed)
                if pressed == lowercase_character || pressed == uppercase_character
        )
}

/// Returns the max valid scroll offset for the active diff selection.
fn diff_max_scroll_offset(
    diff: &str,
    content_area: Rect,
    selected_index: usize,
    scroll_cache: &mut Option<DiffScrollCache>,
    diff_layout_cache: &page::diff::DiffLayoutCache,
    markdown_render_cache: &crate::ui::markdown::MarkdownRenderCache,
    preview: &DiffPreview,
) -> u16 {
    if let Some(cached_scroll_limit) = scroll_cache
        && cached_scroll_limit.content_area == viewport_rect(content_area)
        && cached_scroll_limit.file_explorer_selected_index == selected_index
    {
        return cached_scroll_limit.max_scroll_offset;
    }

    let max_scroll_offset = page::diff::diff_view_max_scroll_offset(
        diff,
        selected_index,
        content_area,
        diff_layout_cache,
        markdown_render_cache,
        preview,
    );

    *scroll_cache = Some(DiffScrollCache {
        content_area: viewport_rect(content_area),
        file_explorer_selected_index: selected_index,
        max_scroll_offset,
    });

    max_scroll_offset
}

/// Converts terminal geometry into the frontend-neutral cached viewport key.
fn viewport_rect(content_area: Rect) -> ViewportRect {
    ViewportRect {
        height: content_area.height,
        width: content_area.width,
        x: content_area.x,
        y: content_area.y,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use crossterm::event::KeyModifiers;
    use ratatui::layout::Rect;

    use super::*;

    const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 12);

    /// Builds an app with one previewable session and injected git boundary.
    async fn preview_test_app(mock_git_client: ag_git::MockGitClient) -> (App, tempfile::TempDir) {
        let clients =
            crate::test_support::test_app_clients().with_git_client(Arc::new(mock_git_client));
        let (mut app, base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
        let session = crate::test_support::SessionFixtureBuilder::new()
            .id("session-id")
            .folder(base_dir.path().to_path_buf())
            .build();
        app.sessions =
            crate::test_support::session_manager_with_handles(vec![session], HashMap::new());

        (app, base_dir)
    }

    /// Waits through unrelated startup events for one diff-preview result.
    async fn next_diff_preview_event(app: &mut App) -> AppEvent {
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = app
                    .next_app_event()
                    .await
                    .expect("preview event channel should remain open");
                if matches!(event, AppEvent::DiffPreviewLoaded { .. }) {
                    return event;
                }
            }
        })
        .await
        .expect("diff preview event should arrive")
    }

    /// Returns a diff long enough to keep the diff pane scrollable in tests.
    fn scrollable_diff_fixture() -> String {
        (0..40)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Draft text carried by [`non_default_prompt_snapshot`].
    const RESTORE_DRAFT_TEXT: &str = "draft body";

    /// The single at-mention entry carried by [`non_default_prompt_snapshot`].
    fn prompt_mention_entry() -> crate::domain::file_entry::FileEntry {
        crate::domain::file_entry::FileEntry {
            is_dir: false,
            path: "src/main.rs".to_string(),
        }
    }

    /// Builds a prompt snapshot with non-default attachment, history, slash,
    /// and at-mention state so restore tests prove no composer field is
    /// dropped when leaving diff.
    fn non_default_prompt_snapshot() -> crate::presentation::app_mode::PromptModeSnapshot {
        use crate::domain::agent::AgentKind;
        use crate::domain::input::InputState;
        use crate::presentation::app_mode::PromptModeSnapshot;
        use crate::presentation::prompt::{
            PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashStage,
            PromptSlashState,
        };

        let mut attachment_state = PromptAttachmentState::default();
        attachment_state.register_local_image(std::path::PathBuf::from("/tmp/pic.png"), 0);

        let mut history_state =
            PromptHistoryState::new(vec!["prev one".to_string(), "prev two".to_string()]);
        history_state.draft_text = Some("saved draft".to_string());
        history_state.selected_index = Some(1);

        let mut slash_state = PromptSlashState::with_available_agent_kinds(vec![AgentKind::Codex]);
        slash_state.stage = PromptSlashStage::Model;
        slash_state.selected_index = 2;

        PromptModeSnapshot {
            at_mention_state: Some(PromptAtMentionState {
                all_entries: vec![prompt_mention_entry()],
                selected_index: 1,
            }),
            attachment_state,
            history_state,
            input: InputState::with_text(RESTORE_DRAFT_TEXT.to_string()),
            scroll_offset: Some(4),
            session_id: "session-p".into(),
            slash_state,
        }
    }

    /// Asserts `mode` is a prompt composer restored losslessly from
    /// [`non_default_prompt_snapshot`], with input focus.
    fn assert_restored_prompt_composer(mode: &AppMode) {
        use crate::domain::agent::AgentKind;
        use crate::presentation::prompt::PromptSlashStage;

        let AppMode::Prompt {
            at_mention_state,
            attachment_state,
            focus,
            history_state,
            input,
            scroll_offset,
            slash_state,
            ..
        } = mode
        else {
            unreachable!("expected AppMode::Prompt after leaving diff");
        };

        assert_eq!(*focus, crate::presentation::app_mode::ChatFocus::Input);
        assert_eq!(input.text(), RESTORE_DRAFT_TEXT);
        assert_eq!(*scroll_offset, Some(4));

        assert_eq!(attachment_state.attachments.len(), 1);
        assert_eq!(attachment_state.next_attachment_number, 2);

        assert_eq!(
            history_state.entries,
            vec!["prev one".to_string(), "prev two".to_string()]
        );
        assert_eq!(history_state.draft_text, Some("saved draft".to_string()));
        assert_eq!(history_state.selected_index, Some(1));

        assert_eq!(slash_state.available_agent_kinds, vec![AgentKind::Codex]);
        assert_eq!(slash_state.stage, PromptSlashStage::Model);
        assert_eq!(slash_state.selected_index, 2);

        let at_mention_state = at_mention_state
            .as_ref()
            .expect("at-mention state must survive leaving diff");
        assert_eq!(at_mention_state.selected_index, 1);
        assert_eq!(at_mention_state.all_entries, vec![prompt_mention_entry()]);
    }

    #[tokio::test]
    async fn test_handle_quit_key_returns_to_view_mode() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_key_restores_cached_review_output() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.review_cache.insert(
            "session-id".into(),
            crate::app::ReviewCacheEntry::Ready {
                text: "Focused review".to_string(),
                diff_hash: 7,
            },
        );
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 7,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::View {
                ref session_id,
                scroll_offset: None,
                ..
            } if session_id == "session-id"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_shift_j_increments_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: scrollable_diff_fixture(),
            scroll_offset: 3,
            file_explorer_selected_index: 2,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('J'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 4,
                file_explorer_selected_index: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_saturates_scroll_offset_at_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_shift_k_saturates_scroll_offset_at_zero() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 2,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('K'), KeyModifiers::SHIFT),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_j_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_j_wraps_file_selection_from_last_to_first() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_resets_scroll_offset() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 1,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_k_wraps_file_selection_from_first_to_last() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff --git a/src/main.rs b/src/main.rs\n+added".to_string(),
            scroll_offset: 10,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 0,
                file_explorer_selected_index: 1,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_question_mark_opens_help_overlay() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 5,
            file_explorer_selected_index: 3,
            preview: DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "README.md".to_string(),
                request_id: 6,
            },
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    ref session_id,
                    ref diff,
                    scroll_offset: 5,
                    file_explorer_selected_index: 3,
                    preview: DiffPreview::Ready { request_id: 6, .. },
                    ..
                },
                scroll_offset: 0,
            } if session_id == "session-id" && diff == "diff output"
        ));
    }

    #[tokio::test]
    async fn test_handle_down_key_clamps_scroll_offset_at_bottom() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let diff = scrollable_diff_fixture();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = crate::ui::markdown::MarkdownRenderCache::default();
        let preview = DiffPreview::default();
        let max_scroll_offset = diff_max_scroll_offset(
            &diff,
            TEST_TERMINAL_SIZE,
            0,
            &mut None,
            &diff_layout_cache,
            &markdown_render_cache,
            &preview,
        );
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff,
            scroll_offset: max_scroll_offset,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Down, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset,
                ..
            } if scroll_offset == max_scroll_offset
        ));
    }

    #[tokio::test]
    async fn test_handle_up_key_recovers_immediately_from_overscrolled_state() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let diff = scrollable_diff_fixture();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = crate::ui::markdown::MarkdownRenderCache::default();
        let preview = DiffPreview::default();
        let max_scroll_offset = diff_max_scroll_offset(
            &diff,
            TEST_TERMINAL_SIZE,
            0,
            &mut None,
            &diff_layout_cache,
            &markdown_render_cache,
            &preview,
        );
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff,
            scroll_offset: u16::MAX,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Up, KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset,
                ..
            } if scroll_offset == max_scroll_offset.saturating_sub(1)
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_question_snapshot_restores_question_mode() {
        // Arrange — diff opened from question mode carries a snapshot.
        use crate::domain::input::InputState;
        use crate::domain::question::QuestionItem;
        use crate::presentation::app_mode::QuestionModeSnapshot;

        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-q".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: Some(Box::new(DiffRestoreTarget::Question(
                QuestionModeSnapshot {
                    at_mention_state: None,
                    current_index: 0,
                    input: InputState::default(),
                    questions: vec![QuestionItem {
                        options: Vec::new(),
                        text: "Q?".to_string(),
                    }],
                    responses: Vec::new(),
                    scroll_offset: None,
                    selected_option_index: None,
                    session_id: "session-q".into(),
                },
            ))),
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — restored to Question mode, not View.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                focus: crate::presentation::app_mode::ChatFocus::Input,
                ..
            } if session_id == "session-q"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_prompt_snapshot_restores_prompt_mode() {
        // Arrange — diff opened from prompt mode carries a composer snapshot.
        use crate::domain::input::InputState;
        use crate::presentation::app_mode::PromptModeSnapshot;
        use crate::presentation::prompt::{
            PromptAttachmentState, PromptHistoryState, PromptSlashState,
        };

        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-p".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: Some(Box::new(DiffRestoreTarget::Prompt(PromptModeSnapshot {
                at_mention_state: None,
                attachment_state: PromptAttachmentState::default(),
                history_state: PromptHistoryState::new(Vec::new()),
                input: InputState::with_text("draft text".to_string()),
                scroll_offset: None,
                session_id: "session-p".into(),
                slash_state: PromptSlashState::default(),
            }))),
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — restored to prompt mode with the draft intact and input focus.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            &app.mode,
            AppMode::Prompt {
                focus: crate::presentation::app_mode::ChatFocus::Input,
                input,
                session_id,
                ..
            } if input.text() == "draft text" && session_id == "session-p"
        ));
    }

    #[tokio::test]
    async fn test_handle_quit_with_prompt_snapshot_preserves_composer_context() {
        // Arrange — a prompt snapshot carrying non-default attachment, history,
        // slash, and at-mention state so leaving diff cannot silently drop it.
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-p".into(),
            diff: "diff output".to_string(),
            scroll_offset: 0,
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: Some(Box::new(DiffRestoreTarget::Prompt(
                non_default_prompt_snapshot(),
            ))),
            scroll_cache: None,
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Assert — every composer field survives the diff round-trip.
        assert_restored_prompt_composer(&app.mode);
    }

    #[tokio::test]
    async fn test_handle_prompt_then_help_then_exit_preserves_composer_context() {
        // Arrange — diff opened from prompt mode, then the user opens help with `?`.
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-p".into(),
            diff: "diff output".to_string(),
            scroll_offset: 3,
            file_explorer_selected_index: 1,
            preview: DiffPreview::default(),
            restore: Some(Box::new(DiffRestoreTarget::Prompt(
                non_default_prompt_snapshot(),
            ))),
            scroll_cache: None,
        };

        // Act — open help overlay.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Intermediate assert — help carries the prompt restore target.
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    restore: Some(_),
                    ..
                },
                ..
            }
        ));

        // Act — close help overlay, returning to diff.
        crate::runtime::mode::help::handle(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Intermediate assert — diff still carries the prompt restore target.
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                restore: Some(_),
                ..
            }
        ));

        // Act — exit diff.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert — restored to the prompt composer with all context intact.
        assert_restored_prompt_composer(&app.mode);
    }

    #[tokio::test]
    async fn test_handle_question_then_help_then_exit_preserves_restore_question() {
        // Arrange — diff opened from question mode, then user opens help with `?`.
        use crate::domain::input::InputState;
        use crate::domain::question::QuestionItem;
        use crate::presentation::app_mode::QuestionModeSnapshot;

        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        let snapshot = QuestionModeSnapshot {
            at_mention_state: None,
            current_index: 1,
            input: InputState::default(),
            questions: vec![
                QuestionItem {
                    options: Vec::new(),
                    text: "Q1?".to_string(),
                },
                QuestionItem {
                    options: Vec::new(),
                    text: "Q2?".to_string(),
                },
            ],
            responses: vec!["answer-1".to_string()],
            scroll_offset: None,
            selected_option_index: None,
            session_id: "session-q".into(),
        };

        app.mode = AppMode::Diff {
            session_id: "session-q".into(),
            diff: "diff output".to_string(),
            scroll_offset: 3,
            file_explorer_selected_index: 1,
            preview: DiffPreview::default(),
            restore: Some(Box::new(DiffRestoreTarget::Question(snapshot))),
            scroll_cache: None,
        };

        // Act — open help overlay.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Intermediate assert — help carries the snapshot.
        assert!(matches!(
            app.mode,
            AppMode::Help {
                context: HelpContext::Diff {
                    restore: Some(_),
                    ..
                },
                ..
            }
        ));

        // Act — close help overlay, returning to diff.
        crate::runtime::mode::help::handle(
            &mut app,
            KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
        );

        // Intermediate assert — diff still carries the snapshot.
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                restore: Some(_),
                ..
            }
        ));

        // Act — exit diff.
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
        );

        // Assert — restored to Question mode, not View.
        assert!(matches!(
            app.mode,
            AppMode::Question {
                ref session_id,
                current_index: 1,
                focus: crate::presentation::app_mode::ChatFocus::Input,
                ..
            } if session_id == "session-q"
        ));
    }

    #[tokio::test]
    async fn test_handle_question_mark_in_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
        );

        // Assert — the help key is a no-op outside diff mode.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_navigation_key_in_non_diff_mode_leaves_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::List;

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );

        // Assert — navigation keys are a no-op outside diff mode.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(app.mode, AppMode::List));
    }

    #[tokio::test]
    async fn test_handle_unhandled_key_keeps_diff_mode_unchanged() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            session_id: "session-id".into(),
            diff: "diff output".to_string(),
            scroll_offset: 4,
            file_explorer_selected_index: 2,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
        };

        // Act
        let event_result = handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('x'), KeyModifiers::NONE),
        );

        // Assert — an unhandled key leaves the diff selection and scroll intact.
        assert!(matches!(event_result, EventResult::Continue));
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                scroll_offset: 4,
                file_explorer_selected_index: 2,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_preview_key_loads_renders_event_and_toggles_off() {
        // Arrange
        let mut mock_git_client = ag_git::MockGitClient::new();
        mock_git_client
            .expect_read_worktree_file()
            .withf(|_, path| path == "README.md")
            .times(1)
            .returning(|_, _| {
                Box::pin(async {
                    Ok(ag_git::WorktreeFileContent::Text(
                        "# Rendered preview".to_string(),
                    ))
                })
            });
        let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
            scroll_offset: 7,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );
        let event = next_diff_preview_event(&mut app).await;
        app.apply_app_events(event).await;
        let ready = matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Ready {
                    ref content,
                    request_id: 1,
                    ..
                },
                scroll_offset: 0,
                ..
            } if content == "# Rendered preview"
        );
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(ready);
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Off { request_id: 2 },
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_preview_key_ignores_non_markdown_selection() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
            scroll_offset: 3,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Off { request_id: 0 },
                scroll_offset: 3,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_selection_change_keeps_preview_sticky_for_unsupported_row() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/docs/README.md b/docs/README.md\n+preview".to_string(),
            file_explorer_selected_index: 1,
            preview: DiffPreview::Ready {
                content: "# Preview".to_string(),
                path: "docs/README.md".to_string(),
                request_id: 4,
            },
            restore: None,
            scroll_cache: None,
            scroll_offset: 6,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                file_explorer_selected_index: 0,
                preview: DiffPreview::Unsupported { request_id: 5 },
                scroll_offset: 0,
                ..
            }
        ));
    }

    #[tokio::test]
    async fn test_handle_selection_change_reloads_next_markdown_file() {
        // Arrange
        let mut mock_git_client = ag_git::MockGitClient::new();
        mock_git_client
            .expect_read_worktree_file()
            .withf(|_, path| path == "SECOND.MD")
            .times(1)
            .returning(|_, _| {
                Box::pin(async { Ok(ag_git::WorktreeFileContent::Text("# Second".to_string())) })
            });
        let (mut app, _base_dir) = preview_test_app(mock_git_client).await;
        app.mode = AppMode::Diff {
            diff: concat!(
                "diff --git a/README.md b/README.md\n+first\n",
                "diff --git a/SECOND.MD b/SECOND.MD\n+second\n",
            )
            .to_string(),
            file_explorer_selected_index: 0,
            preview: DiffPreview::Ready {
                content: "# First".to_string(),
                path: "README.md".to_string(),
                request_id: 2,
            },
            restore: None,
            scroll_cache: None,
            scroll_offset: 4,
            session_id: "session-id".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
        );
        let preview_event = next_diff_preview_event(&mut app).await;

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                file_explorer_selected_index: 1,
                preview: DiffPreview::Loading {
                    ref path,
                    request_id: 3,
                },
                scroll_offset: 0,
                ..
            } if path == "SECOND.MD"
        ));
        assert!(matches!(
            preview_event,
            AppEvent::DiffPreviewLoaded { ref path, .. } if path == "SECOND.MD"
        ));
    }

    #[tokio::test]
    async fn test_handle_preview_key_reports_missing_session_worktree() {
        // Arrange
        let (mut app, _base_dir) = crate::test_support::new_test_app().await;
        app.mode = AppMode::Diff {
            diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            preview: DiffPreview::default(),
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            session_id: "missing-session".into(),
        };

        // Act
        handle(
            &mut app,
            TEST_TERMINAL_SIZE,
            KeyEvent::new(KeyCode::Char('p'), KeyModifiers::NONE),
        );

        // Assert
        assert!(matches!(
            app.mode,
            AppMode::Diff {
                preview: DiffPreview::Unavailable {
                    reason: DiffPreviewUnavailableReason::LoadFailed(ref error),
                    ..
                },
                ..
            } if error == "Session worktree is unavailable"
        ));
    }

    #[test]
    fn test_diff_max_scroll_offset_returns_cached_value_on_matching_key() {
        // Arrange — a cache entry whose key matches the requested viewport and
        // selection.
        let diff = scrollable_diff_fixture();
        let diff_layout_cache = page::diff::DiffLayoutCache::default();
        let markdown_render_cache = crate::ui::markdown::MarkdownRenderCache::default();
        let preview = DiffPreview::default();
        let mut scroll_cache = Some(DiffScrollCache {
            content_area: viewport_rect(TEST_TERMINAL_SIZE),
            file_explorer_selected_index: 0,
            max_scroll_offset: 4242,
        });

        // Act
        let max_scroll_offset = diff_max_scroll_offset(
            &diff,
            TEST_TERMINAL_SIZE,
            0,
            &mut scroll_cache,
            &diff_layout_cache,
            &markdown_render_cache,
            &preview,
        );

        // Assert — the cached limit is returned verbatim without recomputing.
        assert_eq!(max_scroll_offset, 4242);
    }

    #[test]
    #[should_panic(expected = "expected AppMode::Prompt after leaving diff")]
    fn test_assert_restored_prompt_composer_rejects_non_prompt_mode() {
        // Arrange, Act & Assert — the helper rejects modes that are not a restored
        // composer.
        assert_restored_prompt_composer(&AppMode::List);
    }
}
