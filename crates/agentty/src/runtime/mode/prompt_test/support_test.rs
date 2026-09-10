use std::path::{Path, PathBuf};
use std::process::Command;

use crossterm::event;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Terminal;
use tempfile::tempdir;

use super::super::{
    handle_with_cache, insert_pasted_image_placeholder, take_submitted_turn_prompt,
};
use crate::app::App;
use crate::domain::agent::AgentCliInfo;
use crate::domain::input::InputState;
use crate::domain::session_message::SessionTranscript;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::Database;
use crate::infra::fs;
use crate::presentation::app_mode::{AppMode, ChatFocus};
use crate::presentation::prompt::{
    PromptAtMentionState, PromptAttachmentState, PromptHistoryState, PromptSlashState,
};
use crate::ui::RenderCacheStore;

pub(super) trait PromptTestAppExt {
    fn insert_pasted_image_placeholder(&mut self, local_image_path: PathBuf);
    fn take_submitted_turn_prompt(&mut self) -> TurnPrompt;
}

impl PromptTestAppExt for App {
    fn insert_pasted_image_placeholder(&mut self, local_image_path: PathBuf) {
        let _ = insert_pasted_image_placeholder(self, local_image_path);
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
        app.apply_app_events(event).await;
        if is_session_diff {
            return;
        }
    }
}

/// Replaces the app-level git client with a caller-provided mock by
/// rebuilding `AppServices` through its public constructor, preserving
/// the remaining shared dependencies.
pub(super) fn install_mock_git_client(app: &mut App, mock_git_client: ag_git::MockGitClient) {
    let mock_git_client: std::sync::Arc<dyn ag_git::GitClient> =
        std::sync::Arc::new(mock_git_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis = AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let clipboard_image_client_override = Some(app.services.clipboard_image_client());
    let fs_client = app.services.fs_client();
    let review_request_client = app.services.review_request_client();

    app.services = crate::app::AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        crate::app::test_support::AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override,
            fs_client,
            git_client: mock_git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Replaces the app-level clipboard-image dependency with one
/// caller-provided mock.
pub(super) fn install_mock_clipboard_image_client(
    app: &mut App,
    mock_clipboard_image_client: crate::infra::clipboard_image::MockClipboardImageClient,
) {
    let clipboard_image_client: std::sync::Arc<
        dyn crate::infra::clipboard_image::ClipboardImageClient,
    > = std::sync::Arc::new(mock_clipboard_image_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis = AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let fs_client = app.services.fs_client();
    let git_client = app.services.git_client();
    let review_request_client = app.services.review_request_client();

    app.services = crate::app::AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        crate::app::test_support::AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: Some(clipboard_image_client),
            fs_client,
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Replaces the app-level filesystem dependency with a caller-provided
/// mock.
pub(super) fn install_mock_fs_client(app: &mut App, mock_fs_client: fs::MockFsClient) {
    let fs_client: std::sync::Arc<dyn fs::FsClient> = std::sync::Arc::new(mock_fs_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis = AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let clipboard_image_client_override = Some(app.services.clipboard_image_client());
    let git_client = app.services.git_client();
    let review_request_client = app.services.review_request_client();

    app.services = crate::app::AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        crate::app::test_support::AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override,
            fs_client,
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

pub(super) fn setup_test_git_repo(path: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init failed");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .expect("git config failed");
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .expect("git config failed");
    std::fs::write(path.join("README.md"), "test").expect("write failed");
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()
        .expect("git commit failed");
    Command::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(path)
        .output()
        .expect("git branch failed");
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
    let database = Database::open_in_memory()
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
    let AppMode::Prompt { focus, .. } = &app.mode else {
        unreachable!("expected AppMode::Prompt");
    };

    *focus
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
