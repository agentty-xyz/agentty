use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::Arc;

use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use app::review::{
    FocusedReviewPersistence, FocusedReviewPersistenceRetry, ReviewUpdate, apply_review_updates,
};

use super::super::state::App;
use super::{AppEvent, AppEventBatch, AppEventEffect, AppEventReductionPlan};
use crate::app;
use crate::app::session::TurnAppliedState;
use crate::app::sync::ProjectSyncContext;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId,
};
use crate::infra::db::DbError;
use crate::presentation::app_mode::{
    AppMode, DiffFocus, DiffLineComments, DiffPreview, DiffPreviewUnavailableReason,
    DiffReviewComments, DiffSidebarFocus, HelpContext, ReviewCommentSelection,
};
use crate::presentation::review_comment as review_comment_selection;

#[tokio::test]
async fn merged_branch_eligibility_rejects_incomplete_session_context() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let review_request = ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#23".to_string(),
            forge_kind: ForgeKind::GitHub,
            source_branch: "wt/child".to_string(),
            state: ReviewRequestState::Merged,
            status_summary: None,
            target_branch: "wt/parent".to_string(),
            title: "Merged child".to_string(),
            web_url: "https://example.test/pull/23".to_string(),
        },
    };
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session-without-review")
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("child-with-missing-parent")
            .parent_session_id(Some("missing-parent".into()))
            .review_request(Some(review_request.clone()))
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("parent-without-review")
            .build(),
    );
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("child-with-unlinked-parent")
            .parent_session_id(Some("parent-without-review".into()))
            .review_request(Some(review_request))
            .build(),
    );

    // Act
    let missing_session = app
        .merged_session_reached_synced_branch(&"missing-session".into(), "main")
        .await
        .expect("missing session eligibility should not fail");
    let session_without_review = app
        .merged_session_reached_synced_branch(&"session-without-review".into(), "main")
        .await
        .expect("unlinked session eligibility should not fail");
    let child_with_missing_parent = app
        .merged_session_reached_synced_branch(&"child-with-missing-parent".into(), "main")
        .await
        .expect("missing parent eligibility should not fail");
    let child_with_unlinked_parent = app
        .merged_session_reached_synced_branch(&"child-with-unlinked-parent".into(), "main")
        .await
        .expect("unlinked parent eligibility should not fail");

    // Assert
    assert!(!missing_session);
    assert!(!session_without_review);
    assert!(!child_with_missing_parent);
    assert!(!child_with_unlinked_parent);
}

#[tokio::test]
async fn test_session_review_comment_result_updates_matching_open_page() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_diff_mode("session-id", DiffReviewComments::loading(1));

    // Act
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Ok(ag_forge::ReviewCommentSnapshot::default()),
        session_id: "session-id".into(),
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                comment_error: None,
                comment_snapshot: Some(_),
                is_loading_comments: false,
                ..
            }),
            ..
        }
    ));
}

#[tokio::test]
async fn test_session_review_comment_refresh_retargets_selected_thread() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let previous_snapshot = review_comment_snapshot([
        review_comment_thread("selected", false),
        review_comment_thread("other", false),
    ]);
    let updated_snapshot = review_comment_snapshot([
        review_comment_thread("selected", true),
        review_comment_thread("other", false),
    ]);
    app.mode = review_comment_diff_mode(
        "session-id",
        DiffReviewComments {
            selected_comments: vec![
                ReviewCommentSelection {
                    thread_id: "selected".to_string(),
                },
                ReviewCommentSelection {
                    thread_id: "other".to_string(),
                },
            ],
            comment_error: None,
            comment_snapshot: Some(previous_snapshot),
            is_loading_comments: true,
            request_id: 1,
            selected_comment_index: 0,
            sidebar_focus: DiffSidebarFocus::Comments,
        },
    );

    // Act
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Ok(updated_snapshot),
        session_id: "session-id".into(),
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                ref selected_comments,
                comment_snapshot: Some(ref snapshot),
                selected_comment_index: 1,
                ..
            }),
            ..
        } if review_comment_selection::selected_thread_id(snapshot, 1) == Some("selected")
            && selected_comments == &[ReviewCommentSelection {
                thread_id: "other".to_string(),
            }]
    ));
}

