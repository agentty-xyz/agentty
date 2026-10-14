use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_contracts::OneShotError;
use ag_forge as forge;
use ag_git::{GitError, MockGitClient};
use ag_protocol::{ReviewCommentOutcome, ReviewCommentResolution};
use ag_session::test_support as model_fixture;
use ag_worker::MockRunClient;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::super::worker::TurnMetadata;
use super::{
    PostTurnContext, TURN_ERROR_NOTICE_MAX_CHARS, prepare_review_comment_resolutions,
    run_auto_commit, start_published_branch_auto_push, truncate_turn_error_notice,
    validate_review_comment_outcomes,
};
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::AppRepositories;

/// Builds the narrow post-turn context used by review-operation tests.
fn review_operation_test_context(
    db: AppRepositories,
    git_client: MockGitClient,
) -> PostTurnContext {
    PostTurnContext {
        app_event_tx: mpsc::unbounded_channel().0,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: CancellationToken::new(),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(git_client),
        run_client: Arc::new(MockRunClient::new()),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "session-id".into(),
        transcript: Arc::new(Mutex::new(SessionTranscript::default())),
    }
}

/// Links the review request fixture required to prepare durable outcomes.
async fn link_review_operation_test_request(db: &AppRepositories) {
    db.reviews()
        .update_session_review_request(
            "session-id",
            Some(crate::domain::session::ReviewRequest {
                last_refreshed_at: 100,
                summary: forge::ReviewRequestSummary {
                    display_id: "#42".to_string(),
                    forge_kind: forge::ForgeKind::GitHub,
                    source_branch: "wt/session-id".to_string(),
                    state: crate::domain::session::ReviewRequestState::Open,
                    status_summary: None,
                    target_branch: "main".to_string(),
                    title: "Review title".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                },
            }),
        )
        .await
        .expect("failed to link review request");
}

#[test]
fn test_truncate_turn_error_notice_keeps_short_errors_intact() {
    // Arrange
    let error_text = "  Agent command failed with exit code 1.  ";

    // Act
    let notice = truncate_turn_error_notice(error_text);

    // Assert
    assert_eq!(notice, "Agent command failed with exit code 1.");
}

#[test]
fn test_truncate_turn_error_notice_bounds_long_provider_dumps() {
    // Arrange
    let error_text = "x".repeat(TURN_ERROR_NOTICE_MAX_CHARS + 50);

    // Act
    let notice = truncate_turn_error_notice(&error_text);

    // Assert
    assert_eq!(
        notice.chars().count(),
        TURN_ERROR_NOTICE_MAX_CHARS + "\n[error truncated]".chars().count()
    );
    assert!(notice.ends_with("\n[error truncated]"));
}

#[test]
fn test_review_comment_outcomes_require_exactly_one_valid_item_per_thread() {
    // Arrange
    let allowed_thread_ids = vec!["thread-fixed".to_string(), "thread-other".to_string()];
    let outcomes = vec![
        ReviewCommentOutcome {
            reply: "  Applied the validation.  ".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-fixed".to_string(),
        },
        ReviewCommentOutcome {
            reply: "Duplicate outcome.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-fixed".to_string(),
        },
        ReviewCommentOutcome {
            reply: "No change needed.".to_string(),
            resolution: ReviewCommentResolution::NoChangeNeeded,
            thread_id: "thread-other".to_string(),
        },
        ReviewCommentOutcome {
            reply: "Unknown thread.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-unknown".to_string(),
        },
        ReviewCommentOutcome {
            reply: "   ".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-other".to_string(),
        },
    ];

    // Act
    let validation = validate_review_comment_outcomes(&allowed_thread_ids, &outcomes);

    // Assert
    assert_eq!(validation.accepted_count, 2);
    assert_eq!(validation.expected_count, 2);
    assert!(!validation.is_complete);
    assert_eq!(validation.outcomes, Vec::new());
}

