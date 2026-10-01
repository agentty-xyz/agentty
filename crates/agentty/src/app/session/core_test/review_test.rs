use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use ag_contracts::{MockAgentChannel, TurnResult};
use ag_protocol::AgentResponse;
use ag_worker::test_support::MockAgentBackend;
use tempfile::tempdir;
use tokio::sync::{Notify, mpsc};

use super::super::{SESSION_REFRESH_INTERVAL, session_folder};
use super::support::{
    TestClock, create_and_start_session, create_mock_backend, new_test_app_with_db,
    new_test_app_with_git, new_test_app_with_git_and_db, prepare_review_comment_resolution_session,
    register_session_backend, review_comment_resolution_snapshot, review_message_body,
    session_replay_text, test_session_manager, wait_for_output_contains, wait_for_status,
};
use crate::app::prompt_intent::ReviewCommentResolutionOutcome;
use crate::app::review::{review_failure_message, review_loading_message};
use crate::app::session::SessionError;
use crate::app::test_support::TestSessionRunFactory;
use crate::app::{App, AppEvent, ReviewCacheEntry, SessionManager};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::question::QuestionItem;
use crate::domain::session::{SESSION_DATA_DIR, SessionId, Status};
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
async fn test_reply_replays_history_after_app_restart_for_review_session() {
    // Arrange
    let dir = tempdir().expect("failed to create temp dir");
    let db = AppRepositories::in_memory().await.expect("db should open");

    let mut first_app = new_test_app_with_git_and_db(dir.path(), db.clone()).await;
    let session_id = first_app
        .create_session()
        .await
        .expect("failed to create session");
    let start_backend = create_mock_backend();
    let channels = TestSessionRunFactory::install(&mut first_app.services);
    register_session_backend(&first_app, &channels, &session_id, Arc::new(start_backend));
    first_app
        .sessions
        .reply(&first_app.services, &session_id, "Initial prompt")
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
    let channels = TestSessionRunFactory::install(&mut resumed_app.services);
    register_session_backend(
        &resumed_app,
        &channels,
        &session_id,
        Arc::new(resume_backend),
    );
    resumed_app
        .sessions
        .reply(&resumed_app.services, &session_id, "Restart reply")
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
        AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeOpus55),
        ReasoningLevel::XHigh,
        SpeedMode::Normal,
    );
    app.review_cache.insert(
        loading_session_id.into(),
        ReviewCacheEntry::Loading {
            request_id: uuid::Uuid::nil(),
            progress: None,
            diff_hash: 43,
            review_agent: loading_review_agent,
        },
    );
    app.review_cache.insert(
        failed_session_id.into(),
        ReviewCacheEntry::Failed {
            request_id: uuid::Uuid::nil(),
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
    let channels = TestSessionRunFactory::install(&mut app.services);
    channels.register(&session_id, Arc::new(mock_channel));
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

#[tokio::test]
async fn review_comment_batch_joins_chat_queue_and_survives_refresh() {
    // Arrange
    let dir = tempdir().expect("test directory");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = prepare_review_comment_resolution_session(&mut app).await;
    let first_turn_release = Arc::new(Notify::new());
    let resolution_release = Arc::new(Notify::new());
    let (started_tx, mut started_rx) = tokio::sync::mpsc::unbounded_channel();
    let channel =
        review_comment_queue_channel(&first_turn_release, &resolution_release, started_tx);
    TestSessionRunFactory::install(&mut app.services).register(&session_id, Arc::new(channel));
    assert!(app.reply(&session_id, "Active turn").await);
    assert_eq!(started_rx.recv().await.expect("first turn"), "Active turn");
    app.sessions.sync_from_handles();
    app.enqueue_message(&session_id, "Earlier queued chat")
        .expect("queue chat");
    let snapshot = review_comment_resolution_snapshot();
    let selections = vec![ReviewCommentSelection {
        thread_id: "thread-42".to_string(),
    }];

    // Act
    let outcome = app
        .resolve_session_review_comments(&session_id, &snapshot, &selections)
        .await;
    app.apply_app_events(AppEvent::RefreshSessions).await;
    let queued_session = &app.sessions.sessions()[0];
    let queued = queued_session
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentQueue)
        .expect("queued row");
    let action = match &queued.body {
        TransientMessageBody::Queued(action) => Some(action),
        _ => None,
    }
    .expect("expected waiting row");
    let action_order = action.order;
    let chat_order = queued_session.queued_messages[0].order();
    let label = action.text.clone();
    assert!(
        queued_session
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
    let duplicate = app
        .resolve_session_review_comments(&session_id, &snapshot, &selections)
        .await;
    assert_review_prompt_history(&app, &session_id, false).await;
    first_turn_release.notify_one();
    let chat = started_rx.recv().await.expect("queued chat");
    let resolution = started_rx.recv().await.expect("resolution turn");
    assert_review_prompt_history(&app, &session_id, true).await;
    app.process_pending_app_events().await;
    let active = &app.sessions.sessions()[0];
    let loading = active
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentResolution)
        .map(|message| message.body.text().to_string());
    let queue_cleared = active
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentQueue)
        .is_none();
    resolution_release.notify_one();
    wait_for_status(&mut app, &session_id, Status::Review).await;
    app.process_pending_app_events().await;

    // Assert
    assert_eq!(
        outcome,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone()
        }
    );
    assert_eq!(
        duplicate,
        ReviewCommentResolutionOutcome::KeepReviewComments
    );
    assert!(chat_order < action_order);
    assert_eq!(label, "resolve 1 review comment");
    assert_eq!(chat, "Earlier queued chat");
    assert!(resolution.contains("Thread ID: thread-42"));
    assert_eq!(loading.as_deref(), Some("Resolving 1 review comment..."));
    assert!(queue_cleared);
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
}

