use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::{AgentDefinitionPrompt, AgentRequestKind, MockAgentChannel};
use ag_session::{EnvId, ResolvedEnv, SessionStats, test_support as model_fixture};
use ag_worker::SessionRunClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::worker::{SessionWorkerContext, TurnMetadata};
use super::{
    READ_ONLY_CHAT_PROMPT, load_session_permission_mode, load_session_response_style,
    load_title_reasoning_level, prepare_turn_request,
};
use crate::app::service::HarnessModels;
use crate::app::session::SessionError;
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::domain::session_message::SessionTranscript;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{
    AppRepositories, DbError, LEGACY_SESSION_AGENT_CLEAR_MARKER, PersistedSessionCreation,
    SessionTurnMetadata,
};

#[tokio::test]
async fn resumed_upgrade_clears_legacy_instructions_until_successful_persistence() {
    // Arrange
    let (context, _) = persisted_turn_test_context().await;
    context
        .db
        .sessions()
        .update_session_provider_conversation_id("worker", Some("old-context".into()))
        .await
        .expect("conversation");
    context
        .db
        .sessions()
        .update_session_instruction_conversation_id(
            "worker",
            Some(LEGACY_SESSION_AGENT_CLEAR_MARKER.into()),
        )
        .await
        .expect("upgrade marker");
    let metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: context.session_agent,
    };
    let prompt = TurnPrompt::from_text("Continue the work".into());

    for _ in 0..2 {
        // Act: preparing or failing a turn must not consume the marker.
        let prepared = prepare_turn_request(
            &context,
            &metadata,
            AgentRequestKind::SessionResume,
            Some("Ordinary chat history".into()),
            &prompt,
            &resolved_turn_test_env(),
        )
        .await
        .expect("resumed turn");

        // Assert
        assert_eq!(
            prepared.request.agent_profile,
            AgentDefinitionPrompt::cleared(true)
        );
        assert_eq!(
            prepared.request.continuation.provider_conversation_id(),
            Some("old-context")
        );
        assert_eq!(
            prepared.request.continuation.replay_transcript(),
            Some("Ordinary chat history")
        );
    }

    // Act: use the ordinary successful-turn persistence boundary.
    context
        .db
        .sessions()
        .persist_session_turn_metadata(
            "worker",
            &SessionTurnMetadata {
                harness_turn_report: None,
                instruction_conversation_id: Some("current-bootstrap".into()),
                model: model_fixture::CODEX_MODEL_ID.into(),
                provider_conversation_id: Some("old-context".into()),
                questions_json: "[]".into(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats::default(),
            },
        )
        .await
        .expect("successful turn metadata");
    let prepared = prepare_turn_request(
        &context,
        &metadata,
        AgentRequestKind::SessionResume,
        None,
        &prompt,
        &resolved_turn_test_env(),
    )
    .await
    .expect("following turn");

    // Assert
    assert_eq!(
        prepared.request.agent_profile,
        AgentDefinitionPrompt::default()
    );
}

#[tokio::test]
async fn prepared_turn_preserves_default_settings() {
    // Arrange
    let (context, _) = persisted_turn_test_context().await;
    let metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: context.session_agent,
    };
    let prompt = TurnPrompt::from_text("Explain this code".to_string());
    let env = resolved_turn_test_env();

    // Act
    let prepared = prepare_turn_request(
        &context,
        &metadata,
        AgentRequestKind::SessionResume,
        None,
        &prompt,
        &env,
    )
    .await
    .expect("default settings");

    // Assert
    assert_eq!(prepared.request.permission_mode, PermissionMode::AutoEdit);
    assert_eq!(prepared.request.reasoning_level, ReasoningLevel::default());
    assert_eq!(prepared.request.response_style, ResponseStyle::default());
    assert_eq!(prepared.request.speed_mode, SpeedMode::default());
    assert_eq!(
        prepared.request.agent_profile,
        AgentDefinitionPrompt::default()
    );
    assert_eq!(prepared.request.prompt, prompt);
    assert_eq!(prepared.request.folder, env.folder);
    assert_eq!(prepared.request.main_checkout_root, env.main_checkout);
}