#[test]
fn test_review_comment_outcomes_normalize_complete_allowlisted_response() {
    // Arrange
    let allowed_thread_ids = vec!["thread-fixed".to_string(), "thread-other".to_string()];
    let outcomes = vec![
        ReviewCommentOutcome {
            reply: "  Applied the validation.  ".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-fixed".to_string(),
        },
        ReviewCommentOutcome {
            reply: "No change needed.".to_string(),
            resolution: ReviewCommentResolution::NoChangeNeeded,
            thread_id: "thread-other".to_string(),
        },
        ReviewCommentOutcome {
            reply: "Unknown thread.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-unknown".to_string(),
        },
    ];

    // Act
    let validation = validate_review_comment_outcomes(&allowed_thread_ids, &outcomes);

    // Assert
    assert_eq!(validation.accepted_count, 2);
    assert_eq!(validation.expected_count, 2);
    assert!(validation.is_complete);
    assert_eq!(
        validation.outcomes,
        vec![
            ReviewCommentOutcome {
                reply: "Applied the validation.".to_string(),
                resolution: ReviewCommentResolution::Fixed,
                thread_id: "thread-fixed".to_string(),
            },
            ReviewCommentOutcome {
                reply: "No change needed.".to_string(),
                resolution: ReviewCommentResolution::NoChangeNeeded,
                thread_id: "thread-other".to_string(),
            }
        ]
    );
}

#[tokio::test]
async fn review_comment_operations_preserve_complete_outcomes() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&db, "session-id").await;
    link_review_operation_test_request(&db).await;
    let context = review_operation_test_context(db, MockGitClient::new());
    let thread_ids = vec!["thread-fixed".to_string(), "thread-other".to_string()];
    let outcomes = vec![
        ReviewCommentOutcome {
            reply: "Applied the fix.".to_string(),
            resolution: ReviewCommentResolution::Fixed,
            thread_id: "thread-fixed".to_string(),
        },
        ReviewCommentOutcome {
            reply: "No change is needed.".to_string(),
            resolution: ReviewCommentResolution::NoChangeNeeded,
            thread_id: "thread-other".to_string(),
        },
    ];

    // Act
    let operations = prepare_review_comment_resolutions(&context, &thread_ids, &outcomes)
        .await
        .expect("review operations should be prepared");

    // Assert
    assert_eq!(operations.len(), 2);
    assert_eq!(operations[0].resolution, "fixed");
    assert_eq!(operations[1].resolution, "no_change_needed");
    assert!(operations.iter().all(|operation| {
        operation.commit_hash.is_none()
            && operation.review_request_display_id == "#42"
            && !operation.reply_token.is_empty()
    }));
    assert_ne!(operations[0].reply_token, operations[1].reply_token);
}

#[tokio::test]
async fn review_comment_operations_require_a_linked_review_request() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&db, "session-id").await;
    let context = review_operation_test_context(db, MockGitClient::new());
    let outcomes = vec![ReviewCommentOutcome {
        reply: "Applied the fix.".to_string(),
        resolution: ReviewCommentResolution::Fixed,
        thread_id: "thread-fixed".to_string(),
    }];

    // Act
    let error =
        prepare_review_comment_resolutions(&context, &["thread-fixed".to_string()], &outcomes)
            .await
            .expect_err("a linked review request should be required");
    let transcript = context
        .transcript
        .lock()
        .expect("transcript lock should remain usable")
        .replay_text()
        .expect("persistence warning should be rendered");

    // Assert
    assert!(
        error
            .to_string()
            .contains("the session no longer has a linked review request")
    );
    assert!(transcript.contains(
        "Could not save the review-comment operation, so this response will not post replies"
    ));
}

#[tokio::test]
async fn review_comment_operations_report_link_lookup_failures() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_review_session(&db, "session-id").await;
    let context = review_operation_test_context(db, MockGitClient::new());
    let outcomes = vec![ReviewCommentOutcome {
        reply: "Applied the fix.".to_string(),
        resolution: ReviewCommentResolution::Fixed,
        thread_id: "thread-fixed".to_string(),
    }];
    pool.close().await;

    // Act
    let error =
        prepare_review_comment_resolutions(&context, &["thread-fixed".to_string()], &outcomes)
            .await
            .expect_err("review request lookup should fail");
    let transcript = context
        .transcript
        .lock()
        .expect("transcript lock should remain usable")
        .replay_text()
        .expect("persistence warning should be rendered");

    // Assert
    assert!(error.to_string().contains("closed pool"));
    assert!(transcript.contains(
        "Could not save the review-comment operation, so this response will not post replies"
    ));
}