/// Checks that generated context is persisted after earlier completed turns.
async fn assert_review_prompt_history(app: &App, session_id: &str, started: bool) {
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(session_id)
        .await
        .expect("persisted history");
    let review_index = messages
        .iter()
        .position(|message| message.content.contains("Thread ID: thread-42"));
    if started {
        let review_index = review_index.expect("executing review context");
        assert_eq!(messages[review_index].kind, "agent_prompt");
        let chat_index = messages
            .iter()
            .position(|message| message.content == "Earlier queued chat")
            .expect("earlier chat prompt");
        let first_answer = messages
            .iter()
            .position(|message| message.content.contains("Completed queued work"))
            .expect("active answer");
        let last_answer = messages
            .iter()
            .rposition(|message| message.content.contains("Completed queued work"))
            .expect("chat answer");
        assert!(
            first_answer < chat_index && chat_index < last_answer && last_answer < review_index
        );
    } else {
        assert!(
            review_index.is_none(),
            "waiting review must not enter replay history"
        );
    }
}

#[tokio::test]
async fn review_comment_batch_preserves_pending_question_without_earlier_work() {
    // Arrange, Act, Assert
    Box::pin(assert_review_comment_batch_preserves_question(false, false)).await;
}

#[tokio::test]
async fn restored_question_accepts_review_comment_batch_without_worker() {
    // Arrange, Act, Assert
    Box::pin(assert_review_comment_batch_preserves_question(true, false)).await;
}

#[tokio::test]
async fn question_survives_restart_after_queued_review_comments() {
    // Arrange, Act, Assert
    Box::pin(assert_review_comment_batch_preserves_question(true, true)).await;
}