#[tokio::test]
async fn prepared_turn_resolves_current_settings_but_keeps_the_queued_model() {
    // Arrange
    let (context, project_id) = persisted_turn_test_context().await;
    let metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: context.session_agent,
    };
    let prompt = TurnPrompt::from_text("Explain this code".to_string());

    // Act: change persisted settings after the turn's model was captured.
    update_turn_test_settings(&context.db).await;
    let prepared = prepare_turn_request(
        &context,
        &metadata,
        AgentRequestKind::SessionResume,
        Some("earlier messages".to_string()),
        &prompt,
        &resolved_turn_test_env(),
    )
    .await
    .expect("current settings");

    // Assert
    assert_eq!(prepared.session_project_id, Some(project_id));
    assert_eq!(
        prepared.request.model,
        metadata.session_agent.model().provider_model_str()
    );
    assert_eq!(prepared.request.permission_mode, PermissionMode::ReadOnly);
    assert_eq!(prepared.request.reasoning_level, ReasoningLevel::Low);
    assert_eq!(prepared.request.response_style, ResponseStyle::Concise);
    assert_eq!(prepared.request.speed_mode, SpeedMode::Fast);
    assert_eq!(
        prepared.request.agent_profile,
        AgentDefinitionPrompt::default()
    );
    assert!(
        prepared
            .request
            .prompt
            .text
            .starts_with(READ_ONLY_CHAT_PROMPT)
    );
    assert_eq!(
        prepared.request.continuation.replay_transcript(),
        Some("earlier messages")
    );
    assert_eq!(
        prepared.request.continuation.provider_conversation_id(),
        Some("conversation")
    );
    assert_eq!(
        prepared
            .request
            .continuation
            .persisted_instruction_conversation_id(),
        Some("instructions")
    );
}

#[test]
fn read_only_chat_prompt_redirects_write_access_requests_to_mode_shortcut() {
    // Arrange, Act
    let prompt = READ_ONLY_CHAT_PROMPT;

    // Assert
    assert!(prompt.contains("Do not ask a clarification question requesting write access"));
    assert!(prompt.contains("switching the session to `Auto Edit` with `Shift+Tab`"));
}

#[tokio::test]
async fn persisted_permission_mode_controls_read_only_execution() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    repositories
        .sessions()
        .insert_session(
            "worker",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert worker session");
    repositories
        .sessions()
        .insert_session(
            "read-only-worker",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert read-only worker session");
    repositories
        .sessions()
        .update_session_permission_mode("read-only-worker", PermissionMode::ReadOnly)
        .await
        .expect("failed to set read-only permission mode");
    repositories
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: "researcher",
            is_draft: false,
            model: model_fixture::CODEX_MODEL_ID,
            parent_session_id: None,
            permission_mode: PermissionMode::ReadOnly,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_contracts::ResponseStyle::default(),
            speed_mode: crate::domain::agent::SpeedMode::Normal,
            status: "InProgress",
        })
        .await
        .expect("failed to insert research session");

    // Act
    let worker_mode = load_session_permission_mode(&repositories, "worker")
        .await
        .expect("worker mode should load");
    let read_only_worker_mode = load_session_permission_mode(&repositories, "read-only-worker")
        .await
        .expect("read-only worker mode should load");
    let research_mode = load_session_permission_mode(&repositories, "researcher")
        .await
        .expect("research mode should load");
    let missing_error = load_session_permission_mode(&repositories, "missing")
        .await
        .expect_err("missing session should fail");

    // Assert
    assert_eq!(worker_mode, PermissionMode::AutoEdit);
    assert_eq!(read_only_worker_mode, PermissionMode::ReadOnly);
    assert_eq!(research_mode, PermissionMode::ReadOnly);
    assert!(matches!(missing_error, SessionError::NotFound));
}

#[tokio::test]
async fn permission_mode_load_propagates_query_errors() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let context = turn_test_context(repositories);
    let metadata = TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: context.session_agent,
    };

    // Act
    let result = prepare_turn_request(
        &context,
        &metadata,
        AgentRequestKind::SessionStart,
        None,
        &TurnPrompt::from_text("hello".to_string()),
        &resolved_turn_test_env(),
    )
    .await;

    // Assert
    assert!(matches!(
        result,
        Err(SessionError::Db(DbError::Query(sqlx::Error::PoolClosed)))
    ));
}

