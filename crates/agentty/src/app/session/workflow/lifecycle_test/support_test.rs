use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use ag_agent as agent;
use ag_agent::{MockOneShotClient, OneShotClient};
use ag_forge as forge;
use ag_git as git;
use ag_protocol::AgentResponse;
use async_trait::async_trait;
use sqlx::SqlitePool;
use tokio::sync::{Notify, mpsc};

use super::super::super::session_branch;
use super::super::SessionTitleGenerationTaskInput;
use crate::app::session::SessionDefaults;
use crate::app::{AppEvent, AppServices, SessionManager, SessionState};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::selection::SelectionState;
use crate::domain::session::{
    ForgeKind, ReviewRequestState, ReviewRequestSummary, Session, SessionHandles, SessionId, Status,
};
use crate::infra::clock::RealClock;
use crate::infra::db::AppRepositories;
use crate::infra::{db, fs};

/// One-shot boundary that holds title generation until the test releases
/// it.
pub(super) struct DelayedTitleClient {
    pub(super) release: Arc<Notify>,
}

#[async_trait]
impl OneShotClient for DelayedTitleClient {
    async fn submit(
        &self,
        _request: agent::OneShotRequest,
    ) -> Result<agent::OneShotSubmission, agent::OneShotError> {
        self.release.notified().await;

        Ok(agent::OneShotSubmission {
            response: AgentResponse::plain("Assess project quality"),
            stats: agent::SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: agent::SessionDiffState::Unknown,
                input_tokens: 0,
                output_tokens: 0,
            },
        })
    }
}

/// Builds a one-shot boundary that returns one deterministic title
/// response.
pub(super) fn mock_title_client(response: &str) -> Arc<dyn OneShotClient> {
    let response = response.to_string();
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(1)
        .returning(move |_| {
            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain(response.clone()),
                stats: agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    Arc::new(one_shot_client)
}

/// Builds one standard provisional-title generation request for tests.
pub(super) fn title_generation_task_input(
    app_event_tx: mpsc::UnboundedSender<AppEvent>,
    database: AppRepositories,
    one_shot_client: Arc<dyn OneShotClient>,
    prompt: &str,
) -> SessionTitleGenerationTaskInput {
    SessionTitleGenerationTaskInput {
        app_event_tx,
        db: database,
        folder: PathBuf::from("/tmp/session"),
        latest_request: prompt.to_string(),
        one_shot_client,
        requires_provisional_title: true,
        reasoning_level: ReasoningLevel::Low,
        session_agent: AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
        session_id: SessionId::from("session-id"),
        speed_mode: SpeedMode::Normal,
        tracked_generation: None,
    }
}

/// Builds a session manager with one session for reply-context tests.
pub(super) fn session_manager_with_one_session(session: Session) -> SessionManager {
    let mut handles = HashMap::new();
    handles.insert(
        session.id.clone(),
        SessionHandles::new_with_transcript(
            session.status,
            session.transcript.clone().unwrap_or_default(),
        ),
    );

    let state = SessionState::new(
        handles,
        vec![session],
        SelectionState::default(),
        Arc::new(RealClock),
        1,
        0,
    );

    SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    )
}

/// Builds a minimal in-memory session snapshot for lifecycle unit tests.
pub(super) fn test_session(
    prompt: &str,
    status: Status,
    title: Option<&str>,
    output: &str,
) -> Session {
    crate::test_support::SessionFixtureBuilder::new()
        .agent(crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Claude,
            AgentModel::ClaudeSonnet5,
        ))
        .folder(PathBuf::from("/tmp/session"))
        .transcript(output)
        .prompt(prompt)
        .status(status)
        .title(title.map(ToString::to_string))
        .build()
}

/// Builds a filesystem mock that delegates simple checks to local disk.
pub(super) fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_create_dir_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_remove_dir_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_read_file()
        .times(0..)
        .returning(|path| {
            Box::pin(async move { tokio::fs::read(path).await.map_err(fs::FsError::from) })
        });
    mock_fs_client
        .expect_remove_file()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_exists()
        .times(0..)
        .returning(|path| path.exists());
    mock_fs_client
        .expect_is_dir()
        .times(0..)
        .returning(|path| path.is_dir());

    mock_fs_client
}

/// Persists one session row that matches the in-memory fixture.
pub(super) async fn database_with_session(session: &Session) -> AppRepositories {
    let (database, _pool) = database_with_session_and_pool(session).await;

    database
}

/// Persists one review session whose visible title remains provisional.
pub(super) async fn provisional_title_database(title: &str) -> (AppRepositories, SqlitePool) {
    let session = test_session(title, Status::Review, Some(title), "");
    let (database, pool) = database_with_session_and_pool(&session).await;
    database
        .sessions()
        .update_session_provisional_title(&session.id, title)
        .await
        .expect("failed to persist provisional title");

    (database, pool)
}