#[tokio::test]
async fn test_session_review_comment_result_ignores_stale_request() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = review_comment_diff_mode("session-id", DiffReviewComments::loading(2));

    // Act
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Err("stale failure".to_string()),
        session_id: "session-id".into(),
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                comment_error: None,
                comment_snapshot: None,
                is_loading_comments: true,
                request_id: 2,
                ..
            }),
            ..
        }
    ));
}

#[tokio::test]
async fn test_session_review_comment_result_ignores_stale_pages_and_surfaces_errors() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;

    // Act
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Ok(ag_forge::ReviewCommentSnapshot::default()),
        session_id: "closed-session".into(),
    })
    .await;
    app.mode = review_comment_diff_mode("open-session", DiffReviewComments::loading(1));
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Ok(ag_forge::ReviewCommentSnapshot::default()),
        session_id: "stale-session".into(),
    })
    .await;
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Err("authentication failed".to_string()),
        session_id: "open-session".into(),
    })
    .await;

    // Assert
    assert!(app.is_viewing_session("open-session"));
    assert!(!app.is_viewing_session("stale-session"));
    assert!(matches!(
        app.mode,
        AppMode::Diff {
            review_comments: Some(DiffReviewComments {
                comment_error: Some(ref error),
                comment_snapshot: None,
                is_loading_comments: false,
                ..
            }),
            ..
        } if error == "Failed to load review comments: authentication failed"
    ));
}

#[tokio::test]
async fn diff_loading_mode_is_viewing_its_session() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::DiffLoading {
        fallback_view_scroll_offset: None,
        request_id: 1,
        restore: None,
        session_id: "loading-session".into(),
        sidebar_focus: DiffSidebarFocus::Files,
    };

    // Act
    let is_loading_session_visible = app.is_viewing_session("loading-session");
    let is_other_session_visible = app.is_viewing_session("other-session");

    // Assert
    assert!(is_loading_session_visible);
    assert!(!is_other_session_visible);
}

#[tokio::test]
async fn test_session_review_comment_result_updates_open_diff_help_overlay() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    app.mode = AppMode::Help {
        context: HelpContext::Diff {
            can_comment: true,
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: Some(Box::new(DiffReviewComments::loading(1))),
            restore: None,
            scroll_offset: 0,
            session_id: "session-id".into(),
        },
        scroll_offset: 0,
    };

    // Act
    app.apply_app_events(AppEvent::SessionReviewCommentSnapshotLoaded {
        request_id: 1,
        result: Ok(ReviewCommentSnapshot::default()),
        session_id: "session-id".into(),
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::Diff {
                review_comments: Some(ref review_comments),
                ..
            },
            ..
        } if matches!(
            **review_comments,
            DiffReviewComments {
                comment_snapshot: Some(_),
                is_loading_comments: false,
                ..
            }
        )
    ));
}

/// Builds a comment snapshot from inline thread fixtures.
fn review_comment_snapshot<const THREAD_COUNT: usize>(
    threads: [ReviewCommentThread; THREAD_COUNT],
) -> ReviewCommentSnapshot {
    ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: Vec::from(threads),
    }
}

/// Builds a diff workspace focused on one linked review-comment state.
fn review_comment_diff_mode(session_id: &str, mut review_comments: DiffReviewComments) -> AppMode {
    review_comments.sidebar_focus = DiffSidebarFocus::Comments;

    AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(review_comments),
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        session_id: session_id.into(),
    }
}

