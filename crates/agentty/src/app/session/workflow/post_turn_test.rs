use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent::MockOneShotClient;
use ag_forge as forge;
use ag_git::{GitError, MockGitClient};
use ag_protocol::{ReviewCommentOutcome, ReviewCommentResolution};
use tokio::sync::mpsc;
use tracing::instrument::WithSubscriber;

use super::super::worker::TurnMetadata;
use super::{
    PostTurnContext, TURN_ERROR_NOTICE_MAX_CHARS, TurnFinalizerContext, archive_research_diff,
    finalize_channel_turn, prepare_review_comment_resolutions, run_auto_commit,
    start_published_branch_auto_push, truncate_turn_error_notice, validate_review_comment_outcomes,
};
use crate::app::session::SessionError;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::session::Status;
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::{AppRepositories, PersistedSessionCreation};

async fn insert_research_session(database: &AppRepositories, session_id: &str) {
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session_with_agent(PersistedSessionCreation {
            agent: "codex",
            base_branch: "main",
            id: session_id,
            is_draft: false,
            model: AgentKind::Codex.default_model().as_str(),
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

fn research_finalizer_context(
    db: AppRepositories,
    folder: PathBuf,
    fs_client: crate::infra::fs::MockFsClient,
    git_client: MockGitClient,
    session_id: &str,
) -> (TurnFinalizerContext, Arc<Mutex<Status>>) {
    let status = Arc::new(Mutex::new(Status::InProgress));
    let context = TurnFinalizerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder,
        fs_client: Arc::new(fs_client),
        git_client: Arc::new(git_client),
        session_update_versions: Arc::default(),
        session_id: session_id.into(),
        status: Arc::clone(&status),
    };

    (context, status)
}

/// Builds the narrow post-turn context used by review-operation tests.
fn review_operation_test_context(
    db: AppRepositories,
    git_client: MockGitClient,
) -> PostTurnContext {
    PostTurnContext {
        app_event_tx: mpsc::unbounded_channel().0,
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: PathBuf::from("/tmp/project"),
        git_client: Arc::new(git_client),
        one_shot_client: Arc::new(MockOneShotClient::new()),
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
    insert_research_session(&db, "session-id").await;
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
    insert_research_session(&db, "session-id").await;
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
    insert_research_session(&db, "session-id").await;
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
    insert_research_session(&db, "session-id").await;
    let mut git_client = MockGitClient::new();
    git_client
        .expect_is_worktree_clean()
        .once()
        .returning(|_| Box::pin(async { Err(GitError::OutputParse("commit failed".to_string())) }));
    let mut context = review_operation_test_context(db, git_client);
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .once()
        .returning(|_| Err(ag_agent::OneShotError::new("commit failed")));
    context.one_shot_client = Arc::new(one_shot_client);
    let session_agent = AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol);

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
        branch_operation_lock: Arc::new(tokio::sync::Mutex::new(())),
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: PathBuf::new(),
        git_client: Arc::new(MockGitClient::new()),
        one_shot_client: Arc::new(MockOneShotClient::new()),
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
async fn finalization_tolerates_managed_child_evidence_persistence_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let status = Arc::new(Mutex::new(Status::InProgress));
    let context = TurnFinalizerContext {
        app_event_tx: mpsc::unbounded_channel().0,
        clock: Arc::new(crate::infra::clock::RealClock),
        db,
        folder: PathBuf::new(),
        fs_client: Arc::new(crate::infra::fs::MockFsClient::new()),
        git_client: Arc::new(MockGitClient::new()),
        session_update_versions: Arc::default(),
        session_id: "session-id".into(),
        status: Arc::clone(&status),
    };
    let result = Err(SessionError::StoppedByUser("stopped".to_string()));

    // Act
    finalize_channel_turn(&context, &result).await;

    // Assert
    assert_eq!(
        *status.lock().expect("status lock should remain usable"),
        Status::InProgress
    );
}

#[tokio::test]
async fn finalization_archives_research_diff_before_status_transition() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_research_session(&db, "research-session").await;
    let mut fs_client = crate::infra::fs::MockFsClient::new();
    fs_client.expect_is_dir().times(1).return_const(false);
    let mut git_client = MockGitClient::new();
    git_client.expect_diff().times(1).returning(|folder, base| {
        assert_eq!(folder, PathBuf::from("/tmp/research-session"));
        assert_eq!(base, "main");

        Box::pin(async {
            Ok("diff --git a/violation.txt b/violation.txt\n+unexpected\n".to_string())
        })
    });
    let (context, status) = research_finalizer_context(
        db.clone(),
        PathBuf::from("/tmp/research-session"),
        fs_client,
        git_client,
        "research-session",
    );

    // Act
    finalize_channel_turn(&context, &Ok(Status::Review)).await;
    let archived_diff = db
        .sessions()
        .load_session_archived_diff("research-session")
        .await
        .expect("failed to load archived diff");

    // Assert
    assert_eq!(
        archived_diff.as_deref(),
        Some("diff --git a/violation.txt b/violation.txt\n+unexpected\n")
    );
    assert_eq!(
        *status.lock().expect("status lock should remain usable"),
        Status::Review
    );
}

#[tokio::test]
async fn research_diff_archival_returns_when_base_branch_is_missing() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    let (context, _status) = research_finalizer_context(
        db.clone(),
        PathBuf::from("/tmp/missing-research"),
        crate::infra::fs::MockFsClient::new(),
        MockGitClient::new(),
        "missing-research",
    );

    // Act
    archive_research_diff(&context).await;
    let archived_diff = db
        .sessions()
        .load_session_archived_diff("missing-research")
        .await
        .expect("archive lookup should succeed");

    // Assert
    assert_eq!(archived_diff, None);
}

