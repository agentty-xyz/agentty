use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use tempfile::tempdir;

use super::super::{
    PromptContext, PromptEffect, PromptInputMode, PromptKeyStep, apply_prompt_effects,
    handle_with_cache, prompt_input_mode, reduce_prompt_cancel, reduce_prompt_key,
    reduce_prompt_slash_submit, reduce_prompt_submit, take_submitted_turn_prompt,
};
use crate::app::App;
use crate::app::prompt_intent::PromptSessionMode;
use crate::domain::agent::{AgentKind, ReasoningLevel, ResponseStyle};
use crate::domain::input::InputState;
use crate::domain::permission::PermissionMode;
use crate::domain::session_message::SessionTranscript;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::Database;
use crate::infra::fs;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashCommandSet,
    PromptSlashState, insert_prompt_local_image_at,
};
use crate::test_support::setup_test_git_repo;
use crate::ui::RenderCacheStore;

/// Arranges attachments through the app-layer composer insertion.
pub(super) trait PromptTestAppExt {
    fn insert_pasted_image_placeholder(&mut self, local_image_path: PathBuf);
}

impl PromptTestAppExt for App {
    fn insert_pasted_image_placeholder(&mut self, local_image_path: PathBuf) {
        let _ = self.insert_prompt_image_placeholder(local_image_path, None);
    }
}

/// Arranges attachments and drains submissions without application services.
pub(super) trait PromptTestModeExt {
    fn insert_pasted_image_placeholder(&mut self, local_image_path: PathBuf);
    fn take_submitted_turn_prompt(&mut self) -> TurnPrompt;
}

impl PromptTestModeExt for AppMode {
    fn insert_pasted_image_placeholder(&mut self, local_image_path: PathBuf) {
        let AppMode::Prompt {
            attachment_state,
            history_state,
            input,
            slash_state,
            ..
        } = self
        else {
            unreachable!("prompt fixture required");
        };
        let anchor = input.cursor;
        insert_prompt_local_image_at(
            attachment_state,
            history_state,
            input,
            slash_state,
            local_image_path,
            anchor,
        );
    }

    fn take_submitted_turn_prompt(&mut self) -> TurnPrompt {
        let (prompt, _) = take_submitted_turn_prompt(self);

        prompt
    }
}

pub(super) fn session_replay_text(session: &crate::domain::session::Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

/// Applies queued app events through the first completed full-diff load.
pub(super) async fn apply_next_session_diff(app: &mut App) {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), app.next_app_event())
            .await
            .expect("session diff event should arrive")
            .expect("app event channel should remain open");
        let is_session_diff = matches!(event, crate::app::AppEvent::SessionDiffLoaded { .. });
        app.apply_single_app_event(event).await;
        if is_session_diff {
            return;
        }
    }
}

/// Applies queued app events through the first completed clipboard-image
/// capture.
pub(super) async fn apply_next_prompt_image_paste(app: &mut App) {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(10), app.next_app_event())
            .await
            .expect("prompt image paste event should arrive")
            .expect("app event channel should remain open");
        let is_prompt_image_paste = matches!(event, crate::app::AppEvent::PromptImagePasted { .. });
        app.apply_single_app_event(event).await;
        if is_prompt_image_paste {
            return;
        }
    }
}

/// Replaces the app-level Git boundary, preserving other shared dependencies.
pub(super) fn install_mock_git_client(app: &mut App, mock_git_client: ag_git::MockGitClient) {
    app.services.set_git_client(Arc::new(mock_git_client));
}

/// Replaces the app-level clipboard-image dependency with one
/// caller-provided mock.
pub(super) fn install_mock_clipboard_image_client(
    app: &mut App,
    mock_clipboard_image_client: crate::infra::clipboard_image::MockClipboardImageClient,
) {
    app.services
        .set_clipboard_image_client(Arc::new(mock_clipboard_image_client));
}

/// Replaces the app-level filesystem dependency with a caller-provided
/// mock.
pub(super) fn install_mock_fs_client(app: &mut App, mock_fs_client: fs::MockFsClient) {
    app.services.set_fs_client(Arc::new(mock_fs_client));
}