/// Builds one current or resolved review-comment thread.
fn review_comment_thread(id: &str, is_resolved: bool) -> ReviewCommentThread {
    ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::New,
        comments: vec![ReviewComment {
            author: "reviewer".to_string(),
            authored_by_current_user: false,
            body: "Review comment".to_string(),
        }],
        id: id.to_string(),
        is_outdated: Some(false),
        is_resolved,
        line: Some(1),
        path: "src/main.rs".to_string(),
        start_line: None,
    }
}

#[test]
fn test_refresh_sessions_batch_sets_only_session_reload_scope() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::RefreshSessions);

    // Assert
    assert!(event_batch.should_reload_sessions);
    assert!(!event_batch.should_reload_projects);
}

#[test]
fn test_refresh_projects_batch_sets_only_project_reload_scope() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    event_batch.collect_event(AppEvent::RefreshProjects);

    // Assert
    assert!(event_batch.should_reload_projects);
    assert!(!event_batch.should_reload_sessions);
}

#[test]
fn collect_runtime_event_rejects_top_level_event() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    let panic_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        event_batch.collect_runtime_event(AppEvent::RefreshSessions);
    }));

    // Assert
    assert!(panic_result.is_err());
}

#[test]
fn collect_workflow_event_rejects_runtime_event() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    let panic_result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        event_batch.collect_workflow_event(AppEvent::SyncMainConflictResolutionStarted {
            conflicted_files: Vec::new(),
            operation: ProjectSyncContext {
                default_branch: "main".to_string(),
                operation_id: 1,
                project_id: 1,
                project_name: "agentty".to_string(),
            },
        });
    }));

    // Assert
    assert!(panic_result.is_err());
}

#[test]
fn reduction_plan_orders_external_effects_without_running_them() {
    // Arrange
    let mut event_batch = AppEventBatch {
        should_refresh_git_status: true,
        should_reload_projects: true,
        should_reload_sessions: true,
        ..AppEventBatch::default()
    };

    // Act
    let reduction_plan = event_batch.drain_reduction_plan();

    // Assert
    assert_eq!(
        reduction_plan,
        AppEventReductionPlan {
            after_snapshot_effects: Vec::new(),
            before_snapshot_effects: vec![
                AppEventEffect::ReloadSessions,
                AppEventEffect::ReloadProjects,
                AppEventEffect::RefreshGitStatus,
            ],
            changes_observable_state: true,
        }
    );
}

#[test]
fn reduction_plan_keeps_an_empty_batch_pure_and_invisible() {
    // Arrange
    let mut event_batch = AppEventBatch::default();

    // Act
    let reduction_plan = event_batch.drain_reduction_plan();

    // Assert
    assert_eq!(
        reduction_plan,
        AppEventReductionPlan {
            after_snapshot_effects: Vec::new(),
            before_snapshot_effects: Vec::new(),
            changes_observable_state: false,
        }
    );
}

#[test]
fn reduction_plan_orders_review_persistence_after_snapshot_updates() {
    // Arrange
    let mut event_batch = AppEventBatch::default();
    event_batch.collect_event(AppEvent::ReviewPrepared {
        diff_hash: 42,
        review_text: "review output".to_string(),
        session_id: "session-1".into(),
    });

    // Act
    let reduction_plan = event_batch.drain_reduction_plan();

    // Assert
    assert_eq!(
        reduction_plan.after_snapshot_effects,
        vec![AppEventEffect::ApplyReviewUpdates(HashMap::from([(
            "session-1".into(),
            ReviewUpdate {
                diff_hash: 42,
                result: Ok("review output".to_string()),
            },
        )]))]
    );
    assert_eq!(
        reduction_plan.before_snapshot_effects,
        [] as [crate::app::core::event::AppEventEffect; 0]
    );
    assert!(reduction_plan.changes_observable_state);
}