/// Persists one session row and returns its pool for failure-path tests.
pub(super) async fn database_with_session_and_pool(
    session: &Session,
) -> (AppRepositories, SqlitePool) {
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    if session.is_draft {
        database
            .sessions()
            .insert_draft_session(
                &session.id,
                session.agent.model().as_str(),
                &session.base_branch,
                &session.status.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert draft session");
    } else {
        database
            .sessions()
            .insert_session(
                &session.id,
                session.agent.model().as_str(),
                &session.base_branch,
                &session.status.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert session");
    }
    database
        .sessions()
        .update_session_prompt(&session.id, &session.prompt)
        .await
        .expect("failed to persist session prompt");
    if let Some(title) = &session.title {
        database
            .sessions()
            .update_session_title(&session.id, title)
            .await
            .expect("failed to persist session title");
    }
    if let Some(review_request) = &session.review_request {
        database
            .reviews()
            .update_session_review_request(&session.id, Some(review_request.clone()))
            .await
            .expect("failed to persist session review request");
    }

    (database, pool)
}

/// Builds app services with caller-provided filesystem, git, and forge
/// boundaries.
pub(super) fn test_services_with_fs_client(
    database: &AppRepositories,
    clock: Arc<dyn crate::infra::clock::Clock>,
    fs_client: Arc<dyn fs::FsClient>,
    git_client: Arc<dyn git::GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
) -> AppServices {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    AppServices::new_with_agent_clis(
        PathBuf::from("/tmp/agentty-tests"),
        clock,
        event_tx,
        crate::app::service::AppServiceDeps {
            app_server_client_override: Some(crate::test_support::mock_app_server()),
            available_agent_kinds: AgentKind::ALL.to_vec(),
            clipboard_image_client_override: None,
            fs_client,
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: database.clone(),
            review_request_client,
        },
        crate::domain::agent::AgentCliInfo::from_kinds(AgentKind::ALL),
    )
}

/// Builds app services with caller-provided git and forge boundaries.
pub(super) fn test_services(
    database: &AppRepositories,
    git_client: Arc<dyn git::GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
) -> AppServices {
    test_services_with_fs_client(
        database,
        Arc::new(crate::infra::clock::RealClock),
        Arc::new(create_passthrough_mock_fs_client()),
        git_client,
        review_request_client,
    )
}

/// Builds app services plus an event receiver for reducer-event
/// assertions.
pub(super) fn test_services_with_event_receiver(
    database: &AppRepositories,
    git_client: Arc<dyn git::GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
) -> (AppServices, mpsc::UnboundedReceiver<AppEvent>) {
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    let services = AppServices::new_with_agent_clis(
        PathBuf::from("/tmp/agentty-tests"),
        Arc::new(crate::infra::clock::RealClock),
        event_tx,
        crate::app::service::AppServiceDeps {
            app_server_client_override: Some(crate::test_support::mock_app_server()),
            available_agent_kinds: AgentKind::ALL.to_vec(),
            clipboard_image_client_override: None,
            fs_client: Arc::new(create_passthrough_mock_fs_client()),
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: database.clone(),
            review_request_client,
        },
        crate::domain::agent::AgentCliInfo::from_kinds(AgentKind::ALL),
    );

    (services, event_rx)
}

/// Builds one normalized review-request summary for workflow tests.
pub(super) fn review_request_summary(display_id: &str) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: display_id.to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: session_branch("session-id"),
        state: ReviewRequestState::Open,
        status_summary: Some("Checks pending".to_string()),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
        web_url: format!(
            "https://github.com/agentty-xyz/agentty/pull/{}",
            &display_id[1..]
        ),
    }
}

/// Loads the persisted session row used by workflow assertions.
pub(super) async fn load_persisted_session_row(database: &AppRepositories) -> db::SessionRow {
    database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load session rows")
        .into_iter()
        .find(|row| row.id == "session-id")
        .expect("session row should exist")
}

/// Expects rollback to remove both the worktree registration and branch.
pub(super) fn rollback_git_client() -> git::MockGitClient {
    let mut client = git::MockGitClient::new();
    client
        .expect_remove_worktree()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    client
        .expect_delete_branch()
        .once()
        .returning(|_, _| Box::pin(async { Ok(()) }));

    client
}

/// Builds a session manager containing the supplied sessions with no
/// pre-selected row.
pub(super) fn session_manager_with_sessions(sessions: Vec<Session>) -> SessionManager {
    let mut handles = HashMap::new();
    for session in &sessions {
        handles.insert(
            session.id.clone(),
            SessionHandles::new_with_transcript(
                session.status,
                session.transcript.clone().unwrap_or_default(),
            ),
        );
    }
    let row_count = i64::try_from(sessions.len()).unwrap_or(0);
    let state = SessionState::new(
        handles,
        sessions,
        SelectionState::default(),
        Arc::new(RealClock),
        row_count,
        0,
    );

    SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    )
}

/// Returns one session with a custom identifier and status for navigation
/// tests.
pub(super) fn session_with_id(id: &str, status: Status) -> Session {
    let mut session = test_session("prompt", status, None, "");
    session.id = id.to_string().into();

    session
}
