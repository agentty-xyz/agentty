use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_agent as agent;
use ag_agent::{
    AgentError, AgentRequestKind, MockAgentChannel, MockOneShotClient, OneShotClient,
    PermissionMode, TurnResult,
};
use ag_forge as forge;
use ag_git::{MockGitClient, RebaseStepResult};
use ag_protocol::{
    AgentResponse, ReviewCommentOutcome, ReviewCommentResolution, TurnPromptAttachment,
};
use mockall::Sequence;
use tempfile::tempdir;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::super::post_turn::{
    PostTurnContext, TurnPersonalityPersistence, apply_turn_result,
};
use super::super::super::published_branch;
use super::super::{
    ScheduledSessionCommand, SessionCommand, SessionWorkerContext, SessionWorkerHandle,
    TurnMetadata,
};
use crate::app::AppEvent;
use crate::app::branch_publish::BranchPublishTaskSession;
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::session::{
    PublishedBranchSyncStatus, QueuedMessage, ReviewRequest, ReviewRequestState, SessionStats,
    Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{AppRepositories, PersistedSessionCreation, SessionTurnMetadata};
use crate::infra::fs;
use crate::infra::personality::RealPersonalityCatalogClient;

/// Builds one filesystem mock that treats every probed path as an
/// existing directory.
pub(super) fn mock_fs_client_with_existing_directories() -> fs::MockFsClient {
    let mut fs_client = fs::MockFsClient::new();
    fs_client.expect_is_dir().times(0..).returning(|_| true);
    fs_client
        .expect_canonicalize()
        .times(0..)
        .returning(|path| Box::pin(async move { Ok(path) }));

    fs_client
}

/// Builds one git client mock that detects the `wt/sess1` worktree and
/// resolves the given main working checkout.
pub(super) fn mock_git_client_detecting_main_repo(main_repo_root: PathBuf) -> MockGitClient {
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(move |_| {
            let main_repo_root = main_repo_root.clone();

            Box::pin(async move { Ok(Some(main_repo_root)) })
        });

    mock_git_client
}

/// Inserts one in-progress Antigravity-backed session for worker-flow
/// tests.
pub(super) async fn insert_in_progress_test_session(db: &AppRepositories) -> i64 {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");

    project_id
}

/// Inserts one in-progress read-only researcher for worker-flow tests.
pub(super) async fn insert_in_progress_research_session(db: &AppRepositories) {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "antigravity",
            base_branch: "main",
            id: "sess1",
            is_draft: false,
            model: "gemini-3.8-flash",
            orchestration_task_id: None,
            parent_session_id: None,
            permission_mode: ag_agent::PermissionMode::AutoEdit,
            personality_id: None,
            project_id,
            reasoning_level: ReasoningLevel::default(),
            response_style: ag_agent::ResponseStyle::default(),
            role: Some("OrchestrationResearcher"),
            speed_mode: SpeedMode::Normal,
            status: "InProgress",
        })
        .await
        .expect("failed to insert research session");
}

/// Seeds one unfinished operation and its owning session for recovery
/// tests.
pub(super) async fn seed_recovery_test_operation(
    db: &AppRepositories,
    status: Status,
    operation_kind: &str,
) {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            &status.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.operations()
        .insert_session_operation("op-1", "sess1", operation_kind)
        .await
        .expect("failed to insert session operation");
}

pub(super) fn empty_transcript() -> Arc<Mutex<SessionTranscript>> {
    Arc::new(Mutex::new(SessionTranscript::default()))
}

/// Builds one user prompt referencing a single managed image attachment.
pub(super) fn turn_prompt_with_attachment(attachment_path: PathBuf) -> TurnPrompt {
    TurnPrompt {
        attachments: vec![TurnPromptAttachment {
            local_image_path: attachment_path,
            placeholder: "[Image #1]".to_string(),
        }],
        text: "Continue [Image #1]".to_string(),
        text_source: ag_protocol::TurnPromptTextSource::UserPrompt,
    }
}

pub(super) fn resume_command(operation_id: &str) -> SessionCommand {
    SessionCommand::Run {
        operation_id: operation_id.to_string(),
        request_kind: AgentRequestKind::SessionResume,
        replay_transcript: None,
        prompt: "Continue".into(),
        turn_metadata: TurnMetadata {
            published_upstream_ref: None,
            review_comment_thread_ids: Vec::new(),
            session_agent: AgentSelection::new(
                crate::domain::agent::AgentKind::Claude,
                AgentModel::ClaudeSonnet5,
            ),
        },
    }
}

pub(super) fn cancel_token_after_short_delay(cancel_token: Arc<Mutex<CancellationToken>>) {
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        cancel_token.lock().expect("cancel token lock").cancel();
    });
}

pub(super) fn expect_clean_main_checkout_snapshot(
    mock_git_client: &mut MockGitClient,
    main_repo_root: PathBuf,
) {
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .once()
        .returning(move |_| {
            let main_repo_root = main_repo_root.clone();

            Box::pin(async move { Ok(Some(main_repo_root)) })
        });
    mock_git_client
        .expect_tracked_worktree_status()
        .once()
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
}

pub(super) fn transcript_text(transcript: &Arc<Mutex<SessionTranscript>>) -> String {
    transcript
        .lock()
        .ok()
        .and_then(|transcript| transcript.replay_text())
        .unwrap_or_default()
}

