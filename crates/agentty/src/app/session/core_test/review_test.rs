use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use ag_agent::{MockAgentBackend, MockAgentChannel, TurnResult};
use ag_protocol::AgentResponse;
use tempfile::tempdir;
use tokio::sync::Notify;

use super::super::{SESSION_REFRESH_INTERVAL, session_folder};
use super::support::{
    TestClock, create_and_start_session, create_mock_backend, new_test_app_with_db,
    new_test_app_with_git, new_test_app_with_git_and_db, prepare_review_comment_resolution_session,
    review_comment_resolution_snapshot, review_message_body, session_replay_text,
    test_session_manager, wait_for_output_contains, wait_for_status,
};
use crate::app::ReviewCacheEntry;
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::app::review::{review_failure_message, review_loading_message};
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::session::{SESSION_DATA_DIR, Status};
use crate::domain::session_message::SessionMessageKind;
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::clock::Clock;
use crate::infra::db::AppRepositories;
use crate::presentation::app_mode::ReviewCommentSelection;

#[test]
fn test_finish_review_request_publish_keeps_loading_review_at_tail() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Loading("Reviewing changes...".to_string()),
            lifecycle: TransientMessageLifecycle::UntilResolved,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
    session_manager.start_branch_publish("session-id", "Publishing review request...".to_string());

    // Act
    let finished = session_manager.finish_review_request_publish(
        "session-id",
        "[Review Request] Created PR https://example.test/pull/42",
    );

    // Assert
    assert!(finished);
    let transient_messages = &session_manager.sessions()[0].transient_messages;
    assert_eq!(
        transient_messages
            .get(TransientMessageSlot::Review)
            .expect("loading review should remain visible")
            .anchor,
        TransientMessageAnchor::Tail
    );
    assert!(
        transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
}

#[test]
fn test_finish_review_request_publish_reports_unloaded_handle_update() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.state_mut().replace_sessions(Vec::new());

    // Act
    let finished = session_manager.finish_review_request_publish(
        "session-id",
        "[Review Request] Created PR https://example.test/pull/42",
    );

    // Assert
    assert!(finished);
    let transcript = session_manager
        .state()
        .handle("session-id")
        .expect("session handles should remain loaded")
        .transcript
        .lock()
        .expect("session transcript lock should succeed");
    assert_eq!(
        transcript
            .messages()
            .last()
            .map(|message| (message.kind, message.content.as_str())),
        Some((
            SessionMessageKind::WorkflowNotice,
            "[Review Request] Created PR https://example.test/pull/42",
        ))
    );
}

#[tokio::test]
async fn test_append_session_to_stack_rejects_non_review_source() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let session_id = app.create_session().await.expect("failed to create source");
    crate::test_support::set_session_status_for_test(&mut app, &parent_session_id, Status::Review);

    // Act
    let result = app
        .append_session_to_stack(&session_id, &parent_session_id)
        .await;

    // Assert
    assert!(matches!(
        result,
        Err(crate::app::AppError::Session(SessionError::Workflow(message)))
            if message.contains("Review")
    ));
}

/// Ensures resumed review sessions replay persisted transcript output on
/// the first reply after app restart.
#[tokio::test]
async fn test_reply_with_backend_replays_history_after_app_restart_for_review_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");

    let mut first_app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
    let session_id = first_app
        .create_session()
        .await
        .expect("failed to create session");
    let start_backend = create_mock_backend();
    first_app
        .sessions
        .reply_with_backend(
            &first_app.services,
            &session_id,
            "Initial prompt",
            Arc::new(start_backend),
            AgentModel::ClaudeSonnet5,
        )
        .await;
    wait_for_status(&mut first_app, &session_id, Status::Review).await;
    first_app.sessions.sync_from_handles();
    let first_run_output = first_app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map(session_replay_text)
        .expect("missing persisted session");
    assert!(first_run_output.contains("Initial prompt"));
    assert!(first_run_output.contains("mock-start"));
    drop(first_app);

    let mut resumed_app = new_test_app_with_git_and_db(dir.path(), db).await;
    let resumed_session = resumed_app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing resumed session");
    assert_eq!(resumed_session.status, Status::Review);

    // Act
    let mut resume_backend = MockAgentBackend::new();
    resume_backend.expect_build_command().returning(|request| {
        assert!(request.request_kind.is_resume());

        let replay_transcript = request
            .replay_transcript
            .expect("expected replayed session transcript after restart");
        assert!(replay_transcript.contains("Initial prompt"));
        assert!(replay_transcript.contains("mock-start"));

        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf '{\"answer\":\"replayed-after-restart\",\"questions\":[]}'")
            .current_dir(request.folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(cmd)
    });
    resumed_app
        .sessions
        .reply_with_backend(
            &resumed_app.services,
            &session_id,
            "Restart reply",
            Arc::new(resume_backend),
            AgentModel::ClaudeSonnet5,
        )
        .await;

    // Assert
    wait_for_output_contains(
        &mut resumed_app,
        &session_id,
        "replayed-after-restart",
        2000,
    )
    .await;
    wait_for_status(&mut resumed_app, &session_id, Status::Review).await;
}