#[tokio::test]
async fn research_diff_archival_tolerates_base_branch_lookup_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    pool.close().await;
    let (context, _status) = research_finalizer_context(
        db,
        PathBuf::from("/tmp/research-session"),
        crate::infra::fs::MockFsClient::new(),
        MockGitClient::new(),
        "research-session",
    );

    // Act
    archive_research_diff(&context)
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert!(pool.is_closed());
}

#[tokio::test]
async fn research_diff_archival_tolerates_git_failure() {
    // Arrange
    let db = AppRepositories::in_memory().await.expect("db should open");
    insert_research_session(&db, "research-session").await;
    let mut git_client = MockGitClient::new();
    git_client.expect_diff().times(1).returning(|_, _| {
        Box::pin(async { Err(GitError::OutputParse("diff failed".to_string())) })
    });
    let (context, _status) = research_finalizer_context(
        db.clone(),
        PathBuf::from("/tmp/research-session"),
        crate::infra::fs::MockFsClient::new(),
        git_client,
        "research-session",
    );

    // Act
    archive_research_diff(&context)
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;
    let archived_diff = db
        .sessions()
        .load_session_archived_diff("research-session")
        .await
        .expect("archive lookup should succeed");

    // Assert
    assert_eq!(archived_diff, None);
}

#[tokio::test]
async fn research_diff_archival_tolerates_persistence_failure() {
    // Arrange
    let (db, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    insert_research_session(&db, "research-session").await;
    let mut git_client = MockGitClient::new();
    git_client.expect_diff().times(1).returning({
        let pool = pool.clone();

        move |_, _| {
            let pool = pool.clone();

            Box::pin(async move {
                pool.close().await;

                Ok("diff --git a/policy.txt b/policy.txt\n+write\n".to_string())
            })
        }
    });
    let (context, _status) = research_finalizer_context(
        db,
        PathBuf::from("/tmp/research-session"),
        crate::infra::fs::MockFsClient::new(),
        git_client,
        "research-session",
    );

    // Act
    archive_research_diff(&context)
        .with_subscriber(crate::test_support::TestSubscriber)
        .await;

    // Assert
    assert!(pool.is_closed());
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
        branch_operation_lock,
        child_pid: Arc::new(Mutex::new(None)),
        clock: Arc::new(crate::infra::clock::RealClock),
        db: db.clone(),
        folder: PathBuf::new(),
        git_client: Arc::new(mock_git_client),
        one_shot_client: Arc::new(MockOneShotClient::new()),
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
            .await;
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