/// Checks question retention, deferred history, and answer-before-review order.
async fn assert_review_comment_batch_preserves_question(
    restored: bool,
    recover_queued_review: bool,
) {
    // Arrange
    let directory = tempdir().expect("failed to create temp dir");
    let mut app = new_test_app_with_git(directory.path()).await;
    let session_id = prepare_review_comment_resolution_session(&mut app).await;
    if restored {
        app = restore_review_question_app(app, directory.path(), &session_id).await;
    }
    if recover_queued_review {
        app = Box::pin(recover_queued_review_question_app(
            app,
            directory.path(),
            &session_id,
        ))
        .await;
    }
    let release = Arc::new(Notify::new());
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let channel =
        review_comment_question_channel(&release, started_tx, if restored { 2 } else { 3 });
    TestSessionRunFactory::install(&mut app.services).register(&session_id, Arc::new(channel));
    if !restored {
        assert!(app.reply(&session_id, "Request clarification").await);
        assert_eq!(
            started_rx.recv().await.expect("initial turn"),
            "Request clarification"
        );
        wait_for_status(&mut app, &session_id, Status::Question).await;
    }
    let selections = vec![ReviewCommentSelection {
        thread_id: "thread-42".to_string(),
    }];

    // Act
    let outcome = app
        .resolve_session_review_comments(
            &session_id,
            &review_comment_resolution_snapshot(),
            &selections,
        )
        .await;
    app.process_pending_app_events().await;
    let session = &app.sessions.sessions()[0];
    let persisted = app
        .services
        .db()
        .sessions()
        .load_session(&session_id)
        .await
        .expect("persisted session")
        .expect("session row");
    let messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("persisted history");

    // Assert: accepting the batch preserves the clarification and defers its
    // prompt.
    assert_eq!(
        outcome,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone()
        }
    );
    assert_eq!(session.status, Status::Question);
    assert_eq!(session.queued_messages.len(), 0);
    assert!(
        session
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentQueue)
            .is_some()
    );
    assert!(
        session
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
    assert!(
        persisted
            .questions
            .as_deref()
            .is_some_and(|text| text.contains("Use existing behavior?"))
    );
    assert!(
        !messages
            .iter()
            .any(|message| message.content.contains("Thread ID: thread-42"))
    );
    assert!(started_rx.try_recv().is_err());

    // Act: only the explicit answer resumes execution, followed by the review.
    assert!(
        app.sessions
            .reply_to_question_answers(&app.services, &session_id, "Use existing behavior")
            .await
    );
    let answer = started_rx.recv().await.expect("answer turn");
    let review = started_rx.recv().await.expect("review turn");
    release.notify_one();
    wait_for_status(&mut app, &session_id, Status::Review).await;

    // Assert
    assert_eq!(answer, "Use existing behavior");
    assert!(review.contains("Thread ID: thread-42"));
}

/// Reloads a persisted clarification into a fresh app with no live workers.
async fn restore_review_question_app(app: App, path: &Path, session_id: &SessionId) -> App {
    let db = app.services.db().clone();
    db.sessions()
        .update_session_status_with_timing_at(session_id, "Question", 0)
        .await
        .expect("persist question status");
    db.sessions()
        .update_session_questions(
            session_id,
            r#"[{"text":"Use existing behavior?","options":[]}]"#,
        )
        .await
        .expect("persist clarification");
    drop(app);

    new_test_app_with_git_and_db(path, db).await
}

/// Queues real review work, recovers its durable operation, and reopens the
/// session without executing or persisting the abandoned review prompt.
async fn recover_queued_review_question_app(
    mut app: App,
    path: &Path,
    session_id: &SessionId,
) -> App {
    let (started_tx, _started_rx) = mpsc::unbounded_channel();
    let channel = review_comment_question_channel(&Arc::new(Notify::new()), started_tx, 0);
    TestSessionRunFactory::install(&mut app.services).register(session_id, Arc::new(channel));
    let outcome = app
        .resolve_session_review_comments(
            session_id,
            &review_comment_resolution_snapshot(),
            &[ReviewCommentSelection {
                thread_id: "thread-42".to_string(),
            }],
        )
        .await;
    assert_eq!(
        outcome,
        ReviewCommentResolutionOutcome::ShowSession {
            session_id: session_id.clone()
        }
    );
    let db = app.services.db().clone();
    let operations = db
        .operations()
        .load_unfinished_session_operations()
        .await
        .expect("queued review operation");
    assert_eq!(operations.len(), 1);
    assert_eq!(operations[0].session_id, session_id.as_str());
    assert_eq!(operations[0].kind, "reply");
    assert_eq!(operations[0].status, "queued");
    assert_eq!(operations[0].started_at, None);

    SessionManager::fail_unfinished_operations_from_previous_run(
        db.clone(),
        app.services.base_path().to_path_buf(),
        app.services.git_client(),
        app.services.clock(),
    )
    .await
    .expect("recover queued review");
    assert!(
        !db.operations()
            .is_session_operation_unfinished(&operations[0].id)
            .await
            .expect("interrupted operation")
    );
    let row = db
        .sessions()
        .load_session(session_id)
        .await
        .expect("recovered session")
        .expect("session row");
    assert_eq!(row.status, "Question");
    assert!(
        row.questions
            .as_deref()
            .is_some_and(|questions| questions.contains("Use existing behavior?"))
    );
    drop(app);

    new_test_app_with_git_and_db(path, db).await
}