/// Builds prompt state without application startup, persistence, or a worktree.
pub(super) fn new_test_prompt_mode(input_text: &str) -> AppMode {
    AppMode::Prompt {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(Vec::new()),
        slash_state: PromptSlashState::default(),
        session_id: "prompt-test-session".into(),
        input: InputState::with_text(input_text.to_string()),
        scroll_offset: None,
    }
}

/// Composer text width the prompt handler derives from [`test_terminal`].
pub(super) const TEST_INPUT_WIDTH: u16 = 118;

/// Builds session facts for an idle existing session matching
/// [`new_test_prompt_mode`], as a freshly created test session reports them.
pub(super) fn new_test_prompt_context() -> PromptContext {
    PromptContext {
        input_mode: PromptInputMode::Text,
        permission_mode: PermissionMode::default(),
        reasoning_level: ReasoningLevel::default(),
        response_style: ResponseStyle::default(),
        scroll_offset: None,
        selected_session_agent_kind: AgentKind::Codex,
        session_agent_kind: AgentKind::Codex,
        session_id: "prompt-test-session".into(),
        session_index: 0,
        session_mode: PromptSessionMode::Existing,
        sign_in_agent: None,
        slash_command_set: PromptSlashCommandSet::WithoutApply,
    }
}

/// Re-derives the input sub-mode from `mode`, as the handler does before
/// each key.
pub(super) fn refresh_test_input_mode(mode: &AppMode, context: &mut PromptContext) {
    let AppMode::Prompt {
        at_mention_state,
        input,
        slash_state,
        ..
    } = mode
    else {
        unreachable!("prompt fixture required");
    };

    context.input_mode = prompt_input_mode(at_mention_state.as_ref(), input, slash_state);
}

/// Reduces one key over `mode` with `context`, refreshing its input sub-mode
/// first.
pub(super) fn reduce_test_prompt_key(
    mode: &mut AppMode,
    context: &mut PromptContext,
    key: KeyEvent,
) -> PromptKeyStep {
    refresh_test_input_mode(mode, context);

    reduce_prompt_key(mode, context, TEST_INPUT_WIDTH, key)
}

/// Reduces one key event over `mode` for an idle existing session and
/// returns its app effects.
pub(super) fn reduce_prompt_event(mode: &mut AppMode, key: KeyEvent) -> Vec<PromptEffect> {
    expect_prompt_effects(reduce_test_prompt_key(
        mode,
        &mut new_test_prompt_context(),
        key,
    ))
}

/// Reduces one unmodified key over `mode` for an idle existing session and
/// returns its app effects.
pub(super) fn reduce_prompt_code(mode: &mut AppMode, code: KeyCode) -> Vec<PromptEffect> {
    reduce_prompt_event(mode, KeyEvent::new(code, event::KeyModifiers::NONE))
}

/// Returns the app effects of one reduction, panicking on a transcript
/// scroll.
pub(super) fn expect_prompt_effects(step: PromptKeyStep) -> Vec<PromptEffect> {
    let PromptKeyStep::Effects(effects) = step else {
        unreachable!("expected app effects");
    };

    effects
}

/// Reduces one prompt submission and applies its app effects.
pub(super) async fn run_prompt_submit(app: &mut App, prompt_context: &PromptContext) {
    let mut effects = Vec::new();
    reduce_prompt_submit(&mut app.mode, prompt_context, &mut effects);

    apply_prompt_effects(app, prompt_context, effects).await;
}

/// Reduces one slash-menu submission and applies its app effects.
pub(super) async fn run_prompt_slash_submit(app: &mut App, prompt_context: &PromptContext) {
    let mut effects = Vec::new();
    reduce_prompt_slash_submit(&mut app.mode, prompt_context, &mut effects);

    apply_prompt_effects(app, prompt_context, effects).await;
}

/// Reduces one prompt cancellation and applies its app effects.
pub(super) async fn run_prompt_cancel(app: &mut App, prompt_context: &PromptContext) {
    let mut effects = Vec::new();
    reduce_prompt_cancel(&mut app.mode, prompt_context, &mut effects);

    apply_prompt_effects(app, prompt_context, effects).await;
}