#[tokio::test]
async fn test_start_staged_session_rejects_stacked_child_before_parent_review() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let parent_session_id = app.create_session().await.expect("failed to create parent");
    let parent_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == parent_session_id)
        .expect("expected parent session");
    parent_session.status = Status::InProgress;
    let child_session_id = app
        .create_stacked_draft_session(&parent_session_id)
        .await
        .expect("failed to create stacked draft session");
    app.stage_draft_message(&child_session_id, "Stacked draft")
        .await
        .expect("failed to stage stacked draft message");

    // Act
    let result = app.start_staged_session(&child_session_id).await;

    // Assert
    let error = result.expect_err("parent branch work should block child start");
    assert!(error.to_string().contains("parent is in review"));
}

#[tokio::test]
async fn test_periodic_session_refresh_preserves_focused_review_states() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/test", None)
        .await
        .expect("failed to upsert project");
    let ready_session_id = "ready000";
    let loading_session_id = "loading0";
    let failed_session_id = "failed00";
    let review_text = "## Review\nPersisted focused review finding.";
    let review_error = "empty provider response";
    for session_id in [ready_session_id, loading_session_id, failed_session_id] {
        db.sessions()
            .insert_session(
                session_id,
                "gemini-3.8-flash",
                "main",
                &Status::Review.to_string(),
                project_id,
            )
            .await
            .expect("failed to insert review session");
        let data_dir = session_folder(dir.path(), session_id).join(SESSION_DATA_DIR);
        std::fs::create_dir_all(data_dir).expect("failed to create session data dir");
    }
    db.sessions()
        .update_session_focused_review(
            ready_session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some(review_text.to_string()),
        )
        .await
        .expect("failed to persist focused review");
    let mut app = new_test_app_with_db(
        dir.path().to_path_buf(),
        PathBuf::from("/tmp/test"),
        None,
        db.clone(),
    )
    .await;
    let loading_review_agent = (
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus5),
        ReasoningLevel::XHigh,
        SpeedMode::Normal,
    );
    app.review_cache.insert(
        loading_session_id.into(),
        ReviewCacheEntry::Loading {
            diff_hash: 43,
            review_agent: loading_review_agent,
        },
    );
    app.review_cache.insert(
        failed_session_id.into(),
        ReviewCacheEntry::Failed {
            diff_hash: 44,
            error: review_error.to_string(),
        },
    );
    crate::app::review::hydrate_review_transients(&app.review_cache, app.sessions.state_mut());
    app.settings.default_review_selection =
        AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Terra);
    app.settings.default_review_reasoning_level = ReasoningLevel::Low;
    app.settings.default_review_speed_mode = SpeedMode::Fast;
    let clock = Arc::new(TestClock::new(Instant::now(), SystemTime::now()));
    app.sessions.state_mut().clock = clock.clone();
    app.sessions.state_mut().refresh_deadline = clock.now_instant() + SESSION_REFRESH_INTERVAL;
    db.sessions()
        .update_session_updated_at(ready_session_id, 4_000_000_000)
        .await
        .expect("failed to update session timestamp");
    clock.advance(SESSION_REFRESH_INTERVAL);

    // Act
    let refreshed = app.refresh_sessions_if_needed().await;

    // Assert
    assert!(refreshed);
    let ready_message = review_message_body(&app, ready_session_id);
    assert_eq!(ready_message.text(), review_text);

    let loading_message = review_message_body(&app, loading_session_id);
    assert!(matches!(loading_message, TransientMessageBody::Loading(_)));
    assert_eq!(
        loading_message.text(),
        review_loading_message(loading_review_agent)
    );

    let failed_message = review_message_body(&app, failed_session_id);
    assert!(matches!(failed_message, TransientMessageBody::Plain(_)));
    assert_eq!(failed_message.text(), review_failure_message(review_error));
}

