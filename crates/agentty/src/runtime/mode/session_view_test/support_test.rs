use std::io;
use std::sync::Arc;

use crossterm::event::KeyEvent;
use ratatui::Terminal;
use ratatui::backend::Backend;

use super::super::{ViewActionState, ViewSessionSnapshot, handle_with_cache};
use crate::app::{App, AppEvent};
use crate::domain::session::{
    ForgeKind, QueuedMessage, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId,
    Status,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::tmux::{MockTmuxClient, TmuxClient};
use crate::presentation::help_action::ViewSessionState;
use crate::runtime::EventResult;
use crate::ui::RenderCacheStore;

pub(super) fn queued_message(order: u64, text: &str) -> QueuedMessage {
    QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
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
        let is_session_diff = matches!(event, AppEvent::SessionDiffLoaded { .. });
        app.apply_app_events(event).await;
        if is_session_diff {
            return;
        }
    }
}

/// Applies a deterministic completion for the one pending session-diff
/// request.
pub(super) async fn apply_pending_session_diff(
    app: &mut App,
    session_id: &SessionId,
    result: Result<&str, &str>,
) {
    let request_id = app
        .pending_session_diff_requests
        .keys()
        .copied()
        .next()
        .expect("session diff request should be pending");
    app.apply_app_events(AppEvent::SessionDiffLoaded {
        request_id,
        result: result.map(str::to_string).map_err(str::to_string),
        session_id: session_id.clone(),
    })
    .await;
}

/// Builds one git-backed test app with one created session and an
/// injected tmux boundary.
pub(super) async fn new_test_app_with_session_and_tmux_client(
    tmux_client: Arc<dyn TmuxClient>,
) -> (App, tempfile::TempDir, String) {
    let (mut app, base_dir) =
        crate::test_support::new_git_test_app_with_tmux_client(tmux_client).await;
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");

    (app, base_dir, session_id)
}

/// Builds one git-backed test app with one created session and a strict
/// mocked tmux boundary.
pub(super) async fn new_test_app_with_session() -> (App, tempfile::TempDir, String) {
    new_test_app_with_session_and_tmux_client(Arc::new(MockTmuxClient::new())).await
}

/// Attaches one open GitHub review request to a session fixture.
pub(super) fn attach_open_review_request(session: &mut crate::domain::session::Session) {
    session.review_request = Some(ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/linked-terminal".to_string(),
            state: ReviewRequestState::Open,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Linked terminal session".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    });
}

/// Builds one reply-enabled review snapshot for primary-key routing tests.
pub(super) fn reply_enabled_review_snapshot() -> ViewSessionSnapshot {
    ViewSessionSnapshot {
        branch_actions: ViewActionState::Enabled,
        continue_terminal_session: ViewActionState::Disabled,
        fork_session: ViewActionState::Enabled,
        inspect_diff: ViewActionState::Enabled,
        is_managed: false,
        is_orchestrator: false,
        merge_session_branch: ViewActionState::Enabled,
        mutate_session_branch: ViewActionState::Enabled,
        rebase_session_branch: ViewActionState::Enabled,
        open_worktree: ViewActionState::Enabled,
        reply_to_session: ViewActionState::Enabled,
        review_comments: ViewActionState::Disabled,
        start_staged_session: ViewActionState::Disabled,
        follow_up_task_action: None,
        publish_pull_request_action: None,
        session_state: ViewSessionState::Review,
        session_status: Status::Review,
    }
}

/// Replaces the app-level clipboard-image dependency with one
/// caller-provided mock.
pub(super) fn install_mock_clipboard_image_client(
    app: &mut App,
    mock_clipboard_image_client: crate::infra::clipboard_image::MockClipboardImageClient,
) {
    let clipboard_image_client: Arc<dyn crate::infra::clipboard_image::ClipboardImageClient> =
        Arc::new(mock_clipboard_image_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
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

/// Builds one minimal session snapshot for pure view-state tests.
pub(super) fn session_fixture(status: Status, is_draft: bool) -> crate::domain::session::Session {
    crate::test_support::SessionFixtureBuilder::new()
        .status(status)
        .draft(is_draft)
        .folder(std::env::temp_dir())
        .project_name("")
        .build()
}

pub(super) async fn handle<B: Backend>(
    app: &mut App,
    terminal: &mut Terminal<B>,
    key: KeyEvent,
) -> io::Result<EventResult>
where
    B::Error: std::error::Error + Send + Sync + 'static,
{
    handle_with_cache(app, &RenderCacheStore::default(), terminal, key).await
}