/// Builds a title-generation boundary that verifies temporary research
/// sessions use an isolated read-only utility request.
pub(super) fn research_title_one_shot_client() -> Arc<dyn OneShotClient> {
    let mut title_client = MockOneShotClient::new();
    title_client
        .expect_submit()
        .once()
        .withf(|request| {
            request.permission_mode == PermissionMode::ReadOnly
                && request.request_kind == AgentRequestKind::UtilityPrompt
        })
        .returning(|_| {
            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain("Inspect architecture boundaries"),
                stats: agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    Arc::new(title_client)
}

/// Builds a deterministic post-turn one-shot boundary. Tests whose
/// worktrees are clean never submit; auto-commit tests receive the
/// canonical message they already expect.
pub(super) fn auto_commit_one_shot_client() -> Arc<dyn OneShotClient> {
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().times(0..).returning(|request| {
            let answer = if request
                .prompt
                .contains("Reconcile the current review-request title")
            {
                r#"{"title":"Old title","description":"Old body\n\n- Update the linked review request body.","is_title_change_significant":false}"#
            } else {
                "Refine review metadata sync\n\n- Update the linked review request body."
            };

            Ok(agent::OneShotSubmission {
                response: AgentResponse::plain(answer),
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

/// Applies one turn result through the narrowed post-turn dependency set
/// cloned from a full worker context.
pub(super) async fn apply_worker_turn_result(
    context: &SessionWorkerContext,
    turn_metadata: TurnMetadata,
    turn_result: Result<TurnResult, AgentError>,
) -> Result<Status, SessionError> {
    let post_turn_context = PostTurnContext::from_worker(context, auto_commit_one_shot_client());

    apply_turn_result(
        &post_turn_context,
        turn_metadata,
        TurnPersonalityPersistence::default(),
        turn_result,
    )
    .await
}

/// Binds a worker to the same persistence and live handles as cancellation.
pub(super) fn preparation_test_worker_context(
    app: &crate::app::App,
    session_id: &str,
) -> SessionWorkerContext {
    let runtime = app
        .sessions
        .session_worker_runtime_or_err(&app.services, session_id)
        .expect("runtime");

    SessionWorkerContext {
        app_event_tx: app.services.event_sender(),
        branch_operation_lock: runtime.branch_operation_lock,
        cancel_token: runtime.cancel_token,
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: runtime.child_pid,
        clock: app.services.clock(),
        db: app.services.db().clone(),
        folder: runtime.folder,
        fs_client: app.services.fs_client(),
        git_client: app.services.git_client(),
        personality_catalog_client: runtime.personality_catalog_client,
        queued_messages: runtime.queued_messages,
        review_request_client: runtime.review_request_client,
        session_update_versions: runtime.session_update_versions,
        session_id: runtime.session_id,
        session_agent: runtime.session_agent,
        status: runtime.status,
        transcript: runtime.transcript,
    }
}

/// Exercises publication transaction failures and a retry after the
/// stacked parent becomes active, without allowing provider work.
pub(super) async fn assert_preparation_publication_failure_is_retryable(trigger: &'static str) {
    // Arrange
    let (mut app, _directory, pool) = crate::test_support::new_git_test_app_with_pool().await;
    let parent_id = app.create_session().await.expect("parent");
    crate::test_support::set_session_status_for_test(&mut app, &parent_id, Status::Review);
    let session_id = app
        .create_stacked_draft_session(&parent_id)
        .await
        .expect("child");
    let (_images, prompt) = managed_first_prompt();
    app.stage_draft_message(&session_id, prompt.text.as_str())
        .await
        .expect("stage");
    app.services
        .db()
        .sessions()
        .insert_session_preparation(&session_id, "main")
        .await
        .expect("prepare");
    app.services
        .db()
        .sessions()
        .save_preparation_prompt(&session_id, &serde_json::to_string(&prompt).expect("JSON"))
        .await
        .expect("save");
    app.sessions
        .worker_service_mut()
        .test_agent_channels
        .insert(session_id.clone().into(), Arc::new(MockAgentChannel::new()));
    sqlx::query(trigger).execute(&pool).await.expect("trigger");

    // Act
    app.retry_workspace_preparation(&session_id)
        .await
        .expect("submit");
    crate::test_support::finish_session_creation_tasks(&mut app).await;
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let row = app
                .services
                .db()
                .sessions()
                .load_session_preparation(&session_id)
                .await
                .expect("load")
                .expect("preparation");
            if row.state == crate::infra::db::SessionPreparationState::Failed {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("worker rejects start");

    // Assert
    let session = app.sessions.session_for_id(&session_id).expect("session");
    assert_eq!(session.status, Status::Draft);
    assert!(session.is_draft_session());
    assert!(
        app.services
            .db()
            .sessions()
            .load_session(&session_id)
            .await
            .expect("load")
            .expect("row")
            .is_draft
    );
    assert!(prompt.attachments[0].local_image_path.exists());
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_session_messages(&session_id)
            .await
            .expect("messages")
            .len(),
        0
    );

    assert_preparation_retry_rejects_active_parent(&mut app, &session_id, &parent_id).await;
}

/// Retries saved work after its parent resumes, checking the gate before
/// the worker can attempt the failed transaction again.
pub(super) async fn assert_preparation_retry_rejects_active_parent(
    app: &mut crate::app::App,
    session_id: &str,
    parent_id: &str,
) {
    // Act: retry must still enforce the stack gate after a failed marker.
    crate::test_support::set_session_status_for_test(app, parent_id, Status::InProgress);
    app.retry_workspace_preparation(session_id)
        .await
        .expect("retry");
    crate::test_support::finish_session_creation_tasks(app).await;
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(session_id)
        .await
        .expect("load")
        .expect("preparation");

    // Assert
    assert_eq!(
        preparation.state,
        crate::infra::db::SessionPreparationState::Failed
    );
    assert!(
        preparation
            .error
            .expect("parent gate")
            .contains("parent stack")
    );
    assert!(preparation.prompt.is_some());
    assert!(
        app.services
            .db()
            .sessions()
            .preparation_prompt_operation_status(session_id)
            .await
            .expect("operation")
            .is_none()
    );
}

/// Captures a saved child's first turn before the worker accepts it.
pub(super) async fn queue_saved_stacked_prompt(
    app: &mut crate::app::App,
    parent_id: &str,
) -> (String, mpsc::UnboundedReceiver<ScheduledSessionCommand>) {
    let child_id = app
        .create_stacked_draft_session(parent_id)
        .await
        .expect("child");
    app.stage_draft_message(&child_id, "saved child prompt")
        .await
        .expect("stage");
    app.services
        .db()
        .sessions()
        .insert_session_preparation(&child_id, "main")
        .await
        .expect("prepare");
    app.services
        .db()
        .sessions()
        .save_preparation_prompt(
            &child_id,
            &serde_json::to_string(&TurnPrompt::from_text("saved child prompt".to_string()))
                .expect("JSON"),
        )
        .await
        .expect("save");
    let (sender, receiver) = mpsc::unbounded_channel();
    app.sessions.worker_service_mut().workers.insert(
        child_id.clone().into(),
        SessionWorkerHandle {
            queued_work_sequence: Arc::default(),
            sender,
            wakeup: Arc::default(),
        },
    );

    app.retry_workspace_preparation(&child_id)
        .await
        .expect("submit");
    crate::test_support::finish_session_creation_tasks(app).await;

    (child_id, receiver)
}

/// Creates a retained prompt image under the production cleanup boundary.
pub(super) fn managed_first_prompt() -> (tempfile::TempDir, TurnPrompt) {
    let managed_tmp = crate::app::agentty_home().join("tmp");
    std::fs::create_dir_all(&managed_tmp).expect("attachment root");
    let images = tempfile::tempdir_in(managed_tmp).expect("attachment directory");
    let image_directory = images.path().join("images");
    std::fs::create_dir(&image_directory).expect("managed image directory");
    let image_path = image_directory.join("image.png");
    std::fs::write(&image_path, b"retained image").expect("attachment");
    let mut prompt = TurnPrompt::from_text("saved first prompt [Image #1]".to_string());
    prompt.attachments.push(TurnPromptAttachment {
        local_image_path: image_path,
        placeholder: "[Image #1]".to_string(),
    });

    (images, prompt)
}

/// Creates a real fork with copied history on a transport that requires
/// explicit one-time replay, then saves its first reply for preparation.
pub(super) async fn prepare_fork_with_saved_reply(app: &mut crate::app::App) -> String {
    let source_id = app.create_session().await.expect("source");
    app.set_session_model(
        &source_id,
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5),
    )
    .await
    .expect("non-app-server model");
    assert!(!agent::transport_mode(AgentKind::Claude).uses_app_server());
    for (kind, content) in [
        (SessionMessageKind::UserPrompt, "Original source question"),
        (SessionMessageKind::AssistantAnswer, "Copied source answer"),
    ] {
        app.services
            .db()
            .sessions()
            .append_session_message(&source_id, kind, content)
            .await
            .expect("source history");
    }
    crate::test_support::set_session_status_for_test(app, &source_id, Status::Review);
    let session_id = app.fork_session(&source_id).await.expect("fork");
    crate::test_support::finish_session_creation_tasks(app).await;
    let prompt = TurnPrompt::from_text("Continue the copied conversation".to_string());
    app.services
        .db()
        .sessions()
        .save_preparation_prompt(&session_id, &serde_json::to_string(&prompt).expect("JSON"))
        .await
        .expect("save reply");

    session_id
}

/// Captures the actual command emitted by the saved-prompt retry path.
pub(super) async fn capture_prepared_fork_reply(
    app: &mut crate::app::App,
    session_id: &str,
) -> (
    SessionCommand,
    mpsc::UnboundedReceiver<ScheduledSessionCommand>,
) {
    let (sender, mut receiver) = mpsc::unbounded_channel();
    app.sessions.worker_service_mut().workers.insert(
        session_id.into(),
        SessionWorkerHandle {
            queued_work_sequence: Arc::default(),
            sender,
            wakeup: Arc::default(),
        },
    );
    app.retry_workspace_preparation(session_id)
        .await
        .expect("retry");
    crate::test_support::finish_session_creation_tasks(app).await;
    let command = receiver.try_recv().expect("saved fork reply").command;
    assert!(receiver.try_recv().is_err(), "only one turn is queued");

    (command, receiver)
}

/// Checks both sides of the frozen source conversation reach the worker.
pub(super) fn assert_fork_history_replayed(command: &SessionCommand) {
    assert!(matches!(command, SessionCommand::Run {
            request_kind: AgentRequestKind::SessionResume,
            replay_transcript: Some(history), ..
        } if history.contains("Original source question") && history.contains("Copied source answer") && !history.contains("Continue the copied conversation")));
}

/// Injects either operation insertion failure or a closed worker queue.
pub(super) async fn inject_handoff_failure(
    app: &mut crate::app::App,
    pool: &sqlx::SqlitePool,
    session_id: &str,
    reject_delivery: bool,
) -> Option<mpsc::UnboundedReceiver<ScheduledSessionCommand>> {
    let (sender, receiver) = mpsc::unbounded_channel();
    let rejected_receiver = if reject_delivery {
        drop(receiver);
        None
    } else {
        sqlx::query(
            "CREATE TRIGGER reject_start BEFORE INSERT ON session_operation BEGIN SELECT \
             RAISE(ABORT, 'handoff rejected'); END",
        )
        .execute(pool)
        .await
        .expect("reject operation insertion");
        Some(receiver)
    };
    app.sessions.worker_service_mut().workers.insert(
        session_id.into(),
        SessionWorkerHandle {
            queued_work_sequence: Arc::default(),
            sender,
            wakeup: Arc::default(),
        },
    );

    rejected_receiver
}

/// Checks durable and rendered state after a rejected first-turn handoff.
pub(super) async fn assert_first_prompt_remains_retryable(
    app: &crate::app::App,
    pool: &sqlx::SqlitePool,
    session_id: &str,
    prompt: &TurnPrompt,
    initial_status: Status,
) {
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(session_id)
        .await
        .expect("load preparation")
        .expect("preparation");
    assert_eq!(
        preparation.state,
        crate::infra::db::SessionPreparationState::Failed
    );
    assert_eq!(
        preparation.prompt.as_deref(),
        Some(serde_json::to_string(prompt).expect("prompt JSON").as_str())
    );
    assert_eq!(
        app.sessions
            .session_for_id(session_id)
            .expect("session")
            .status,
        initial_status
    );
    let status: String = sqlx::query_scalar("SELECT status FROM session WHERE id = ?")
        .bind(session_id)
        .fetch_one(pool)
        .await
        .expect("persisted status");
    assert_eq!(status, initial_status.to_string());
    assert!(prompt.attachments[0].local_image_path.is_file());
    assert!(
        app.services
            .db()
            .operations()
            .load_unfinished_session_operations()
            .await
            .expect("operations")
            .is_empty()
    );
    assert!(
        app.services
            .db()
            .sessions()
            .load_session_messages(session_id)
            .await
            .expect("messages")
            .iter()
            .all(|message| message.kind != "user_prompt")
    );
}

/// Checks that retry acknowledges the saved prompt once, preserving the
/// worker's attachment ownership and a single transcript entry.
pub(super) async fn assert_first_prompt_was_accepted(
    app: &crate::app::App,
    session_id: &str,
    prompt: &TurnPrompt,
    initial_status: Status,
) {
    assert_eq!(
        app.sessions
            .session_for_id(session_id)
            .expect("session")
            .status,
        initial_status
    );
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(session_id)
        .await
        .expect("load preparation")
        .expect("preparation");
    assert_eq!(
        preparation.state,
        crate::infra::db::SessionPreparationState::Ready
    );
    assert!(preparation.prompt.is_some());
    assert!(
        prompt.attachments[0].local_image_path.is_file(),
        "worker owns the retained image after acceptance"
    );
    assert_eq!(
        app.services
            .db()
            .sessions()
            .preparation_prompt_operation_status(session_id)
            .await
            .expect("operation status")
            .as_deref(),
        Some("queued")
    );
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(session_id)
        .await
        .expect("messages");
    let prompts = messages
        .iter()
        .filter(|message| message.kind == "user_prompt")
        .collect::<Vec<_>>();
    assert!(
        prompts.is_empty(),
        "queued prompts remain recoverable until execution"
    );
}

/// Builds the default turn metadata used by session worker tests that
/// exercise the `Gemini38Flash` path without branch publication.
pub(super) fn default_turn_metadata() -> TurnMetadata {
    TurnMetadata {
        published_upstream_ref: None,
        review_comment_thread_ids: Vec::new(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
    }
}

/// Persists one successful personality-delivery marker for resolver tests.
pub(super) async fn persist_test_personality_state(
    db: &AppRepositories,
    personality: TurnPersonalityPersistence,
) {
    db.sessions()
        .persist_session_turn_metadata(
            "sess1",
            &SessionTurnMetadata {
                applied_personality_id: personality.applied_personality_id,
                applied_personality_prompt_hash: personality.applied_personality_prompt_hash,
                instruction_conversation_id: None,
                model: AgentModel::Gemini38Flash.as_str().to_string(),
                provider_conversation_id: None,
                questions_json: "[]".to_string(),
                review_comment_resolutions: Vec::new(),
                token_usage_delta: SessionStats::default(),
            },
        )
        .await
        .expect("personality application should persist");
}

/// Inserts an in-progress session linked to an open GitHub review request.
pub(super) async fn insert_in_progress_session_with_review_request(db: &AppRepositories) {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.reviews()
        .update_session_review_request("sess1", Some(linked_github_review_request()))
        .await
        .expect("failed to persist review request");
}

/// Returns one linked GitHub review request fixture for metadata sync
/// tests.
pub(super) fn linked_github_review_request() -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 100,
        summary: forge::ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: forge::ForgeKind::GitHub,
            source_branch: "wt/session-id".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Old title".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}

/// Expects the branch-publish safety preflight used by published-branch
/// auto-push.
pub(super) fn expect_safe_auto_push_state(mock_git_client: &mut MockGitClient) {
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
}

/// Expects one successful advisory pre-commit readiness check.
pub(super) fn expect_pre_commit_hook_ready(mock_git_client: &mut MockGitClient) {
    mock_git_client
        .expect_check_pre_commit_hook_ready()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
}

/// Proves a later successful push performs no review-thread effects.
pub(super) async fn assert_later_push_skips_review_operations(context: &SessionWorkerContext) {
    let mut git_client = MockGitClient::new();
    expect_safe_auto_push_state(&mut git_client);
    git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
    git_client.expect_repo_url().never();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

    published_branch::run_published_branch_auto_push(
        published_branch::PublishedBranchAutoPushInput {
            app_event_tx,
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::new(git_client),
            published_upstream_ref: "origin/wt/session-id".to_string(),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            review_request_metadata_sync: None,
            session_id: context.session_id.clone(),
            session_update_versions: context.session_update_versions.clone(),
            sync_operation_id: "later-push".to_string(),
            transcript: Arc::clone(&context.transcript),
        },
    )
    .await;
    let event = app_event_rx
        .recv()
        .await
        .expect("later push should report completion");

    assert!(matches!(
        event,
        AppEvent::PublishedBranchSyncUpdated {
            sync_status: PublishedBranchSyncStatus::Succeeded,
            ..
        }
    ));
}

/// Pushes a descendant commit that has reverted the reported fix.
pub(super) async fn push_descendant_that_reverted_fix(context: &SessionWorkerContext) {
    let mut git_client = MockGitClient::new();
    expect_safe_auto_push_state(&mut git_client);
    git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
    git_client
        .expect_get_ref_ahead_behind()
        .once()
        .withf(|_, left_ref, right_ref| left_ref == "HEAD" && right_ref == "fix-commit")
        .returning(|_, _, _| Box::pin(async { Ok((1, 0)) }));
    git_client.expect_repo_url().never();

    published_branch::run_published_branch_auto_push(
        published_branch::PublishedBranchAutoPushInput {
            app_event_tx: context.app_event_tx.clone(),
            db: context.db.clone(),
            folder: context.folder.clone(),
            git_client: Arc::new(git_client),
            published_upstream_ref: "origin/wt/session-id".to_string(),
            review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
            review_request_metadata_sync: None,
            session_id: context.session_id.clone(),
            session_update_versions: context.session_update_versions.clone(),
            sync_operation_id: "push-after-revert".to_string(),
            transcript: Arc::clone(&context.transcript),
        },
    )
    .await;
}

/// Returns one git client mock through a successful dirty-worktree commit.
pub(super) fn dirty_auto_commit_git_client(commit_message: &str) -> MockGitClient {
    let mut mock_git_client = MockGitClient::new();
    expect_pre_commit_hook_ready(&mut mock_git_client);
    mock_git_client
        .expect_is_worktree_clean()
        .once()
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_diff()
        .once()
        .returning(|_, _| Box::pin(async { Ok("diff --git a/a.rs b/a.rs".to_string()) }));
    mock_git_client
        .expect_has_commits_since()
        .once()
        .returning(|_, _| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_head_commit_message()
        .once()
        .returning({
            let commit_message = commit_message.to_string();

            move |_| {
                let commit_message = commit_message.clone();

                Box::pin(async move { Ok(Some(commit_message)) })
            }
        });
    mock_git_client
        .expect_commit_all_preserving_single_commit()
        .once()
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_head_short_hash()
        .once()
        .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));

    mock_git_client
}

/// Returns one git client mock that produces a successful auto-commit
/// outcome.
pub(super) fn auto_commit_git_client(
    commit_message: &str,
    sequence: &mut Sequence,
) -> MockGitClient {
    let mut mock_git_client = dirty_auto_commit_git_client(commit_message);
    expect_safe_auto_push_state(&mut mock_git_client);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(|folder, remote_branch_name| {
            folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
        })
        .in_sequence(sequence)
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
    mock_git_client
        .expect_repo_url()
        .once()
        .in_sequence(sequence)
        .returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });

    mock_git_client
}

/// Returns one git client that proves the branch push precedes forge
/// review-thread mutations.
pub(super) fn review_resolution_git_client(sequence: &mut Sequence) -> MockGitClient {
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_is_worktree_clean()
        .once()
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_head_hash()
        .once()
        .in_sequence(sequence)
        .returning(|_| Box::pin(async { Ok("commit-1".to_string()) }));
    expect_safe_auto_push_state(&mut mock_git_client);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .in_sequence(sequence)
        .returning(|_, _| Box::pin(async { Ok("origin/wt/session-id".to_string()) }));
    mock_git_client
        .expect_get_ref_ahead_behind()
        .once()
        .in_sequence(sequence)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    mock_git_client
        .expect_repo_url()
        .once()
        .in_sequence(sequence)
        .returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });

    mock_git_client
}

/// Returns one forge client that replies to and resolves the expected
/// review thread after the sequenced push.
pub(super) fn review_resolution_client(
    folder: PathBuf,
    sequence: &mut Sequence,
) -> forge::MockReviewRequestClient {
    let mut review_request_client = forge::MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .in_sequence(sequence)
        .returning(|_| Ok(github_forge_remote()));
    review_request_client
        .expect_fetch_review_comment_snapshot()
        .once()
        .in_sequence(sequence)
        .returning(|_, _| {
            Box::pin(async {
                Ok(forge::ReviewCommentSnapshot {
                    pr_level_comments: Vec::new(),
                    threads: vec![forge::ReviewCommentThread {
                        anchor_side: forge::ReviewCommentAnchorSide::New,
                        comments: Vec::new(),
                        id: "thread-42".to_string(),
                        is_outdated: Some(false),
                        is_resolved: false,
                        line: Some(1),
                        path: "src/lib.rs".to_string(),
                        start_line: None,
                    }],
                })
            })
        });
    review_request_client
        .expect_reply_to_thread()
        .once()
        .in_sequence(sequence)
        .withf(move |remote, display_id, thread_id, body| {
            remote.command_working_directory.as_deref() == Some(folder.as_path())
                && display_id == "#42"
                && thread_id == "thread-42"
                && body
                    .starts_with("Added the missing validation.\n\n<!-- agentty review resolution:")
                && body.ends_with(" -->")
        })
        .returning(|_, _, _, _| Box::pin(async { Ok(()) }));
    review_request_client
        .expect_resolve_thread()
        .once()
        .in_sequence(sequence)
        .withf(|_, display_id, thread_id| display_id == "#42" && thread_id == "thread-42")
        .returning(|_, _, _| Box::pin(async { Ok(()) }));

    review_request_client
}

/// Returns one git client mock that commits successfully but fails the
/// follow-up auto-push.
pub(super) fn auto_commit_git_client_with_push_failure(commit_message: &str) -> MockGitClient {
    let mut mock_git_client = dirty_auto_commit_git_client(commit_message);
    expect_safe_auto_push_state(&mut mock_git_client);
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(|folder, remote_branch_name| {
            folder.ends_with("sess1") && remote_branch_name == "wt/session-id"
        })
        .returning(|_, _| {
            Box::pin(async {
                Err(ag_git::GitError::CommandFailed {
                    command: "git push origin wt/session-id".to_string(),
                    stderr: "fatal: remote rejected the push".to_string(),
                })
            })
        });

    mock_git_client
}

/// Returns one review-request client mock that expects the latest commit
/// message metadata.
pub(super) fn review_metadata_sync_client(
    base_dir: &std::path::Path,
    sequence: &mut Sequence,
) -> forge::MockReviewRequestClient {
    let folder = base_dir.join("sess1");
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .in_sequence(sequence)
        .returning(|_| Ok(github_forge_remote()));
    mock_review_request_client
        .expect_review_request_metadata()
        .once()
        .in_sequence(sequence)
        .returning(|_, _| {
            Box::pin(async {
                Ok(forge::ReviewRequestMetadata {
                    body: "Old body".to_string(),
                    title: "Old title".to_string(),
                })
            })
        });
    mock_review_request_client
        .expect_sync_review_request_metadata()
        .once()
        .in_sequence(sequence)
        .withf(move |remote, display_id, input| {
            remote.command_working_directory.as_deref() == Some(folder.as_path())
                && display_id == "#42"
                && input.title.as_ref().is_some_and(|title| {
                    title.current == "Old title" && title.desired == "Old title"
                })
                && input.body.as_ref().is_some_and(|body| {
                    body.current == "Old body"
                        && body.desired == "Old body\n\n- Update the linked review request body."
                })
        })
        .returning(|_, _, _| {
            Box::pin(async {
                Ok(forge::ReviewRequestSummary {
                    display_id: "#42".to_string(),
                    forge_kind: forge::ForgeKind::GitHub,
                    source_branch: "wt/session-id".to_string(),
                    state: ReviewRequestState::Open,
                    status_summary: Some("Draft".to_string()),
                    target_branch: "main".to_string(),
                    title: "Old title".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                })
            })
        });

    mock_review_request_client
}

/// Returns one GitHub forge remote fixture for worker metadata sync tests.
pub(super) fn github_forge_remote() -> forge::ForgeRemote {
    forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: forge::ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Returns one successful turn result with the provided answer text.
pub(super) fn successful_turn_result(answer: &str) -> TurnResult {
    TurnResult {
        assistant_message: AgentResponse {
            answer: answer.to_string(),
            questions: Vec::new(),
            review_comment_outcomes: Vec::new(),
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    }
}

/// Returns one completed turn that reports a fixed review thread.
pub(super) fn fixed_review_turn_result() -> TurnResult {
    TurnResult {
        assistant_message: AgentResponse {
            answer: "Implemented the change.".to_string(),
            questions: Vec::new(),
            review_comment_outcomes: vec![ReviewCommentOutcome {
                reply: "Added the missing validation.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-42".to_string(),
            }],
            subtasks: Vec::new(),
            verification_verdicts: Vec::new(),
        },
        context_reset: false,
        input_tokens: 0,
        output_tokens: 0,
        provider_conversation_id: None,
    }
}

/// Test harness for existing-session rebase assistance worker coverage.
pub(super) struct RebaseAssistWorkerHarness {
    pub(super) context: SessionWorkerContext,
    pub(super) db: AppRepositories,
    pub(super) status: Arc<Mutex<Status>>,
}

/// Writes one conflict-marked file used by rebase-assist worker tests.
pub(super) fn write_rebase_conflict_file(base_dir: &std::path::Path) {
    let conflict_file = base_dir.join("src/lib.rs");
    std::fs::create_dir_all(
        conflict_file
            .parent()
            .expect("conflict file should have a parent"),
    )
    .expect("failed to create conflict directory");
    std::fs::write(
        conflict_file,
        concat!(
            "<<",
            "<<<< HEAD\nours\n",
            "===",
            "====\ntheirs\n",
            ">>",
            ">>>>> incoming\n"
        ),
    )
    .expect("failed to write conflict file");
}

/// Seeds one rebasing session with existing provider conversation ids.
pub(super) async fn seed_existing_session_rebase_metadata(
    db: &AppRepositories,
    parent_session_id: Option<&str>,
) {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    if let Some(parent_session_id) = parent_session_id {
        db.sessions()
            .insert_session(
                parent_session_id,
                "gpt-5.6-sol",
                "main",
                "Review",
                project_id,
            )
            .await
            .expect("failed to insert parent session");
        db.sessions()
            .insert_stacked_draft_session(
                "sess1",
                "gpt-5.6-sol",
                "main",
                "Rebasing",
                parent_session_id,
                project_id,
            )
            .await
            .expect("failed to insert stacked session");
    } else {
        db.sessions()
            .insert_session("sess1", "gpt-5.6-sol", "main", "Rebasing", project_id)
            .await
            .expect("failed to insert session");
    }
    db.sessions()
        .update_session_provider_conversation_id("sess1", Some("thread-before".to_string()))
        .await
        .expect("failed to seed provider conversation id");
    db.sessions()
        .update_session_instruction_conversation_id("sess1", Some("instruction-before".to_string()))
        .await
        .expect("failed to seed instruction conversation id");
}

/// Builds the mock channel expected for one existing-session rebase turn.
pub(super) fn mock_existing_session_rebase_channel(
    main_checkout_root: PathBuf,
) -> MockAgentChannel {
    let mut mock_channel = MockAgentChannel::new();
    mock_channel
        .expect_run_turn()
        .times(1)
        .withf(move |session_id, request, _| {
            session_id == "sess1"
                && request.request_kind == AgentRequestKind::UtilityPrompt
                && request.permission_mode == PermissionMode::AutoEdit
                && request.main_checkout_root.as_ref() == Some(&main_checkout_root)
                && request.continuation.provider_conversation_id() == Some("thread-before")
                && request.continuation.persisted_instruction_conversation_id()
                    == Some("instruction-before")
                && request
                    .prompt
                    .text
                    .contains("Resolve conflicts in only these files")
                && request.prompt.text.contains("src/lib.rs")
        })
        .returning(|_, _, _| {
            Box::pin(async {
                Ok(TurnResult {
                    assistant_message: AgentResponse {
                        answer: "Resolved conflicts inside existing session.".to_string(),
                        questions: Vec::new(),
                        review_comment_outcomes: Vec::new(),
                        subtasks: Vec::new(),
                        verification_verdicts: Vec::new(),
                    },
                    context_reset: false,
                    input_tokens: 11,
                    output_tokens: 7,
                    provider_conversation_id: Some("thread-after".to_string()),
                })
            })
        });

    mock_channel
}

/// Builds the git mock expected for one assisted rebase conflict.
pub(super) fn mock_successful_conflict_rebase_git_client(
    main_checkout_root: PathBuf,
) -> MockGitClient {
    let mut mock_git_client = MockGitClient::new();
    let mut sequence = Sequence::new();
    mock_git_client
        .expect_detect_git_info()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(move |_| {
            let main_checkout_root = main_checkout_root.clone();
            Box::pin(async move { Ok(Some(main_checkout_root)) })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_is_rebase_in_progress()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_rebase_start()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, target| {
            assert_eq!(target, "main");
            Box::pin(async {
                Ok(RebaseStepResult::Conflict {
                    detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                })
            })
        });
    mock_git_client
        .expect_list_conflicted_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, paths| {
            assert_eq!(paths, [] as [std::string::String; 0]);
            Box::pin(async { Ok(Vec::new()) })
        });
    mock_git_client
        .expect_stage_all()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_has_unmerged_paths()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, paths| {
            assert_eq!(paths, vec!["src/lib.rs".to_string()]);
            Box::pin(async { Ok(Vec::new()) })
        });
    mock_git_client
        .expect_run_pre_commit_hook()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_rebase_continue()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Box::pin(async { Ok(RebaseStepResult::Completed) }));

    mock_git_client
}

/// Extends the successful rebase mock with a controllable stack-metadata
/// boundary and a terminal review-request push failure.
pub(super) fn blocking_stack_metadata_git_client(
    main_checkout_root: PathBuf,
    metadata_persistence_started: Arc<tokio::sync::Notify>,
    release_metadata_persistence: Arc<tokio::sync::Notify>,
) -> MockGitClient {
    let mut git_client = mock_successful_conflict_rebase_git_client(main_checkout_root);
    git_client
        .expect_ref_hash()
        .once()
        .withf(|_, reference| reference == "main")
        .returning(move |_, _| {
            let metadata_persistence_started = Arc::clone(&metadata_persistence_started);
            let release_metadata_persistence = Arc::clone(&release_metadata_persistence);

            Box::pin(async move {
                metadata_persistence_started.notify_one();
                release_metadata_persistence.notified().await;

                Ok("parent-tip".to_string())
            })
        });
    git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(Some(ag_git::InProgressGitOperation::Rebase)) }));

    git_client
}