#[tokio::test]
async fn ordinary_commit_failures_do_not_emit_review_comment_warnings() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_review_session(&db, "session-id").await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_is_worktree_clean()
        .once()
        .returning(|_| Box::pin(async { Err(GitError::OutputParse("commit failed".to_string())) }));
    let mut context = review_operation_test_context(db, git_client);
    let mut run_client = MockRunClient::new();
    run_client
        .expect_submit()
        .once()
        .returning(|_| Err(OneShotError::new("commit failed")));
    context.run_client = Arc::new(run_client);
    let session_agent = model_fixture::codex_selection();

    // Act
    let result = run_auto_commit(&context, session_agent, false, &[])
        .await
        .expect("commit failure handling should remain recoverable");
    let transcript = context
        .transcript
        .lock()
        .expect("transcript lock should remain usable")
        .replay_text()
        .expect("commit failure should be rendered");

    // Assert
    assert_eq!(result, (false, None));
    assert!(transcript.contains("[Commit Error] commit failed"));
    assert!(!transcript.contains("[Review Comments Warning]"));
}

#[tokio::test]
async fn test_unfinished_rebase_check_fails_closed_when_operation_query_fails() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let context = PostTurnContext {
        app_event_tx: mpsc::unbounded_channel().0,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        cancel_token: CancellationToken::new(),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: PathBuf::new(),
        git_client: Arc::new(MockGitClient::new()),
        run_client: Arc::new(MockRunClient::new()),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "session-id".into(),
        transcript: Arc::new(Mutex::new(SessionTranscript::default())),
    };

    // Act
    let should_skip_auto_push = context.has_unfinished_branch_operation().await;

    // Assert
    assert!(
        should_skip_auto_push,
        "operation-query failures must suppress post-turn auto-push"
    );
}

#[tokio::test]
async fn test_auto_push_rechecks_queued_rebase_after_waiting_for_branch_lock() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "session-id",
            "gemini-3.8-flash",
            "main",
            "InProgress",
            project_id,
        )
        .await
        .expect("failed to insert session");
    let branch_operation_lock = Arc::new(tokio::sync::Mutex::new(()));
    let enqueue_guard = Arc::clone(&branch_operation_lock).lock_owned().await;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .never();
    let context = Arc::new(PostTurnContext {
        app_event_tx,
        available_agent_kinds: Arc::from(AgentKind::ALL),
        branch_operation_lock,
        cancel_token: CancellationToken::new(),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: PathBuf::new(),
        git_client: Arc::new(mock_git_client),
        run_client: Arc::new(MockRunClient::new()),
        queued_messages: Arc::new(Mutex::new(VecDeque::new())),
        review_request_client: Arc::new(forge::MockReviewRequestClient::new()),
        session_update_versions: Arc::default(),
        session_id: "session-id".into(),
        transcript: Arc::new(Mutex::new(SessionTranscript::default())),
    });
    let auto_push_task = {
        let context = Arc::clone(&context);

        tokio::spawn(async move {
            start_published_branch_auto_push(
                &context,
                TurnMetadata {
                    published_upstream_ref: Some("origin/wt/session-id".to_string()),
                    review_comment_thread_ids: Vec::new(),
                    session_agent: AgentSelection::new(
                        AgentKind::Antigravity,
                        AgentModel::Gemini38Flash,
                    ),
                },
                None,
            )
            .await
            .expect("queued sync should suppress auto-push");
        })
    };
    db.operations()
        .insert_session_operation("queued-sync", "session-id", "rebase")
        .await
        .expect("failed to insert queued sync");

    // Act
    drop(enqueue_guard);
    auto_push_task.await.expect("auto-push task should join");

    // Assert
    assert!(
        app_event_rx.try_recv().is_err(),
        "the queued sync should retain publish ownership"
    );
}

