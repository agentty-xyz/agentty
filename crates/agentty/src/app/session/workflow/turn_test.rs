//! Channel turn preparation and execution tests covering session settings and
//! failure handling.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use ag_contracts::{
    ActivityEvent, ActivityKind, ActivityStatus, AgentDefinitionPrompt, AgentError,
    AgentRequestKind, MockAgentChannel, TurnEvent, TurnNotice, TurnResult, TurnUsage,
    UsageCheckpoint,
};
use ag_git::{DiffStats, GitError, MockGitClient};
use ag_protocol::AgentResponse;
use ag_session::{EnvId, ResolvedEnv, test_support as model_fixture};
use ag_worker::SessionRunClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::worker::{SessionWorkerContext, TurnMetadata};
use super::{
    LiveUsage, MainCheckoutSnapshot, add_main_checkout_warning, consume_turn_events,
    fresh_turn_cancel_token, live_transcript_source, load_session_response_style,
    load_title_reasoning_level, prepare_turn_request, relay_turn_events,
};
use crate::app::AppEvent;
use crate::app::cargo_cache::CargoCacheWarmer;
use crate::app::graph_activity::GraphActivityRegistry;
use crate::app::service::HarnessModels;
use crate::domain::agent::{
    AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode,
};
use crate::domain::permission::PermissionMode;
use crate::domain::session::Status;
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::session_usage::LiveUsageRun;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{AppRepositories, LEGACY_SESSION_AGENT_CLEAR_MARKER, SessionTurnMetadata};
use crate::test_support::poison_lock;

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
        generated_turn: None,
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
        .await;

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
        assert_eq!(prepared.request.continuation.usage_checkpoint(), None);
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
                usage: TurnUsage {
                    checkpoint: Some(UsageCheckpoint::new("/worktree", "cumulative-totals")),
                    ..TurnUsage::default()
                },
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
    .await;

    // Assert: the next turn continues from the stored usage checkpoint.
    assert_eq!(
        prepared.request.agent_profile,
        AgentDefinitionPrompt::default()
    );
    assert_eq!(
        prepared.request.continuation.usage_checkpoint(),
        Some("cumulative-totals")
    );
}