/// Applies queued events until the expected session-diff request settles.
async fn apply_session_diff_request(app: &mut App, expected_request_id: u64) {
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while app
            .pending_session_diff_requests
            .contains_key(&expected_request_id)
        {
            let event = app
                .next_app_event()
                .await
                .expect("session diff should emit an event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("timed out waiting for session diff");
}

/// Queues an event that may share a reducer batch with a session-diff
/// result.
fn queue_unrelated_session_progress(app: &App) {
    app.services
        .event_sender()
        .send(AppEvent::SessionProgressUpdated {
            progress_message: Some("Unrelated progress".to_string()),
            session_id: "unrelated-session".into(),
        })
        .expect("unrelated event should queue before the diff result");
}

#[tokio::test]
async fn completed_turn_starts_auto_review_when_project_is_inactive() {
    // Arrange
    let diff_text = "diff --git a/file.rs b/file.rs\n+inactive change";
    let expected_hash = crate::app::test_support::diff_content_hash(diff_text);
    let mut git_client = ag_git::MockGitClient::new();
    git_client
        .expect_diff()
        .once()
        .returning(move |_, _| Box::pin(async move { Ok(diff_text.to_string()) }));
    let clients = crate::test_support::test_app_clients().with_git_client(Arc::new(git_client));
    let (mut app, base_dir) = crate::test_support::new_test_app_with_clients(clients).await;
    let session_id = SessionId::from("inactive-completed-session");
    let inactive_project_path = base_dir.path().join("inactive-project");
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&inactive_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert inactive project");
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id.as_str(),
            "gpt-5.6-sol",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("failed to insert inactive session");
    app.services
        .db()
        .settings()
        .upsert_project_settings(
            inactive_project_id,
            vec![
                (
                    crate::domain::setting::SettingName::DefaultReviewAgent,
                    "claude".to_string(),
                ),
                (
                    crate::domain::setting::SettingName::DefaultReviewModel,
                    "claude-sonnet-5".to_string(),
                ),
                (
                    crate::domain::setting::SettingName::DefaultReviewReasoningLevel,
                    "low".to_string(),
                ),
                (
                    crate::domain::setting::SettingName::DefaultReviewSpeedMode,
                    "fast".to_string(),
                ),
            ],
        )
        .await
        .expect("failed to persist inactive-project review settings");
    let turn_applied_state = TurnAppliedState {
        follow_up_tasks: Vec::new(),
        questions: Vec::new(),
        token_usage_delta: crate::domain::session::SessionStats::default(),
    };

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id: session_id.clone(),
        turn_applied_state,
    })
    .await;
    let expected_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("inactive-project diff should be pending");
    queue_unrelated_session_progress(&app);
    apply_session_diff_request(&mut app, expected_request_id).await;

    // Assert
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert!(app.pending_session_diff_requests.is_empty());
    assert!(matches!(
        app.review_cache.get(&session_id),
        Some(app::review::ReviewCacheEntry::Loading {
            diff_hash,
            review_agent,
        }) if *diff_hash == expected_hash
            && *review_agent == (
                crate::domain::agent::AgentSelection::new(
                    crate::domain::agent::AgentKind::Claude,
                    crate::domain::agent::AgentModel::ClaudeOpus5,
                ),
                crate::domain::agent::ReasoningLevel::Low,
                crate::domain::agent::SpeedMode::Fast,
            )
    ));
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_pending_focused_review_session_ids(inactive_project_id)
            .await
            .expect("failed to load deferred review"),
        [session_id.as_str()]
    );
}

#[tokio::test]
async fn late_completed_turn_for_deleted_session_is_not_deferred() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let session_id = SessionId::from("deleted-completed-session");
    let inactive_project_path = base_dir.path().join("inactive-project");
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&inactive_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert inactive project");
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id.as_str(),
            "gpt-5.6-sol",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("failed to insert inactive session");
    app.services
        .db()
        .sessions()
        .delete_session(session_id.as_str())
        .await
        .expect("failed to delete inactive session");
    let turn_applied_state = TurnAppliedState {
        follow_up_tasks: Vec::new(),
        questions: Vec::new(),
        token_usage_delta: crate::domain::session::SessionStats::default(),
    };

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id,
        turn_applied_state,
    })
    .await;

    // Assert
    assert!(app.deferred_auto_review_session_ids.is_empty());
    assert_eq!(
        app.services
            .db()
            .sessions()
            .load_pending_focused_review_session_ids(inactive_project_id)
            .await
            .expect("failed to load deferred reviews"),
        [] as [String; 0]
    );
}