/// Supplies resolved paths independently of the worker's cached folder.
fn resolved_turn_test_env() -> ResolvedEnv {
    ResolvedEnv {
        branch: "wt/worker".to_string(),
        folder: PathBuf::from("resolved/worktree"),
        id: EnvId::SessionWorktree("worker".into()),
        main_checkout: Some(PathBuf::from("resolved/main")),
    }
}

#[tokio::test]
async fn response_style_load_returns_persisted_value_and_defaults_missing_sessions() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    repositories
        .sessions()
        .insert_session(
            "worker",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert worker session");
    repositories
        .sessions()
        .update_session_response_style("worker", ResponseStyle::Concise)
        .await
        .expect("failed to persist response style");

    // Act
    let persisted_style = load_session_response_style(&repositories, "worker").await;
    let missing_style = load_session_response_style(&repositories, "missing").await;

    // Assert
    assert_eq!(persisted_style, ResponseStyle::Concise);
    assert_eq!(missing_style, ResponseStyle::Balanced);
}

#[tokio::test]
async fn permission_mode_load_rejects_invalid_persisted_values() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let project_id = repositories
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    repositories
        .sessions()
        .insert_session(
            "worker",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert worker session");
    sqlx::query("UPDATE session SET permission_mode = 'invalid' WHERE id = 'worker'")
        .execute(&pool)
        .await
        .expect("failed to corrupt permission mode");

    // Act
    let permission_result = load_session_permission_mode(&repositories, "worker").await;
    // Assert
    assert!(matches!(
        permission_result,
        Err(SessionError::Db(DbError::InvalidData {
            entity: "session permission mode",
            ..
        }))
    ));
}

#[tokio::test]
async fn title_reasoning_level_defaults_without_project() {
    // Arrange
    let repositories = AppRepositories::in_memory().await.expect("db should open");

    // Act
    let reasoning_level = load_title_reasoning_level(&repositories, None).await;

    // Assert
    assert_eq!(reasoning_level, ReasoningLevel::High);
}

/// Request preparation needs persisted state but no filesystem or worker
/// execution.
async fn persisted_turn_test_context() -> (SessionWorkerContext, i64) {
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/project", None)
        .await
        .expect("project");
    db.sessions()
        .insert_session(
            "worker",
            model_fixture::CODEX_MODEL_ID,
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("session");

    (turn_test_context(db), project_id)
}

/// Changes the persisted selection after capturing the queued turn metadata.
async fn update_turn_test_settings(db: &AppRepositories) {
    db.sessions()
        .update_session_model("worker", "changed-model")
        .await
        .expect("model");
    db.sessions()
        .update_session_permission_mode("worker", PermissionMode::ReadOnly)
        .await
        .expect("permissions");
    db.sessions()
        .update_session_reasoning_level("worker", ReasoningLevel::Low)
        .await
        .expect("reasoning");
    db.sessions()
        .update_session_response_style("worker", ResponseStyle::Concise)
        .await
        .expect("style");
    db.sessions()
        .update_session_speed_mode("worker", SpeedMode::Fast)
        .await
        .expect("speed");
    db.sessions()
        .update_session_provider_conversation_id("worker", Some("conversation".to_string()))
        .await
        .expect("conversation");
    db.sessions()
        .update_session_instruction_conversation_id("worker", Some("instructions".to_string()))
        .await
        .expect("instructions");
}

/// Builds the host boundary without starting a worker or touching the
/// filesystem.
fn turn_test_context(db: AppRepositories) -> SessionWorkerContext {
    SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        harness_models: HarnessModels::default(),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: PathBuf::from("/project/worktree"),
        fs_client: Arc::new(crate::infra::fs::MockFsClient::new()),
        git_client: Arc::new(ag_git::MockGitClient::new()),
        queued_messages: Arc::default(),
        review_request_client: Arc::new(ag_forge::MockReviewRequestClient::new()),
        session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        session_id: "worker".into(),
        session_run: SessionRunClient::from_channel(
            "worker".to_string(),
            Arc::new(MockAgentChannel::new()),
        ),
        session_update_versions: Arc::default(),
        status: Arc::new(Mutex::new(Status::InProgress)),
        transcript: Arc::new(Mutex::new(SessionTranscript::default())),
    }
}