#[tokio::test]
async fn revalidated_identical_reply_keeps_its_posting_token_but_changed_reply_gets_a_new_one() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("database");
    insert_review_session(&db, "session-id").await;
    link_review_operation_test_request(&db).await;
    let context = review_operation_test_context(db.clone(), MockGitClient::new());
    let targets = vec!["thread-fixed".to_string()];
    let mut outcomes = vec![ReviewCommentOutcome {
        reply: "Applied the fix.".to_string(),
        resolution: ReviewCommentResolution::Fixed,
        thread_id: "thread-fixed".to_string(),
    }];
    let original = prepare_review_comment_resolutions(&context, &targets, &outcomes)
        .await
        .expect("outcome");
    db.reviews()
        .insert_session_review_comment_resolutions("session-id", &original)
        .await
        .expect("operation");
    db.reviews()
        .mark_session_review_comment_resolution_posting("session-id", &original[0].reply_token)
        .await
        .expect("posting");
    db.reviews()
        .bind_session_review_comment_resolutions_to_commit("session-id", &original, "old-tip")
        .await
        .expect("original tip");

    // Act
    db.reviews()
        .prepare_session_review_comment_turn("session-id", &targets)
        .await
        .expect("next turn");
    let repeated = prepare_review_comment_resolutions(&context, &targets, &outcomes)
        .await
        .expect("revalidated");
    db.reviews()
        .insert_session_review_comment_resolutions("session-id", &repeated)
        .await
        .expect("save repeated reply");
    db.reviews()
        .bind_session_review_comment_resolutions_to_commit("session-id", &repeated, "new-tip")
        .await
        .expect("new tip");
    let pending = db
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("pending");
    outcomes[0].reply = "The behavior now needs a different explanation.".to_string();
    outcomes[0].resolution = ReviewCommentResolution::NoChangeNeeded;
    db.reviews()
        .prepare_session_review_comment_turn("session-id", &targets)
        .await
        .expect("another turn");
    let changed = prepare_review_comment_resolutions(&context, &targets, &outcomes)
        .await
        .expect("changed outcome");
    db.reviews()
        .insert_session_review_comment_resolutions("session-id", &changed)
        .await
        .expect("save changed reply");
    let replacement = db
        .reviews()
        .load_session_review_comment_resolutions("session-id")
        .await
        .expect("replacement");

    // Assert
    assert_eq!(repeated[0].reply_token, original[0].reply_token);
    assert!(pending[0].is_posting);
    assert_eq!(pending[0].commit_hash.as_deref(), Some("new-tip"));
    assert_ne!(changed[0].reply_token, original[0].reply_token);
    assert!(!replacement[0].is_posting);
    assert_eq!(replacement[0].resolution, "no_change_needed");
}

#[tokio::test]
async fn stopped_review_does_not_wait_for_publish_lock() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("database");
    let context = review_operation_test_context(db, MockGitClient::new());
    let guard = Arc::clone(&context.branch_operation_lock)
        .lock_owned()
        .await;
    let metadata = TurnMetadata {
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        review_comment_thread_ids: vec!["thread-42".to_string()],
        session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
    };
    let mut publishing = Box::pin(start_published_branch_auto_push(&context, metadata, None));

    // Act
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(20), &mut publishing)
            .await
            .is_err()
    );
    context.cancel_token.cancel();
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), &mut publishing)
        .await
        .expect("stop must finish while lock remains held");

    // Assert
    assert!(matches!(result, Err(SessionError::StoppedByUser(_))));
    assert!(context.branch_operation_lock.try_lock().is_err());
    drop(guard);
}

/// Seeds an ordinary session for review-operation persistence tests.
async fn insert_review_session(db: &AppRepositories, session_id: &str) {
    let project_id = db
        .projects()
        .upsert_project("project", Some("main".to_string()))
        .await
        .expect("project");
    db.sessions()
        .insert_session(session_id, "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("session");
}