/// Builds the review-request command queued behind the controlled rebase.
pub(super) fn queued_review_request_command(folder: PathBuf) -> SessionCommand {
    SessionCommand::CreateReviewRequest {
        branch_publish_session: BranchPublishTaskSession {
            base_branch: "main".to_string(),
            folder,
            id: "sess1".into(),
            published_upstream_ref: None,
            review_request: None,
            status: Status::Rebasing,
        },
        operation_id: "op-review-request".to_string(),
        remote_branch_name: None,
        response: None,
    }
}

/// Builds one worker harness for session rebase command tests.
pub(super) fn rebase_assist_worker_harness(
    base_dir: PathBuf,
    db: AppRepositories,
    build_git_client: impl FnOnce(PathBuf) -> MockGitClient,
) -> RebaseAssistWorkerHarness {
    let main_checkout_root = base_dir.join("main-checkout");
    std::fs::create_dir_all(&main_checkout_root).expect("failed to create main checkout");
    // The worker canonicalizes the resolved main checkout root through the
    // real filesystem client, so the mocks must expect the canonical path
    // (on macOS `/var/...` resolves to `/private/var/...`).
    let expected_main_checkout_root = main_checkout_root
        .canonicalize()
        .expect("failed to canonicalize main checkout");

    let status = Arc::new(Mutex::new(Status::Rebasing));
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(mock_existing_session_rebase_channel(
            expected_main_checkout_root,
        )),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir,
        fs_client: Arc::new(fs::RealFsClient),
        git_client: Arc::new(build_git_client(main_checkout_root)),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Codex,
            AgentModel::Gpt56Sol,
        ),
        status: Arc::clone(&status),
    };

    RebaseAssistWorkerHarness {
        context,
        db,
        status,
    }
}