/// Provides clarification, its explicit answer, and a deferred review turn.
fn review_comment_question_channel(
    release: &Arc<Notify>,
    started_tx: mpsc::UnboundedSender<String>,
    turn_count: usize,
) -> MockAgentChannel {
    let mut channel = MockAgentChannel::new();
    let release = Arc::clone(release);
    channel
        .expect_run_turn()
        .times(turn_count)
        .returning(move |_, request, _| {
            let prompt = request.prompt.text;
            let release = Arc::clone(&release);
            let started_tx = started_tx.clone();
            Box::pin(async move {
                started_tx
                    .send(prompt.clone())
                    .expect("report started turn");
                let mut response = AgentResponse::plain("Completed turn");
                if prompt == "Request clarification" {
                    response.questions = vec![QuestionItem::new("Use existing behavior?")];
                } else if prompt.contains("Thread ID: thread-42") {
                    release.notified().await;
                }
                Ok(TurnResult {
                    assistant_message: response,
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });
    channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    channel
}

/// Builds three deferred turns for the mixed chat/comment queue scenario.
fn review_comment_queue_channel(
    first_turn_release: &Arc<Notify>,
    resolution_release: &Arc<Notify>,
    started_tx: mpsc::UnboundedSender<String>,
) -> MockAgentChannel {
    let mut channel = MockAgentChannel::new();
    let first_release = Arc::clone(first_turn_release);
    let last_release = Arc::clone(resolution_release);
    channel
        .expect_run_turn()
        .times(3)
        .returning(move |_, request, _| {
            let prompt = request.prompt.text;
            let first_release = Arc::clone(&first_release);
            let last_release = Arc::clone(&last_release);
            let started_tx = started_tx.clone();
            Box::pin(async move {
                started_tx
                    .send(prompt.clone())
                    .expect("report started turn");
                if prompt == "Active turn" {
                    first_release.notified().await;
                }
                if prompt.contains("Thread ID: thread-42") {
                    last_release.notified().await;
                }
                Ok(TurnResult {
                    assistant_message: AgentResponse::plain("Completed queued work"),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    provider_conversation_id: None,
                })
            })
        });
    channel
        .expect_shutdown_session()
        .returning(|_| Box::pin(async { Ok(()) }));

    channel
}

#[tokio::test]
async fn review_comment_resolution_projection_handles_skips_missing_sessions_and_coalesced_events()
{
    // Arrange
    let dir = tempdir().expect("test directory");
    let mut app = new_test_app_with_git(dir.path()).await;
    let session_id = prepare_review_comment_resolution_session(&mut app).await;
    app.sessions
        .queue_review_comment_resolution(&session_id, 0, 2);

    // Act
    app.sessions
        .update_review_comment_resolution("missing", Some(2));
    app.sessions
        .update_review_comment_resolution(&session_id, Some(0));
    let skipped_row_removed = app.sessions.sessions()[0]
        .transient_messages
        .get(TransientMessageSlot::ReviewCommentQueue)
        .is_none();
    app.services
        .emit_app_event(AppEvent::SessionReviewCommentResolutionUpdated {
            comment_count: Some(2),
            session_id: session_id.clone(),
        });
    app.services
        .emit_app_event(AppEvent::SessionReviewCommentResolutionUpdated {
            comment_count: None,
            session_id: session_id.clone(),
        });
    app.process_pending_app_events().await;

    // Assert
    assert!(skipped_row_removed);
    assert!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::ReviewCommentResolution)
            .is_none()
    );
}
