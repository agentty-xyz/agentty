use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::layout::Rect;

use super::super::handle_with_cache;
use crate::app::{App, AppEvent};
use crate::domain::agent::AgentKind;
use crate::domain::input::InputState;
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffRestoreTarget, DiffSidebarFocus,
    PromptModeSnapshot,
};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashStage,
    PromptSlashState,
};
use crate::runtime::EventResult;
use crate::ui::RenderCacheStore;

pub(super) const TEST_TERMINAL_SIZE: Rect = Rect::new(0, 0, 80, 12);

/// Builds an app with one previewable session and injected git boundary.
pub(super) async fn preview_test_app(
    mock_git_client: ag_git::MockGitClient,
) -> (App, tempfile::TempDir) {
    let clients =
        crate::test_support::test_app_clients().with_git_client(Arc::new(mock_git_client));
    let (mut app, base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
    let session = crate::test_support::SessionFixtureBuilder::new()
        .id("session-id")
        .folder(base_dir.path().to_path_buf())
        .build();
    app.sessions =
        crate::test_support::session_manager_with_handles(vec![session], HashMap::new()).into();

    (app, base_dir)
}

/// Waits through unrelated startup events for one diff-preview result.
pub(super) async fn next_diff_preview_event(app: &mut App) -> AppEvent {
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
pub(super) fn scrollable_diff_fixture() -> String {
    format!(
        "diff --git a/src/main.rs b/src/main.rs\n@@ -0,0 +1,40 @@\n{}",
        (0..40)
            .map(|index| format!("+line {index}"))
            .collect::<Vec<_>>()
            .join("\n")
    )
}

/// Returns eight file rows whose last file starts changing at line 33.
pub(super) fn aligned_file_diff_fixture() -> String {
    let preceding_files = ('a'..='g')
        .map(|file_name| {
            format!("diff --git a/{file_name}.rs b/{file_name}.rs\n@@ -0,0 +1 @@\n+seed")
        })
        .collect::<Vec<_>>()
        .join("\n");
    let selected_file_lines = (33..=42)
        .map(|line_number| format!("+line {line_number}"))
        .collect::<Vec<_>>()
        .join("\n");

    format!("{preceding_files}\ndiff --git a/h.rs b/h.rs\n@@ -0,0 +33,10 @@\n{selected_file_lines}")
}

/// Builds a diff-mode snapshot for focused navigation tests.
pub(super) fn diff_mode_fixture(
    diff: &str,
    file_explorer_selected_index: usize,
    focus: DiffFocus,
    preview: DiffPreview,
) -> AppMode {
    AppMode::Diff {
        diff: diff.to_string(),
        file_explorer_selected_index,
        focus,
        line_comments: DiffLineComments::default(),
        preview,
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session_id: "session-id".into(),
    }
}

/// Draft text carried by [`non_default_prompt_snapshot`].
pub(super) const RESTORE_DRAFT_TEXT: &str = "draft body";

/// The single at-mention entry carried by [`non_default_prompt_snapshot`].
pub(super) fn prompt_mention_entry() -> crate::domain::file_entry::FileEntry {
    crate::domain::file_entry::FileEntry {
        is_dir: false,
        path: "src/main.rs".to_string(),
    }
}

/// Builds a prompt snapshot with non-default attachment, history, slash,
/// and at-mention state so restore tests prove no composer field is
/// dropped when leaving diff.
pub(super) fn non_default_prompt_snapshot() -> crate::presentation::app_mode::PromptModeSnapshot {
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
pub(super) fn assert_restored_prompt_composer(mode: &AppMode) {
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

pub(super) fn handle(app: &mut App, content_area: Rect, key: KeyEvent) -> EventResult {
    handle_with_cache(app, &RenderCacheStore::default(), content_area, key)
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
    sidebar_focus: DiffSidebarFocus,
) {
    let session_id = session_id.into();
    let mut review_comments = app.start_session_review_comment_load(&session_id);
    if let Some(review_comments) = &mut review_comments {
        review_comments.sidebar_focus = sidebar_focus;
    }
    let line_comments = app
        .diff_comment_progress
        .remove(&session_id)
        .unwrap_or_default();

    app.mode = AppMode::Diff {
        diff,
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments,
        preview: DiffPreview::default(),
        review_comments,
        restore: restore.map(Box::new),
        scroll_cache: None,
        selected_diff_line_index: 0,
        session_id,
        scroll_offset: 0,
    };
}