#[tokio::test]
async fn completed_turn_with_questions_is_not_deferred() {
    // Arrange
    let mut app = crate::test_support::new_test_app_without_retained_base_dir().await;
    let session_id = SessionId::from("inactive-question-session");
    let turn_applied_state = TurnAppliedState {
        follow_up_tasks: Vec::new(),
        questions: vec![crate::domain::question::QuestionItem::new("Which project?")],
        token_usage_delta: crate::domain::session::SessionStats::default(),
    };

    // Act
    app.apply_app_events(AppEvent::AgentResponseReceived {
        session_id,
        turn_applied_state,
    })
    .await;

    // Assert
    assert!(app.deferred_auto_review_session_ids.is_empty());
}

#[tokio::test]
async fn completed_focused_review_persists_for_inactive_project() {
    // Arrange
    let (mut app, base_dir) = crate::test_support::new_test_app().await;
    let inactive_project_path = base_dir.path().join("inactive-project");
    let inactive_project_id = app
        .services
        .db()
        .projects()
        .upsert_project(&inactive_project_path.to_string_lossy(), None)
        .await
        .expect("failed to insert inactive project");
    let session_id = SessionId::from("inactive-review");
    let review_text = "## Review\nInactive project finding.";
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id.as_str(),
            "gpt-5.6-sol",
            "main",
            "Review",
            inactive_project_id,
        )
        .await
        .expect("failed to insert inactive review session");
    let review_agent = app.review_agent();
    app.review_cache.insert(
        session_id.clone(),
        app::review::ReviewCacheEntry::Loading {
            diff_hash: 42,
            review_agent,
        },
    );

    // Act
    app.apply_app_events(AppEvent::ReviewPrepared {
        diff_hash: 42,
        review_text: review_text.to_string(),
        session_id: session_id.clone(),
    })
    .await;
    let persisted_reviews = app
        .services
        .db()
        .sessions()
        .load_session_focused_reviews_for_project(inactive_project_id)
        .await
        .expect("failed to load inactive project review");

    // Assert
    assert_eq!(persisted_reviews.len(), 1);
    assert_eq!(persisted_reviews[0].session_id, session_id.as_str());
    assert_eq!(persisted_reviews[0].diff_hash, "42");
    assert_eq!(persisted_reviews[0].text, review_text);
    assert!(!app.review_cache.contains_key(&session_id));
    assert!(app.pending_focused_review_persistence.is_empty());
}