#[tokio::test]
async fn test_resolve_session_review_comments_enqueues_turn_and_clears_focused_review() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = prepare_review_comment_resolution_session(&mut app).await;
    let snapshot = review_comment_resolution_snapshot();
    let (done_tx, mut done_rx) = tokio::sync::mpsc::unbounded_channel();
    let turn_release = Arc::new(Notify::new());
    let mut mock_channel = MockAgentChannel::new();
    let turn_release_for_agent = Arc::clone(&turn_release);
    mock_channel
        .expect_run_turn()
        .once()
        .returning(move |_, request, _| {
            assert!(request.prompt.text.contains("Thread ID: thread-42"));
            let done_tx = done_tx.clone();
            let turn_release = Arc::clone(&turn_release_for_agent);

            Box::pin(async move {
                let _ = done_tx.send(());
                turn_release.notified().await;

                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("Resolved the review comment."),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });
    mock_channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));
    app.sessions
        .worker_service
        .test_agent_channels
        .insert(session_id.clone(), Arc::new(mock_channel));
    let selected_comments = vec![ReviewCommentSelection {
        thread_id: "thread-42".to_string(),
    }];

    // Act
    let outcome = app
        .resolve_session_review_comments(&session_id, &snapshot, &selected_comments)
        .await;
    done_rx.recv().await.expect("turn should start");
    app.sessions.sync_from_handles();
    let active_session = &app.sessions.sessions()[0];
    let active_status = active_session.status;
    let resolution_loader = active_session
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentResolution)
        .map(|message| message.body.text().to_string());
    let generated_prompt_kind = active_session
        .transcript
        .as_ref()
        .and_then(|transcript| transcript.messages().last())
        .map(|message| message.kind);
    turn_release.notify_one();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    let focused_reviews = app
        .services
        .db()
        .sessions()
        .load_session_focused_reviews_for_project(app.active_project_id())
        .await
        .expect("failed to load focused reviews");
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(session_id.as_str())
        .await
        .expect("session messages should load");
    let persisted_generated_prompt = persisted_messages
        .iter()
        .find(|message| message.content.contains("Thread ID: thread-42"));

    // Assert
    assert_eq!(
        outcome,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone(),
        }
    );
    assert!(!app.review_cache.contains_key(&session_id));
    assert_eq!(active_status, Status::InProgress);
    assert_eq!(
        resolution_loader.as_deref(),
        Some("Resolving 1 review comment...")
    );
    assert_eq!(
        generated_prompt_kind,
        Some(crate::domain::session_message::SessionMessageKind::AgentPrompt)
    );
    assert!(matches!(
        persisted_generated_prompt,
        Some(message) if message.kind == "agent_prompt"
    ));
    assert_eq!(
        focused_reviews,
        [] as [crate::infra::db::SessionFocusedReviewRow; 0]
    );
}

#[tokio::test]
async fn test_reply_to_parent_allows_review_ready_stacked_child() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(dir.path()).await;
    create_and_start_session(&mut app, "Initial").await;
    let parent_session_id = app.sessions.sessions()[0].id.clone();
    wait_for_status(&mut app, &parent_session_id, Status::Review).await;
    let child_session_id = app
        .create_draft_session()
        .await
        .expect("failed to create child session");
    let child_session = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == child_session_id)
        .expect("expected child session");
    child_session.parent_session_id = Some(parent_session_id.clone());
    child_session.status = Status::Review;

    // Act
    app.reply(&parent_session_id, "Parent follow-up").await;

    // Assert
    app.sessions.sync_from_handles();
    let parent_session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == parent_session_id)
        .expect("expected parent session");
    assert!(session_replay_text(parent_session).contains("Parent follow-up"));
}

#[test]
fn test_finish_branch_publish_promotes_result_when_project_snapshot_is_unloaded() {
    // Arrange
    let mut session_manager = test_session_manager("session-id", None);
    session_manager.state_mut().replace_sessions(Vec::new());

    // Act
    let persistent_notice = session_manager.finish_branch_publish(
        "session-id",
        TransientMessageBody::Markdown("**Branch push failed**\n\nRemote rejected".to_string()),
    );

    // Assert
    assert_eq!(
        persistent_notice.as_deref(),
        Some("**Branch push failed**\n\nRemote rejected")
    );
    let transcript = session_manager
        .state()
        .handle("session-id")
        .expect("session handles should remain loaded")
        .transcript
        .lock()
        .expect("session transcript lock should succeed");
    let notice = transcript
        .messages()
        .last()
        .expect("branch publish result should be promoted to the transcript");
    assert_eq!(notice.kind, SessionMessageKind::WorkflowNotice);
    assert_eq!(notice.content, "**Branch push failed**\n\nRemote rejected");
}