#[tokio::test]
async fn prepared_turn_preserves_default_settings() {
    // Arrange
    let (context, _) = persisted_turn_test_context().await;
    let metadata = TurnMetadata {
        published_upstream_ref: None,
        generated_turn: None,
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
    .await;

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
        generated_turn: None,
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
    .await;

    // Assert
    assert_eq!(prepared.session_project_id, Some(project_id));
    assert_eq!(
        prepared.request.model,
        metadata.session_agent.model().provider_model_str()
    );
    assert_eq!(prepared.request.permission_mode, PermissionMode::AutoEdit);
    assert_eq!(prepared.request.reasoning_level, ReasoningLevel::Low);
    assert_eq!(prepared.request.response_style, ResponseStyle::Concise);
    assert_eq!(prepared.request.speed_mode, SpeedMode::Fast);
    assert_eq!(
        prepared.request.agent_profile,
        AgentDefinitionPrompt::default()
    );
    assert_eq!(prepared.request.prompt, prompt);
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

#[tokio::test]
async fn prepared_turn_falls_back_to_default_settings_when_storage_fails() {
    // Arrange
    let (repositories, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let context = turn_test_context(repositories);
    let metadata = TurnMetadata {
        published_upstream_ref: None,
        generated_turn: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: context.session_agent,
    };

    // Act
    let prepared = prepare_turn_request(
        &context,
        &metadata,
        AgentRequestKind::SessionStart,
        None,
        &TurnPrompt::from_text("hello".to_string()),
        &resolved_turn_test_env(),
    )
    .await;

    // Assert
    assert_eq!(prepared.session_project_id, None);
    assert_eq!(prepared.request.permission_mode, PermissionMode::AutoEdit);
    assert_eq!(prepared.request.reasoning_level, ReasoningLevel::default());
    assert_eq!(prepared.request.response_style, ResponseStyle::default());
    assert_eq!(prepared.request.speed_mode, SpeedMode::default());
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
        .update_session_response_style("worker", ResponseStyle::Detailed)
        .await
        .expect("failed to persist response style");

    // Act
    let persisted_style = load_session_response_style(&repositories, "worker").await;
    let missing_style = load_session_response_style(&repositories, "missing").await;

    // Assert
    assert_eq!(persisted_style, ResponseStyle::Detailed);
    assert_eq!(missing_style, ResponseStyle::Concise);
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

#[test]
fn live_transcript_source_replays_the_shared_transcript() {
    // Arrange
    let transcript = Arc::new(Mutex::new(SessionTranscript::new(vec![
        SessionMessage::conversation(0, SessionMessageKind::UserPrompt, "question"),
        SessionMessage::conversation(1, SessionMessageKind::AssistantAnswer, "answer"),
    ])));
    let expected = transcript.lock().expect("transcript").replay_text();

    // Act
    let replay = live_transcript_source(&transcript).replay_text();

    // Assert
    assert!(expected.is_some());
    assert_eq!(replay, expected);
}

#[tokio::test]
async fn main_checkout_status_failure_after_the_turn_fails_the_turn() {
    // Arrange
    let mut context = turn_test_context(AppRepositories::in_memory().await.expect("db"));
    let mut git_client = MockGitClient::new();
    git_client
        .expect_tracked_worktree_status()
        .once()
        .returning(|_| Box::pin(async { Err(GitError::OutputParse("status unavailable".into())) }));
    context.git_client = Arc::new(git_client);
    let snapshot = MainCheckoutSnapshot {
        main_repo_root: PathBuf::from("/project"),
        tracked_status_output: String::new(),
    };
    let turn_result = TurnResult {
        assistant_message: AgentResponse::plain("done"),
        context_reset: false,
        harness_report: None,
        provider_conversation_id: None,
        usage: TurnUsage::default(),
    };

    // Act
    let result = add_main_checkout_warning(&context, Some(&snapshot), Ok(turn_result)).await;

    // Assert
    let error = result.expect_err("post-turn status failure");
    assert!(
        matches!(&error, AgentError::Backend(message) if message.contains("status unavailable")),
        "{error}"
    );
}

#[tokio::test]
async fn sandbox_notices_reach_the_transcript_as_they_arrive_and_other_events_pass_on() {
    // Arrange
    let (context, _) = persisted_turn_test_context().await;
    let unavailable = || {
        TurnEvent::Notice(TurnNotice::SandboxUnavailable {
            message: "Commands run without the sandbox. Bubblewrap (`bwrap`) is not installed."
                .to_string(),
            remedy: "Install bubblewrap.".to_string(),
        })
    };
    let compacted = TurnEvent::Notice(TurnNotice::ContextCompacted { model: None });
    let turns = [
        vec![
            TurnEvent::Notice(TurnNotice::LikelySandboxDenial),
            TurnEvent::ThoughtDelta("Running a command".to_string()),
            unavailable(),
            unavailable(),
            compacted.clone(),
        ],
        vec![unavailable()],
    ];

    // Act
    let mut forwarded = Vec::new();
    // Edit-capable and read-only turns relay sandbox notices the same way.
    for (events, permission_mode) in turns
        .into_iter()
        .zip([PermissionMode::AutoEdit, PermissionMode::ReadOnly])
    {
        let (event_tx, relayed_rx) = mpsc::unbounded_channel();
        let (relay_tx, mut event_rx) = mpsc::unbounded_channel();
        for event in events {
            event_tx.send(event).expect("queued event");
        }
        drop(event_tx);
        relay_turn_events(&context, permission_mode, relayed_rx, relay_tx).await;
        while let Ok(event) = event_rx.try_recv() {
            forwarded.push(event);
        }
    }
    // A consumer that receives sandbox notices directly keeps none of them.
    let (event_tx, event_rx) = mpsc::unbounded_channel();
    event_tx
        .send(TurnEvent::Notice(TurnNotice::LikelySandboxDenial))
        .expect("queued event");
    drop(event_tx);
    let activity = consume_turn_events(
        event_rx,
        context.app_event_tx.clone(),
        context.session_id.clone(),
        Arc::clone(&context.child_pid),
        LiveUsage::new(
            context.app_event_tx.clone(),
            context.session_id.clone(),
            LiveUsageRun::SessionTurn,
        ),
        Arc::clone(&context.clock),
    )
    .await;

    // Assert
    assert_eq!(
        forwarded,
        [
            TurnEvent::ThoughtDelta("Running a command".to_string()),
            compacted
        ]
    );
    assert_eq!(activity.notices(), [] as [String; 0]);
    let notices = context
        .transcript
        .lock()
        .expect("transcript")
        .messages()
        .iter()
        .map(|message| message.content.trim().to_string())
        .collect::<Vec<_>>();
    let warning = "[Sandbox Warning] Commands run without the sandbox. Bubblewrap (`bwrap`) is \
                   not installed. Install bubblewrap.";
    assert_eq!(
        notices.iter().filter(|notice| *notice == warning).count(),
        1
    );
    assert_eq!(
        notices
            .iter()
            .filter(|notice| notice.starts_with("[Sandbox] A command may have been blocked"))
            .count(),
        1
    );
}

#[tokio::test]
async fn fresh_turn_cancel_token_rejects_a_poisoned_token_slot() {
    // Arrange
    let context = turn_test_context(AppRepositories::in_memory().await.expect("db"));
    poison_lock(Arc::clone(&context.cancel_token));

    // Act
    let result = fresh_turn_cancel_token(&context);

    // Assert
    assert!(
        result
            .expect_err("poisoned token slot")
            .to_string()
            .contains("cancel token lock poisoned")
    );
}

#[tokio::test]
async fn only_edit_capable_turns_refresh_diff_stats_after_a_finished_edit() {
    for (permission_mode, expected_added_lines) in
        [(PermissionMode::ReadOnly, 0), (PermissionMode::AutoEdit, 9)]
    {
        // Arrange
        let (mut context, project_id) = persisted_turn_test_context().await;
        let mut fs_client = crate::infra::fs::MockFsClient::new();
        fs_client.expect_is_dir().return_const(true);
        let mut git_client = MockGitClient::new();
        git_client.expect_diff_stats().returning(|_, _| {
            Box::pin(async {
                Ok(DiffStats {
                    added_lines: 9,
                    deleted_lines: 0,
                    has_changes: true,
                })
            })
        });
        context.fs_client = Arc::new(fs_client);
        context.git_client = Arc::new(git_client);
        let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
        context.app_event_tx = app_event_tx;
        let (event_tx, relayed_rx) = mpsc::unbounded_channel();
        let (relay_tx, mut event_rx) = mpsc::unbounded_channel();
        event_tx
            .send(TurnEvent::Activity(ActivityEvent {
                attempt_id: "attempt".into(),
                exit_code: None,
                facts: Box::default(),
                id: "edit".into(),
                input: None,
                kind: ActivityKind::Tool,
                name: "Edit".into(),
                observed_at: SystemTime::UNIX_EPOCH,
                output: None,
                parent_id: None,
                status: ActivityStatus::Completed,
            }))
            .expect("queued event");
        drop(event_tx);

        // Act
        let refreshed = relay_turn_events(&context, permission_mode, relayed_rx, relay_tx).await;
        let sessions = context
            .db
            .sessions()
            .load_sessions_for_project(project_id)
            .await
            .expect("sessions");

        // Assert
        assert!(matches!(event_rx.try_recv(), Ok(TurnEvent::Activity(_))));
        assert_eq!(sessions[0].added_lines, expected_added_lines);
        let projected = std::iter::from_fn(|| app_event_rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::SessionDiffStatsUpdated { .. }));
        assert_eq!(projected, expected_added_lines > 0);
        assert_eq!(refreshed, expected_added_lines > 0);
    }
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
        graph_activity: GraphActivityRegistry::default(),
        harness_models: HarnessModels::default(),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        cargo_cache: CargoCacheWarmer::default(),
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