/// Builds one [`SessionWorkerContext`] backed by the supplied
/// [`MockAgentChannel`], a fresh in-memory database, and the queued
/// prompt list. The session row is pre-inserted as `InProgress` so the
/// worker reaches drainage without first transitioning status.
pub(super) fn queued_message(order: u64, text: &str) -> QueuedMessage {
    QueuedMessage::new(order, TurnPrompt::from_text(text.to_string()))
}

pub(super) async fn queue_test_context(
    channel: MockAgentChannel,
    queued_messages: VecDeque<QueuedMessage>,
    status: Status,
) -> (
    SessionWorkerContext,
    AppRepositories,
    Arc<Mutex<VecDeque<QueuedMessage>>>,
    tempfile::TempDir,
) {
    // Arrange
    let base_dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert");
    db.sessions()
        .insert_session(
            "sess1",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");

    let mut mock_git_client = MockGitClient::new();
    let main_repo_root = base_dir.path().join("main");
    mock_git_client
        .expect_detect_git_info()
        .times(0..)
        .returning(|_| Box::pin(async { Some("wt/sess1".to_string()) }));
    mock_git_client
        .expect_main_checkout_working_tree()
        .times(0..)
        .returning({
            let main_repo_root = main_repo_root.clone();

            move |_| {
                let main_repo_root = main_repo_root.clone();
                Box::pin(async move { Ok(Some(main_repo_root)) })
            }
        });
    mock_git_client
        .expect_main_repo_root()
        .times(0..)
        .returning(move |_| {
            let main_repo_root = main_repo_root.clone();
            Box::pin(async move { Ok(main_repo_root) })
        });
    mock_git_client
        .expect_tracked_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_diff()
        .returning(|_, _| Box::pin(async { Ok(String::new()) }));
    mock_git_client
        .expect_is_worktree_clean()
        .returning(|_| Box::pin(async { Ok(true) }));

    let queue_handle = Arc::new(Mutex::new(queued_messages));
    let context = SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(channel),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: base_dir.path().to_path_buf(),
        fs_client: Arc::new(mock_fs_client_with_existing_directories()),
        git_client: Arc::new(mock_git_client),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: Arc::clone(&queue_handle),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess1".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(status)),
    };

    (context, db, queue_handle, base_dir)
}

/// Builds one [`SessionWorkerContext`] whose only meaningful state is the
/// shared `queued_messages` mutex; every other field is wired with a stub
/// value because these tests only exercise the queue helpers.
pub(super) async fn queue_helper_context(
    queue: Arc<Mutex<VecDeque<QueuedMessage>>>,
) -> SessionWorkerContext {
    SessionWorkerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: Arc::new(Mutex::new(CancellationToken::new())),
        channel: Arc::new(MockAgentChannel::new()),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: AppRepositories::in_memory().await.expect("db should open"),
        folder: PathBuf::new(),
        fs_client: Arc::new(fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        transcript: empty_transcript(),
        personality_catalog_client: Arc::new(RealPersonalityCatalogClient),
        queued_messages: queue,
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "sess".into(),
        session_agent: AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            AgentModel::Gemini38Flash,
        ),
        status: Arc::new(Mutex::new(Status::InProgress)),
    }
}