#[tokio::test]
async fn failed_focused_review_persistence_retries_without_replaying_stale_state() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let project_id = app.projects.active_project_id();
    app.services
        .db()
        .sessions()
        .insert_session("session-1", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert review session");
    app.review_cache.insert(
        "session-1".into(),
        app::review::ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "review output".to_string(),
        },
    );
    let persistence_update = FocusedReviewPersistence {
        diff_hash: Some(42),
        session_id: "session-1".into(),
        status: crate::domain::review::FocusedReviewStatus::Ready,
        text: Some("review output".to_string()),
    };
    app.pending_focused_review_persistence.insert(
        persistence_update.session_id.clone(),
        persistence_update.clone(),
    );
    let (retry_tx, mut retry_rx) = tokio::sync::mpsc::unbounded_channel();
    let retry_scheduled = App::handle_focused_review_persistence_result(
        retry_tx,
        FocusedReviewPersistenceRetry::initial(persistence_update.clone()),
        Err(DbError::Query(sqlx::Error::PoolClosed)),
    );

    // Act
    let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), retry_rx.recv())
        .await
        .expect("timed out waiting for focused-review persistence retry")
        .expect("focused-review persistence failure should requeue an event");
    app.apply_app_events(retry_event).await;
    let stale_pending = FocusedReviewPersistence {
        status: crate::domain::review::FocusedReviewStatus::Pending,
        text: None,
        ..persistence_update.clone()
    };
    app.apply_app_events(AppEvent::FocusedReviewPersistenceRetry {
        retry: FocusedReviewPersistenceRetry {
            attempt: 1,
            persistence_update: stale_pending,
        },
    })
    .await;
    let persisted = app
        .services
        .db()
        .sessions()
        .load_session_focused_reviews_for_project(project_id)
        .await
        .expect("failed to load retried focused review");
    let (exhausted_tx, mut exhausted_rx) = tokio::sync::mpsc::unbounded_channel();
    let exhausted_retry_scheduled = App::handle_focused_review_persistence_result(
        exhausted_tx,
        FocusedReviewPersistenceRetry {
            attempt: app::review::MAX_FOCUSED_REVIEW_PERSISTENCE_RETRIES,
            persistence_update,
        },
        Err(DbError::Query(sqlx::Error::PoolClosed)),
    );
    let exhausted_event = exhausted_rx.recv().await;

    // Assert
    assert_eq!(persisted.len(), 1);
    assert_eq!(persisted[0].session_id, "session-1");
    assert_eq!(persisted[0].diff_hash, "42");
    assert_eq!(persisted[0].text, "review output");
    assert!(retry_scheduled);
    assert!(!exhausted_retry_scheduled);
    assert_eq!(exhausted_event, None);
}

#[tokio::test]
async fn test_diff_preview_events_map_all_worktree_results() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let outcomes = [
        Ok(ag_git::WorktreeFileContent::Text("# Preview".to_string())),
        Ok(ag_git::WorktreeFileContent::Missing),
        Ok(ag_git::WorktreeFileContent::Binary),
        Ok(ag_git::WorktreeFileContent::TooLarge),
        Err("read failed".to_string()),
    ];
    let resolve_diff_state = |mode: &AppMode| match mode {
        AppMode::Diff {
            preview,
            scroll_cache,
            ..
        } => Some((preview.clone(), scroll_cache.is_none())),
        _ => None,
    };

    // Act
    let mut resolved_previews = Vec::new();
    for (request_id, result) in (1_u64..).zip(outcomes) {
        app.mode = AppMode::Diff {
            diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::Loading {
                path: "README.md".to_string(),
                request_id,
            },
            review_comments: None,
            restore: None,
            scroll_cache: Some(crate::presentation::app_mode::DiffScrollCache {
                content_area: crate::presentation::app_mode::ViewportRect {
                    height: 24,
                    width: 80,
                    x: 0,
                    y: 0,
                },
                file_explorer_selected_index: 0,
                max_scroll_offset: 4,
            }),
            scroll_offset: 2,
            session_id: "session-id".into(),
        };
        app.apply_app_events(AppEvent::DiffPreviewLoaded {
            path: "README.md".to_string(),
            request_id,
            result,
            session_id: "session-id".into(),
        })
        .await;
        let (preview, scroll_cache_cleared) =
            resolve_diff_state(&app.mode).expect("diff preview result should preserve diff mode");
        assert!(scroll_cache_cleared);
        resolved_previews.push(preview);
    }

    // Assert
    assert!(resolve_diff_state(&AppMode::List).is_none());
    assert_eq!(resolved_previews.len(), 5);
    assert!(matches!(
        &resolved_previews[0],
        DiffPreview::Ready { content, .. } if content == "# Preview"
    ));
    assert!(matches!(
        &resolved_previews[1],
        DiffPreview::Unavailable {
            reason: DiffPreviewUnavailableReason::Deleted,
            ..
        }
    ));
    assert!(matches!(
        &resolved_previews[2],
        DiffPreview::Unavailable {
            reason: DiffPreviewUnavailableReason::Binary,
            ..
        }
    ));
    assert!(matches!(
        &resolved_previews[3],
        DiffPreview::Unavailable {
            reason: DiffPreviewUnavailableReason::TooLarge,
            ..
        }
    ));
    assert!(matches!(
        &resolved_previews[4],
        DiffPreview::Unavailable {
            reason: DiffPreviewUnavailableReason::LoadFailed(error),
            ..
        } if error == "read failed"
    ));
}