/// Returns the composer text and cursor, panicking outside prompt mode.
pub(super) fn mode_input(mode: &AppMode) -> (String, usize) {
    let AppMode::Prompt { input, .. } = mode else {
        unreachable!("expected AppMode::Prompt");
    };

    (input.text().to_string(), input.cursor)
}

/// Returns the composer focus, panicking outside prompt mode.
pub(super) fn mode_focus(mode: &AppMode) -> ChatFocus {
    let AppMode::Prompt { focus, .. } = mode else {
        unreachable!("expected AppMode::Prompt");
    };

    *focus
}

pub(super) async fn new_test_prompt_app(
    input_text: &str,
    at_mention_state: Option<PromptAtMentionState>,
) -> (App, tempfile::TempDir) {
    let (app, base_dir, _) =
        new_test_prompt_app_with_session_mode(input_text, at_mention_state, false).await;

    (app, base_dir)
}

/// Builds one prompt-mode test app backed by either an immediate-start or
/// explicit draft session.
pub(super) async fn new_test_prompt_app_with_session_mode(
    input_text: &str,
    at_mention_state: Option<PromptAtMentionState>,
    is_draft_session: bool,
) -> (App, tempfile::TempDir, sqlx::SqlitePool) {
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    setup_test_git_repo(base_dir.path());
    let database = Database::in_memory()
        .await
        .expect("failed to open in-memory db");
    let pool = database.pool().clone();
    let mut app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        crate::test_support::test_app_clients(),
    )
    .await
    .expect("failed to build app");

    let session_id = if is_draft_session {
        app.create_draft_session()
            .await
            .expect("failed to create draft session")
    } else {
        app.create_session()
            .await
            .expect("failed to create session")
    };
    app.mode = AppMode::Prompt {
        at_mention_state,
        attachment_state: PromptAttachmentState::default(),
        focus: ChatFocus::Input,
        history_state: PromptHistoryState::new(Vec::new()),
        slash_state: PromptSlashState::default(),
        session_id: session_id.into(),
        input: InputState::with_text(input_text.to_string()),
        scroll_offset: None,
    };

    (app, base_dir, pool)
}

/// Builds one prompt-mode test app whose active session uses the explicit
/// staged-draft workflow.
pub(super) async fn new_test_draft_prompt_app(
    input_text: &str,
    at_mention_state: Option<PromptAtMentionState>,
) -> (App, tempfile::TempDir) {
    let (app, base_dir, _) =
        new_test_prompt_app_with_session_mode(input_text, at_mention_state, true).await;

    (app, base_dir)
}

/// Waits until the app emits an `AtMentionEntriesLoaded` event and skips
/// unrelated background events produced during startup.
pub(super) async fn wait_for_at_mention_entries_event(app: &mut App) -> crate::app::AppEvent {
    let timeout = std::time::Duration::from_secs(1);
    let deadline = tokio::time::Instant::now() + timeout;

    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let next_event = tokio::time::timeout(remaining, app.next_app_event())
            .await
            .expect("at-mention event should arrive")
            .expect("at-mention event channel closed unexpectedly");

        if matches!(
            next_event,
            crate::app::AppEvent::AtMentionEntriesLoaded { .. }
        ) {
            return next_event;
        }
    }
}

/// Builds a terminal large enough to render the composer and transcript.
pub(super) fn test_terminal() -> Terminal<ratatui::backend::TestBackend> {
    let backend = ratatui::backend::TestBackend::new(120, 30);

    Terminal::new(backend).expect("failed to create terminal")
}

/// Returns the current composer focus, panicking outside prompt mode.
pub(super) fn prompt_focus(app: &App) -> ChatFocus {
    mode_focus(&app.mode)
}

/// Sends one plain key press through the prompt-mode handler.
pub(super) async fn press_prompt_key(app: &mut App, code: KeyCode) {
    let mut terminal = test_terminal();

    handle_with_cache(
        app,
        &RenderCacheStore::default(),
        &mut terminal,
        KeyEvent::new(code, event::KeyModifiers::NONE),
    )
    .await
    .expect("prompt key handling failed");
}