#[tokio::test]
async fn test_diff_preview_event_ignores_stale_mode_session_path_and_request() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    let loading = || DiffPreview::Loading {
        path: "README.md".to_string(),
        request_id: 4,
    };
    let event = |path: &str, request_id: u64, session_id: &str| AppEvent::DiffPreviewLoaded {
        path: path.to_string(),
        request_id,
        result: Ok(ag_git::WorktreeFileContent::Text("stale".to_string())),
        session_id: session_id.into(),
    };
    let diff_mode = |preview| AppMode::Diff {
        diff: "diff".to_string(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        preview,
        review_comments: None,
        restore: None,
        scroll_cache: None,
        scroll_offset: 0,
        selected_diff_line_index: 0,
        session_id: "session-id".into(),
    };

    // Act
    app.mode = diff_mode(loading());
    app.apply_app_events(event("OTHER.md", 4, "session-id"))
        .await;
    let stale_path_ignored = matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Loading { .. },
            ..
        }
    );
    app.mode = diff_mode(loading());
    app.apply_app_events(event("README.md", 5, "session-id"))
        .await;
    let stale_request_ignored = matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Loading { .. },
            ..
        }
    );
    app.mode = diff_mode(loading());
    app.apply_app_events(event("README.md", 4, "other-session"))
        .await;
    let stale_session_ignored = matches!(
        app.mode,
        AppMode::Diff {
            preview: DiffPreview::Loading { .. },
            ..
        }
    );
    app.mode = AppMode::List;
    app.apply_app_events(event("README.md", 4, "session-id"))
        .await;

    // Assert
    assert!(stale_path_ignored);
    assert!(stale_request_ignored);
    assert!(stale_session_ignored);
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn test_diff_preview_event_resolves_while_help_is_open() {
    // Arrange
    let (mut app, _base_dir) = crate::test_support::new_test_app().await;
    app.mode = AppMode::Help {
        context: HelpContext::Diff {
            can_comment: true,
            diff: "diff --git a/README.md b/README.md\n+preview".to_string(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::Loading {
                path: "README.md".to_string(),
                request_id: 8,
            },
            review_comments: None,
            restore: None,
            scroll_offset: 0,
            session_id: "session-id".into(),
        },
        scroll_offset: 0,
    };

    // Act
    app.apply_app_events(AppEvent::DiffPreviewLoaded {
        path: "README.md".to_string(),
        request_id: 8,
        result: Ok(ag_git::WorktreeFileContent::Text("# Ready".to_string())),
        session_id: "session-id".into(),
    })
    .await;

    // Assert
    assert!(matches!(
        app.mode,
        AppMode::Help {
            context: HelpContext::Diff {
                preview: DiffPreview::Ready { ref content, .. },
                ..
            },
            ..
        } if content == "# Ready"
    ));
}

impl App {
    /// Waits for the next internal app event.
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    /// Applies one review assist update to cache and focused render state.
    pub(in crate::app::core) fn apply_review_update(
        &mut self,
        session_id: &str,
        review_update: app::review::ReviewUpdate,
    ) {
        let mut review_updates = HashMap::new();
        review_updates.insert(SessionId::from(session_id), review_update);
        apply_review_updates(
            &mut self.review_cache,
            self.sessions.state_mut(),
            review_updates,
        );
    }
}

#[path = "event_boundary_test.rs"]
mod boundary;
