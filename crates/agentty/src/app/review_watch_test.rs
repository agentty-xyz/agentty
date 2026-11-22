//! Review-request watch tests.

use std::sync::Arc;
use std::time::Duration;

use ag_forge::{
    ForgeRemote, MockReviewRequestClient, ReviewCheckFailure, ReviewComment,
    ReviewCommentAnchorSide, ReviewCommentThread, ReviewRequestAttention, ReviewRequestError,
};

use super::{
    ReviewWatchAction, ReviewWatchState, build_address_review_prompt,
    build_fix_failed_checks_prompt, retain_current_keys,
};
use crate::app::branch_publish::{BranchPublishTaskFailure, BranchPublishTaskSuccess};
use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::session::SyncReviewRequestOutcome;
use crate::app::sync::{ProjectSyncContext, ProjectSyncPhase, ProjectSyncStatus};
use crate::app::{App, AppEvent};
use crate::domain::session::{
    ForgeKind, PublishBranchAction, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    SessionId, Status,
};
use crate::domain::session_message::{SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::{
    GeneratedTurnUpdate, TransientMessageBody, TransientMessageSlot,
};
use crate::domain::turn_prompt::TurnPromptTextSource;
use crate::infra::db::SessionReviewWatchRow;
use crate::presentation::app_mode::{AppMode, ConfirmationIntent, ConfirmationViewMode};
use crate::test_support::{
    new_git_test_app, new_git_test_app_with_pool, set_session_status_for_test,
};

/// Builds one linked review request in `state`.
fn review_request(forge_kind: ForgeKind, state: ReviewRequestState) -> ReviewRequest {
    ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind,
            source_branch: "wt/session".to_string(),
            state,
            status_summary: None,
            target_branch: "main".to_string(),
            title: "Add watch".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}

/// Builds one unresolved reviewer thread with `comment_count` comments.
fn review_thread(id: &str, comment_count: usize) -> ReviewCommentThread {
    ReviewCommentThread {
        anchor_side: ReviewCommentAnchorSide::New,
        comments: vec![
            ReviewComment {
                author: "maintainer".to_string(),
                authored_by_current_user: false,
                body: "Handle the empty case.".to_string(),
            };
            comment_count
        ],
        id: id.to_string(),
        is_outdated: Some(false),
        is_resolved: false,
        line: Some(12),
        path: "src/lib.rs".to_string(),
        start_line: None,
    }
}

/// Builds one failed check with an optional details URL that the forge
/// cannot rerun.
fn failed_check(name: &str, web_url: Option<&str>) -> ReviewCheckFailure {
    ReviewCheckFailure {
        name: name.to_string(),
        outcome: "Failure".to_string(),
        rerun_id: None,
        web_url: web_url.map(str::to_string),
    }
}

/// Builds one failed check the forge can rerun through workflow run
/// `rerun_id`.
fn rerunnable_check(name: &str, rerun_id: &str) -> ReviewCheckFailure {
    ReviewCheckFailure {
        rerun_id: Some(rerun_id.to_string()),
        ..failed_check(name, None)
    }
}

/// Builds forge facts with conflicts, one thread, and one failed check.
fn busy_attention() -> ReviewRequestAttention {
    ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        has_merge_conflict: true,
        has_pending_checks: false,
        head_commit: "abc123".to_string(),
        review_threads: vec![review_thread("thread-1", 1)],
    }
}

/// Creates a session with a linked review request in `status`.
async fn seed_review_session(
    app: &mut App,
    status: Status,
    review_request_state: ReviewRequestState,
) -> SessionId {
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.sessions.sessions_mut()[0].prompt = "Existing prompt".to_string();
    set_session_status_for_test(app, &session_id, status);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, review_request_state),
    );

    session_id
}

/// Returns the commit at the head of one session worktree, which follow-ups
/// on already attempted checks require the forge to report.
async fn session_head(app: &App, session_id: &str) -> String {
    let folder = app
        .sessions
        .session_for_id(session_id)
        .expect("session")
        .folder
        .clone();

    app.services
        .git_client()
        .head_hash(folder)
        .await
        .expect("session head")
}

/// Returns the rendered live transcript of one session.
fn transcript(app: &App, session_id: &str) -> String {
    let transcript = app
        .sessions
        .session_handles_or_err(session_id)
        .expect("handles")
        .transcript
        .lock()
        .expect("transcript lock");

    SessionTranscript::display_text_for_messages(transcript.messages())
}

/// Returns the status held by one session's live handle.
fn handle_status(app: &App, session_id: &str) -> Status {
    *app.sessions
        .session_handles_or_err(session_id)
        .expect("handles")
        .status
        .lock()
        .expect("status lock")
}

#[test]
fn plan_prefers_conflicts_then_threads_with_completed_failed_checks() {
    // Arrange
    let attention = busy_attention();
    let conflict_handled = vec!["conflict:abc123".to_string()];
    let comments_handled = vec![
        "conflict:abc123".to_string(),
        "thread:thread-1:1".to_string(),
    ];
    let checks_handled = vec![
        "conflict:abc123".to_string(),
        "thread:thread-1:1".to_string(),
        "checks:abc123".to_string(),
    ];
    let everything_handled = [
        checks_handled.clone(),
        vec!["checks-left:abc123".to_string()],
    ]
    .concat();

    // Act
    let conflict = ReviewWatchAction::plan(&attention, &[]);
    let comments = ReviewWatchAction::plan(&attention, &conflict_handled);
    let checks = ReviewWatchAction::plan(&attention, &comments_handled);
    let left = ReviewWatchAction::plan(&attention, &checks_handled);
    let nothing = ReviewWatchAction::plan(&attention, &everything_handled);

    // Assert
    assert_eq!(
        conflict,
        Some(ReviewWatchAction::ResolveConflicts {
            head_commit: "abc123".to_string(),
        })
    );
    assert_eq!(
        comments,
        Some(ReviewWatchAction::AddressReview {
            failed_checks: vec![failed_check("test", None)],
            head_commit: "abc123".to_string(),
            threads: vec![review_thread("thread-1", 1)],
        })
    );
    assert_eq!(
        checks,
        Some(ReviewWatchAction::FixChecks {
            failed_checks: vec![failed_check("test", None)],
            head_commit: "abc123".to_string(),
        })
    );
    assert_eq!(
        left,
        Some(ReviewWatchAction::LeaveChecks {
            failed_checks: vec![failed_check("test", None)],
            head_commit: "abc123".to_string(),
        })
    );
    assert_eq!(nothing, None);
}

#[test]
fn plan_reruns_rerunnable_checks_once_before_leaving_unfixed_checks_to_the_user() {
    // Arrange
    let attention = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101"), failed_check("sonar", None)],
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    };
    let pending = ReviewRequestAttention {
        has_pending_checks: true,
        ..attention.clone()
    };
    let passing = ReviewRequestAttention {
        failed_checks: Vec::new(),
        ..attention.clone()
    };
    let fix_attempted = vec!["checks:abc123".to_string()];
    let rerun_requested = vec!["checks:abc123".to_string(), "rerun:abc123".to_string()];
    let rerun_started = [
        rerun_requested.clone(),
        vec!["rerun-started:abc123".to_string()],
    ]
    .concat();

    // Act
    let rerun = ReviewWatchAction::plan(&attention, &fix_attempted);
    let rerun_not_yet_started = ReviewWatchAction::plan(&attention, &rerun_requested);
    let rerun_pending = ReviewWatchAction::plan(&pending, &rerun_started);
    let rerun_passed = ReviewWatchAction::plan(&passing, &rerun_started);
    let left = ReviewWatchAction::plan(&attention, &rerun_started);

    // Assert
    assert_eq!(
        rerun,
        Some(ReviewWatchAction::RerunChecks {
            failed_checks: vec![rerunnable_check("test", "101")],
            head_commit: "abc123".to_string(),
        })
    );
    assert_eq!(rerun_not_yet_started, None);
    assert_eq!(rerun_pending, None);
    assert_eq!(rerun_passed, None);
    assert_eq!(
        left,
        Some(ReviewWatchAction::LeaveChecks {
            failed_checks: vec![rerunnable_check("test", "101"), failed_check("sonar", None)],
            head_commit: "abc123".to_string(),
        })
    );
}

#[test]
fn plan_waits_for_pending_checks_and_retries_after_forge_state_changes() {
    // Arrange
    let handled_keys = vec!["checks:abc123".to_string(), "thread:thread-1:1".to_string()];
    let pending = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        has_pending_checks: true,
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    };
    let new_head = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        head_commit: "def456".to_string(),
        ..ReviewRequestAttention::default()
    };
    let follow_up = ReviewRequestAttention {
        head_commit: "abc123".to_string(),
        review_threads: vec![review_thread("thread-1", 2)],
        ..ReviewRequestAttention::default()
    };

    // Act
    let pending_plan = ReviewWatchAction::plan(&pending, &[]);
    let new_head_plan = ReviewWatchAction::plan(&new_head, &handled_keys);
    let follow_up_plan = ReviewWatchAction::plan(&follow_up, &handled_keys);

    // Assert
    assert_eq!(pending_plan, None);
    assert_eq!(
        new_head_plan,
        Some(ReviewWatchAction::FixChecks {
            failed_checks: vec![failed_check("test", None)],
            head_commit: "def456".to_string(),
        })
    );
    assert_eq!(
        follow_up_plan,
        Some(ReviewWatchAction::AddressReview {
            failed_checks: Vec::new(),
            head_commit: "abc123".to_string(),
            threads: vec![review_thread("thread-1", 2)],
        })
    );
}

#[test]
fn plan_addresses_threads_without_failed_checks_that_are_pending_or_already_attempted() {
    // Arrange
    let pending = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        has_pending_checks: true,
        head_commit: "abc123".to_string(),
        review_threads: vec![review_thread("thread-1", 1)],
        ..ReviewRequestAttention::default()
    };
    let attempted = ReviewRequestAttention {
        has_pending_checks: false,
        ..pending.clone()
    };
    let checks_handled = vec!["checks:abc123".to_string()];
    let expected = Some(ReviewWatchAction::AddressReview {
        failed_checks: Vec::new(),
        head_commit: "abc123".to_string(),
        threads: vec![review_thread("thread-1", 1)],
    });

    // Act
    let pending_plan = ReviewWatchAction::plan(&pending, &[]);
    let attempted_plan = ReviewWatchAction::plan(&attempted, &checks_handled);

    // Assert
    assert_eq!(pending_plan, expected);
    assert_eq!(attempted_plan, expected);
}

#[test]
fn actions_describe_their_follow_up_and_handled_keys() {
    // Arrange
    let linked = review_request(ForgeKind::GitHub, ReviewRequestState::Open);
    let cases = [
        (
            ReviewWatchAction::ResolveConflicts {
                head_commit: "abc123".to_string(),
            },
            "Resolving merge conflicts with `main`",
            vec!["conflict:abc123".to_string()],
        ),
        (
            ReviewWatchAction::AddressReview {
                failed_checks: Vec::new(),
                head_commit: "abc123".to_string(),
                threads: vec![review_thread("thread-1", 1), review_thread("thread-2", 3)],
            },
            "Addressing 2 review comments",
            vec![
                "thread:thread-1:1".to_string(),
                "thread:thread-2:3".to_string(),
            ],
        ),
        (
            ReviewWatchAction::AddressReview {
                failed_checks: vec![failed_check("test", None)],
                head_commit: "abc123".to_string(),
                threads: vec![review_thread("thread-1", 1)],
            },
            "Addressing 1 review comment and 1 failing check: test",
            vec!["thread:thread-1:1".to_string(), "checks:abc123".to_string()],
        ),
        (
            ReviewWatchAction::FixChecks {
                failed_checks: vec![failed_check("test", None)],
                head_commit: "abc123".to_string(),
            },
            "Fixing 1 failing check: test",
            vec!["checks:abc123".to_string()],
        ),
        (
            ReviewWatchAction::RerunChecks {
                failed_checks: vec![rerunnable_check("test", "101")],
                head_commit: "abc123".to_string(),
            },
            "Rerunning 1 failing check: test",
            vec!["rerun:abc123".to_string()],
        ),
        (
            ReviewWatchAction::LeaveChecks {
                failed_checks: vec![failed_check("test", None), failed_check("lint", None)],
                head_commit: "abc123".to_string(),
            },
            "Leaving 2 failing checks to you after automatic attempts: test, lint",
            vec!["checks-left:abc123".to_string()],
        ),
    ];

    for (action, expected_notice, expected_keys) in cases {
        // Act
        let notice = action.notice(&linked);
        let keys = action.handled_keys();

        // Assert
        assert_eq!(notice, expected_notice);
        assert_eq!(keys, expected_keys);
    }
}

#[test]
fn failed_checks_prompt_fences_forge_data_and_names_the_forge_cli() {
    // Arrange
    let linked = review_request(ForgeKind::GitLab, ReviewRequestState::Open);
    let failed_checks = [
        failed_check("lint", Some("https://ci.example/lint")),
        failed_check("```test```", None),
    ];

    // Act
    let prompt = build_fix_failed_checks_prompt(&linked, "abc123", &failed_checks);

    // Assert
    assert_eq!(prompt.text_source, TurnPromptTextSource::AgentData);
    assert!(
        prompt
            .text
            .starts_with("The merge request for this session has failed checks")
    );
    assert!(prompt.text.contains("with the `glab` CLI"));
    assert!(
        prompt
            .text
            .contains("Do not create commits and do not push.")
    );
    assert!(
        prompt
            .text
            .contains("Do not rerun checks: Agentty reruns the failures")
    );
    assert!(
        prompt.text.ends_with(
            "````text\nReview request: https://github.com/agentty-xyz/agentty/pull/42\nCommit: \
             abc123\n\nCheck: lint\nOutcome: Failure\nDetails: https://ci.example/lint\n\nCheck: \
             ```test```\nOutcome: Failure\n````"
        ),
        "{}",
        prompt.text
    );
    assert!(!prompt.text.contains("{{"), "{}", prompt.text);
}

#[test]
fn address_review_prompt_appends_failed_checks_after_the_review_comments() {
    // Arrange
    let linked = review_request(ForgeKind::GitHub, ReviewRequestState::Open);
    let threads = vec![review_thread("thread-1", 1)];
    let failed_checks = [failed_check("lint", Some("https://ci.example/lint"))];

    // Act
    let (comments_only, comment_thread_ids) =
        build_address_review_prompt(&linked, "abc123", &[], threads.clone())
            .expect("actionable thread");
    let (combined, combined_thread_ids) =
        build_address_review_prompt(&linked, "abc123", &failed_checks, threads)
            .expect("actionable thread");

    // Assert
    assert_eq!(comment_thread_ids, vec!["thread-1".to_string()]);
    assert_eq!(combined_thread_ids, comment_thread_ids);
    assert_eq!(combined.text_source, TurnPromptTextSource::AgentData);
    assert!(comments_only.text.contains("Thread ID: thread-1"));
    assert!(!comments_only.text.contains("failed checks"));
    assert!(combined.text.starts_with(&comments_only.text));
    assert!(
        combined
            .text
            .contains("The pull request also has failed checks")
    );
    assert!(combined.text.contains("with the `gh` CLI"));
    assert!(
        combined
            .text
            .contains("return one `review_comment_outcomes` item for every thread ID")
    );
    assert!(
        combined.text.ends_with(
            "```text\nReview request: https://github.com/agentty-xyz/agentty/pull/42\nCommit: \
             abc123\n\nCheck: lint\nOutcome: Failure\nDetails: https://ci.example/lint\n```"
        ),
        "{}",
        combined.text
    );
    assert!(!combined.text.contains("{{"), "{}", combined.text);
}

#[test]
fn address_review_prompt_requires_a_thread_that_needs_a_response() {
    // Arrange
    let linked = review_request(ForgeKind::GitHub, ReviewRequestState::Open);
    let mut resolved_thread = review_thread("thread-1", 1);
    resolved_thread.is_resolved = true;

    // Act
    let prompt = build_address_review_prompt(&linked, "abc123", &[], vec![resolved_thread]);

    // Assert
    assert_eq!(prompt, None);
}

#[test]
fn watch_state_restores_watched_sessions_from_rows() {
    // Arrange
    let rows = vec![SessionReviewWatchRow {
        handled_keys: vec!["checks:abc123".to_string()],
        session_id: "session-1".to_string(),
    }];

    // Act
    let state = ReviewWatchState::from_rows(rows);

    // Assert
    assert!(state.is_watched("session-1"));
    assert!(!state.is_watched("session-2"));
    assert_eq!(
        state.handled_keys.get("session-1"),
        Some(&vec!["checks:abc123".to_string()])
    );
}

#[tokio::test]
async fn watch_state_loads_persisted_rows_and_tolerates_storage_failure() {
    // Arrange
    let (mut app, _base_dir, pool) = new_git_test_app_with_pool().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;

    // Act
    let loaded = ReviewWatchState::load(app.services.db()).await;
    sqlx::query("DROP TABLE session_review_watch")
        .execute(&pool)
        .await
        .expect("drop watch table");
    let failed = ReviewWatchState::load(app.services.db()).await;

    // Assert
    assert!(loaded.is_watched(&session_id));
    assert!(!failed.is_watched(&session_id));
}

/// Builds the session view restored by publication for `session_id`.
fn publish_restore_view(session_id: &str) -> ConfirmationViewMode {
    ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: SessionId::from(session_id),
    }
}

/// Returns whether publication was attempted for a session that no longer
/// exists, which reports the missing session instead of pushing.
fn reported_missing_session_publish(app: &App) -> bool {
    matches!(
        &app.mode,
        AppMode::ViewInfoPopup { message, title, .. }
            if title == "Branch push failed" && message == "Session is no longer available."
    )
}

#[tokio::test]
async fn publishing_an_unpushed_branch_asks_to_watch_before_pushing() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let restore_view = publish_restore_view("missing-session");

    // Act
    app.submit_review_request_publish(
        restore_view.clone(),
        Some("review/topic".to_string()),
        false,
    )
    .await;

    // Assert
    assert!(matches!(
        &app.mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::WatchPublishedReviewRequest,
            confirmation_message,
            confirmation_title,
            restore_view: Some(confirmation_restore_view),
            session_id: Some(confirmation_session_id),
            selected_confirmation_index: 0,
        } if confirmation_title == "Watch Review Request"
            && confirmation_message == "Auto-resolve checks, comments, conflicts?"
            && *confirmation_restore_view == restore_view
            && confirmation_session_id == "missing-session"
    ));
    let pending_publish = app
        .review_watch
        .pending_publish
        .as_ref()
        .expect("held publication");
    assert_eq!(
        pending_publish.remote_branch_name.as_deref(),
        Some("review/topic")
    );
    assert_eq!(pending_publish.restore_view, restore_view);
}

#[tokio::test]
async fn answering_the_watch_question_publishes_without_keeping_rejected_watches() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = SessionId::from("missing-session");

    // Act
    app.submit_review_request_publish(publish_restore_view(&session_id), None, false)
        .await;
    app.answer_review_watch_before_publish(true).await;
    let accepted_publishes = reported_missing_session_publish(&app);
    let rejected_keeps_watch = !app.review_watch.watch_on_publish.is_empty();
    app.submit_review_request_publish(publish_restore_view(&session_id), None, false)
        .await;
    app.answer_review_watch_before_publish(false).await;
    let declined_publishes = reported_missing_session_publish(&app);
    app.mode = AppMode::List;
    app.answer_review_watch_before_publish(true).await;

    // Assert
    assert!(accepted_publishes);
    assert!(!rejected_keeps_watch);
    assert!(declined_publishes);
    assert!(app.review_watch.watch_on_publish.is_empty());
    assert!(app.review_watch.pending_publish.is_none());
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn admitted_watch_answer_is_kept_until_its_own_publication_resolves() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    let branch_operation_lock = Arc::clone(
        &app.sessions
            .session_handles_or_err(&session_id)
            .expect("handles")
            .branch_operation_lock,
    );
    let _running_publish_guard = branch_operation_lock.lock_owned().await;

    // Act
    app.submit_review_request_publish(publish_restore_view(&session_id), None, false)
        .await;
    app.answer_review_watch_before_publish(true).await;
    let watched_operation_ids = app.review_watch.watch_on_publish.clone();
    app.apply_app_events(AppEvent::BranchPublishActionResolved {
        operation_id: "other-publish".to_string(),
        session_id: session_id.clone(),
    })
    .await;
    let other_resolution_keeps_watch = app.review_watch.watch_on_publish == watched_operation_ids;
    let operation_id = watched_operation_ids
        .iter()
        .next()
        .expect("admitted publication watch")
        .clone();
    app.apply_app_events(AppEvent::BranchPublishActionResolved {
        operation_id,
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert_eq!(watched_operation_ids.len(), 1);
    assert!(other_resolution_keeps_watch);
    assert!(app.review_watch.watch_on_publish.is_empty());
}

#[tokio::test]
async fn other_publication_of_the_session_does_not_consume_the_watch_answer() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = SessionId::from("session-1");
    app.review_watch
        .watch_on_publish
        .insert("approved-publish".to_string());

    // Act
    app.apply_app_events(publish_completed_event(&session_id, "other-publish", false))
        .await;

    // Assert
    assert!(
        app.review_watch
            .watch_on_publish
            .contains("approved-publish")
    );
    assert!(!app.review_watch.is_watched(&session_id));
}

#[tokio::test]
async fn pushed_or_watched_sessions_publish_without_asking() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let pushed_session_id = SessionId::from("pushed-session");
    let watched_session_id = SessionId::from("watched-session");
    app.review_watch
        .handled_keys
        .insert(watched_session_id.clone(), Vec::new());

    // Act
    app.submit_review_request_publish(publish_restore_view(&pushed_session_id), None, true)
        .await;
    let pushed_publishes = reported_missing_session_publish(&app);
    app.submit_review_request_publish(publish_restore_view(&watched_session_id), None, false)
        .await;

    // Assert
    assert!(pushed_publishes);
    assert!(reported_missing_session_publish(&app));
    assert!(app.review_watch.watch_on_publish.is_empty());
    assert!(app.review_watch.pending_publish.is_none());
}

#[tokio::test]
async fn watch_confirmation_opens_only_for_an_open_unwatched_review_request() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id =
        seed_review_session(&mut app, Status::Review, ReviewRequestState::Merged).await;
    let restore_view = publish_restore_view(&session_id);

    // Act
    let merged_opens = app.open_review_watch_confirmation(&session_id, None);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Open),
    );
    let open_opens = app.open_review_watch_confirmation(&session_id, Some(restore_view.clone()));
    let offered_mode = std::mem::replace(&mut app.mode, AppMode::List);
    app.enable_review_watch(&session_id).await;
    let watched_opens = app.open_review_watch_confirmation(&session_id, None);

    // Assert
    assert!(!merged_opens);
    assert!(open_opens);
    assert!(matches!(
        &offered_mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::WatchReviewRequest,
            confirmation_title,
            restore_view: Some(confirmation_restore_view),
            selected_confirmation_index: 0,
            ..
        } if confirmation_title == "Watch PR #42" && *confirmation_restore_view == restore_view
    ));
    assert!(!watched_opens);
}

#[tokio::test]
async fn enabling_and_disabling_a_watch_moves_the_session_between_review_and_watching() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;

    // Act
    app.enable_review_watch(&session_id).await;
    let watching_status = handle_status(&app, &session_id);
    let persisted_rows = app
        .services
        .db()
        .reviews()
        .load_session_review_watches()
        .await
        .expect("watch rows");
    let is_watched_target = App::review_request_sync_targets(&app.sessions)
        .iter()
        .any(|target| target.session_id == session_id && target.is_watched);
    app.disable_review_watch(&session_id).await;
    app.disable_review_watch(&session_id).await;

    // Assert
    assert_eq!(watching_status, Status::Watching);
    assert_eq!(
        persisted_rows,
        vec![SessionReviewWatchRow {
            handled_keys: Vec::new(),
            session_id: session_id.to_string(),
        }]
    );
    assert!(is_watched_target);
    assert_eq!(handle_status(&app, &session_id), Status::Review);
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
    assert!(!app.review_watch.is_watched(&session_id));
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains(
            "[Review Watch] Watching pull request #42 for failing checks, review comments, and \
             merge conflicts"
        ),
        "{transcript}"
    );
    assert_eq!(
        transcript
            .matches("[Review Watch] Stopped watching the review request")
            .count(),
        1,
        "{transcript}"
    );
}

#[tokio::test]
async fn enabling_a_watch_requires_an_open_review_request_and_working_storage() {
    // Arrange
    let (mut app, _base_dir, pool) = new_git_test_app_with_pool().await;
    let session_id =
        seed_review_session(&mut app, Status::Review, ReviewRequestState::Closed).await;

    // Act
    app.enable_review_watch(&session_id).await;
    let closed_is_watched = app.review_watch.is_watched(&session_id);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Open),
    );
    sqlx::query("DROP TABLE session_review_watch")
        .execute(&pool)
        .await
        .expect("drop watch table");
    app.enable_review_watch(&session_id).await;

    // Assert
    assert!(!closed_is_watched);
    assert!(!app.review_watch.is_watched(&session_id));
    assert_eq!(handle_status(&app, &session_id), Status::Review);
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Watch] Could not start watching:"),
        "{transcript}"
    );
}

#[tokio::test]
async fn forgetting_a_watch_survives_storage_failure() {
    // Arrange
    let (mut app, _base_dir, pool) = new_git_test_app_with_pool().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    sqlx::query("DROP TABLE session_review_watch")
        .execute(&pool)
        .await
        .expect("drop watch table");

    // Act
    let was_watched = app.forget_review_watch(&session_id).await;
    let is_still_watched = app.forget_review_watch(&session_id).await;

    // Assert
    assert!(was_watched);
    assert!(!is_still_watched);
}

#[tokio::test]
async fn reconciliation_only_moves_idle_sessions_whose_live_status_matches() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;

    // Act
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Closed),
    );
    app.reconcile_review_watch_statuses().await;
    let closed_status = handle_status(&app, &session_id);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Open),
    );
    *app.sessions
        .session_handles_or_err(&session_id)
        .expect("handles")
        .status
        .lock()
        .expect("status lock") = Status::InProgress;
    app.reconcile_review_watch_statuses().await;
    let busy_status = handle_status(&app, &session_id);
    app.sessions
        .session_handles_mut()
        .remove(session_id.as_str());
    app.reconcile_review_watch_statuses().await;

    // Assert
    assert_eq!(closed_status, Status::Review);
    assert_eq!(busy_status, Status::InProgress);
    assert_eq!(app.sessions.sessions()[0].status, Status::Review);
}

#[tokio::test]
async fn attention_is_ignored_unless_the_session_rests_in_watching() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;

    // Act
    app.apply_review_request_attention(&session_id, busy_attention())
        .await;
    let unwatched_transcript = transcript(&app, &session_id);
    app.enable_review_watch(&session_id).await;
    app.apply_review_request_attention(&session_id, ReviewRequestAttention::default())
        .await;
    set_session_status_for_test(&mut app, &session_id, Status::AgentReview);
    app.apply_review_request_attention(&session_id, busy_attention())
        .await;
    set_session_status_for_test(&mut app, &session_id, Status::Watching);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Merged),
    );
    app.apply_review_request_attention(&session_id, busy_attention())
        .await;

    // Assert
    assert!(!unwatched_transcript.contains("[Review Watch]"));
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&Vec::new())
    );
    let transcript = transcript(&app, &session_id);
    assert!(
        !transcript.contains("Resolving merge conflicts"),
        "{transcript}"
    );
}

#[tokio::test]
async fn attention_starts_one_follow_up_and_remembers_it() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    let failed_checks = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None), failed_check("lint", None)],
        head_commit: head.clone(),
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, failed_checks.clone())
        .await;
    set_session_status_for_test(&mut app, &session_id, Status::Watching);
    app.apply_review_request_attention(&session_id, failed_checks)
        .await;

    // Assert
    let persisted_rows = app
        .services
        .db()
        .reviews()
        .load_session_review_watches()
        .await
        .expect("watch rows");
    assert_eq!(
        persisted_rows[0].handled_keys,
        vec![format!("checks:{head}"), format!("checks-left:{head}")]
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript
            .matches("[Review Watch] Fixing 2 failing checks: test, lint")
            .count(),
        1,
        "{transcript}"
    );
    assert_eq!(
        transcript
            .matches(
                "[Review Watch] Leaving 2 failing checks to you after automatic attempts: test, \
                 lint"
            )
            .count(),
        1,
        "{transcript}"
    );
}

#[tokio::test]
async fn unfixed_checks_are_rerun_in_the_background_and_a_rejected_rerun_is_reported() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    app.review_watch
        .handled_keys
        .insert(session_id.clone(), vec![format!("checks:{head}")]);
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_rerun_failed_checks()
        .once()
        .withf(|remote, rerun_ids| remote.project == "agentty" && rerun_ids == &["101".to_string()])
        .returning(|_, _| {
            Box::pin(async {
                Err(ReviewRequestError::OperationFailed {
                    forge_kind: ForgeKind::GitHub,
                    message: "HTTP 403".to_string(),
                })
            })
        });
    app.services
        .set_review_request_client(Arc::new(review_request_client));
    let unfixed_checks = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101"), failed_check("sonar", None)],
        head_commit: head.clone(),
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, unfixed_checks.clone())
        .await;
    let rejected_rerun_notice = wait_for_transcript_text(
        &app,
        &session_id,
        "[Review Watch] Could not rerun the failing checks: GitHub review-request operation \
         failed: HTTP 403",
    )
    .await;
    let keys_after_rerun = app.review_watch.handled_keys.get(&session_id).cloned();
    app.apply_review_request_attention(&session_id, unfixed_checks.clone())
        .await;
    app.apply_review_request_attention(&session_id, unfixed_checks.clone())
        .await;
    let keys_while_rerun_in_flight = app.review_watch.handled_keys.get(&session_id).cloned();
    let transcript_while_rerun_in_flight = transcript(&app, &session_id);
    let rerun_finished = wait_for_rerun_finished(&mut app, &session_id).await;
    app.apply_review_request_attention(&session_id, unfixed_checks.clone())
        .await;
    app.apply_review_request_attention(&session_id, unfixed_checks)
        .await;

    // Assert
    assert_eq!(
        keys_after_rerun,
        Some(vec![format!("checks:{head}"), format!("rerun:{head}")])
    );
    assert_eq!(keys_while_rerun_in_flight, keys_after_rerun);
    assert!(
        !transcript_while_rerun_in_flight.contains("Leaving"),
        "{transcript_while_rerun_in_flight}"
    );
    assert!(rerun_finished);
    assert_eq!(handle_status(&app, &session_id), Status::Watching);
    assert!(rejected_rerun_notice, "{}", transcript(&app, &session_id));
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Watch] Rerunning 1 failing check: test"),
        "{transcript}"
    );
    assert_eq!(
        transcript
            .matches("[Review Watch] Leaving 2 failing checks to you after automatic attempts")
            .count(),
        1,
        "{transcript}"
    );
}

#[tokio::test]
async fn rerun_reports_a_forge_remote_that_cannot_be_resolved() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    app.review_watch
        .handled_keys
        .insert(session_id.clone(), vec![format!("checks:{head}")]);
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|repo_url| Err(ReviewRequestError::UnsupportedRemote { repo_url }));
    review_request_client.expect_rerun_failed_checks().never();
    app.services
        .set_review_request_client(Arc::new(review_request_client));
    let unfixed_checks = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101")],
        head_commit: head,
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, unfixed_checks)
        .await;
    let unresolved_remote_notice = wait_for_transcript_text(
        &app,
        &session_id,
        "[Review Watch] Could not rerun the failing checks: Review requests are only supported",
    )
    .await;

    // Assert
    assert!(
        unresolved_remote_notice,
        "{}",
        transcript(&app, &session_id)
    );
}

#[tokio::test]
async fn accepted_rerun_finishes_without_a_rejection_notice() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    app.review_watch
        .handled_keys
        .insert(session_id.clone(), vec![format!("checks:{head}")]);
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_remote()));
    review_request_client
        .expect_rerun_failed_checks()
        .once()
        .withf(|_, rerun_ids| rerun_ids == &["101".to_string()])
        .returning(|_, _| Box::pin(async { Ok(()) }));
    app.services
        .set_review_request_client(Arc::new(review_request_client));
    let unfixed_checks = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101")],
        head_commit: head.clone(),
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, unfixed_checks)
        .await;
    let rerun_finished = wait_for_rerun_finished(&mut app, &session_id).await;

    // Assert
    assert!(rerun_finished);
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec![format!("checks:{head}"), format!("rerun:{head}")])
    );
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Watch] Rerunning 1 failing check: test"),
        "{transcript}"
    );
    assert!(!transcript.contains("Could not rerun"), "{transcript}");
    assert!(!transcript.contains("Leaving"), "{transcript}");
}

#[tokio::test]
async fn unfixed_checks_wait_until_the_finished_turn_is_pushed() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    app.review_watch
        .handled_keys
        .insert(session_id.clone(), vec![format!("checks:{head}")]);
    let unfixed_checks = ReviewRequestAttention {
        failed_checks: vec![failed_check("sonar", None)],
        head_commit: head.clone(),
        ..ReviewRequestAttention::default()
    };
    app.sessions
        .start_published_branch_sync(&session_id, "push-1".to_string());

    // Act
    app.apply_review_request_attention(&session_id, unfixed_checks.clone())
        .await;
    let keys_while_pushing = app.review_watch.handled_keys.get(&session_id).cloned();
    app.sessions
        .finish_published_branch_sync(&session_id, "push-1", None);
    app.apply_review_request_attention(&session_id, unfixed_checks)
        .await;

    // Assert
    assert_eq!(keys_while_pushing, Some(vec![format!("checks:{head}")]));
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec![
            format!("checks:{head}"),
            format!("checks-left:{head}")
        ])
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript
            .matches(
                "[Review Watch] Leaving 1 failing check to you after automatic attempts: sonar"
            )
            .count(),
        1,
        "{transcript}"
    );
}

#[tokio::test]
async fn reports_fetched_before_the_fix_commit_was_pushed_leave_its_checks_alone() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    app.review_watch
        .handled_keys
        .insert(session_id.clone(), vec!["checks:abc123".to_string()]);
    let replaced_head_checks = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101"), failed_check("sonar", None)],
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    };
    let mut review_request_client = MockReviewRequestClient::new();
    review_request_client.expect_detect_remote().never();
    review_request_client.expect_rerun_failed_checks().never();
    app.services
        .set_review_request_client(Arc::new(review_request_client));

    // Act
    app.apply_review_request_attention(&session_id, replaced_head_checks)
        .await;

    // Assert
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec!["checks:abc123".to_string()])
    );
    let transcript = transcript(&app, &session_id);
    assert!(!transcript.contains("Rerunning"), "{transcript}");
    assert!(!transcript.contains("Leaving"), "{transcript}");
}

#[tokio::test]
async fn rerun_checks_reported_running_are_left_to_the_user_once_they_finish_failing() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    app.review_watch.handled_keys.insert(
        session_id.clone(),
        vec![format!("checks:{head}"), format!("rerun:{head}")],
    );
    let finished_failures = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101")],
        head_commit: head.clone(),
        ..ReviewRequestAttention::default()
    };
    let rerun_running = ReviewRequestAttention {
        has_pending_checks: true,
        ..finished_failures.clone()
    };

    // Act
    app.apply_review_request_attention(&session_id, finished_failures.clone())
        .await;
    let transcript_before_rerun_started = transcript(&app, &session_id);
    app.apply_review_request_attention(&session_id, rerun_running.clone())
        .await;
    app.apply_review_request_attention(&session_id, rerun_running)
        .await;
    let keys_after_rerun_started = app.review_watch.handled_keys.get(&session_id).cloned();
    app.apply_review_request_attention(&session_id, finished_failures)
        .await;

    // Assert
    assert!(
        !transcript_before_rerun_started.contains("Leaving"),
        "{transcript_before_rerun_started}"
    );
    assert_eq!(
        keys_after_rerun_started,
        Some(vec![
            format!("checks:{head}"),
            format!("rerun:{head}"),
            format!("rerun-awaited:{head}"),
            format!("rerun-started:{head}"),
        ])
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript
            .matches("[Review Watch] Leaving 1 failing check to you after automatic attempts: test")
            .count(),
        1,
        "{transcript}"
    );
}

#[tokio::test]
async fn rerun_checks_never_reported_running_are_left_to_the_user_after_a_later_report() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    app.review_watch.handled_keys.insert(
        session_id.clone(),
        vec![format!("checks:{head}"), format!("rerun:{head}")],
    );
    let finished_failures = ReviewRequestAttention {
        failed_checks: vec![rerunnable_check("test", "101")],
        head_commit: head.clone(),
        ..ReviewRequestAttention::default()
    };
    let passing = ReviewRequestAttention {
        failed_checks: Vec::new(),
        ..finished_failures.clone()
    };

    // Act
    app.apply_review_request_attention(&session_id, passing)
        .await;
    let keys_after_passing = app.review_watch.handled_keys.get(&session_id).cloned();
    app.apply_review_request_attention(&session_id, finished_failures.clone())
        .await;
    let transcript_after_awaited_report = transcript(&app, &session_id);
    let keys_after_awaited_report = app.review_watch.handled_keys.get(&session_id).cloned();
    app.apply_review_request_attention(&session_id, finished_failures.clone())
        .await;
    app.apply_review_request_attention(&session_id, finished_failures)
        .await;

    // Assert
    assert_eq!(
        keys_after_passing,
        Some(vec![format!("checks:{head}"), format!("rerun:{head}")])
    );
    assert_eq!(
        keys_after_awaited_report,
        Some(vec![
            format!("checks:{head}"),
            format!("rerun:{head}"),
            format!("rerun-awaited:{head}"),
        ])
    );
    assert!(
        !transcript_after_awaited_report.contains("Leaving"),
        "{transcript_after_awaited_report}"
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript
            .matches("[Review Watch] Leaving 1 failing check to you after automatic attempts: test")
            .count(),
        1,
        "{transcript}"
    );
}

/// Builds the GitHub remote returned by mocked remote detection.
fn github_remote() -> ForgeRemote {
    ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Waits up to five seconds for background work to append `text` to the
/// session transcript, returning whether it appeared.
async fn wait_for_transcript_text(app: &App, session_id: &str, text: &str) -> bool {
    for _ in 0..500 {
        if transcript(app, session_id).contains(text) {
            return true;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    false
}

/// Applies queued app events for up to five seconds until the rerun requests
/// of `session_id` are reported finished, returning whether they were.
async fn wait_for_rerun_finished(app: &mut App, session_id: &SessionId) -> bool {
    for _ in 0..500 {
        app.process_pending_app_events().await;
        if !app.review_watch.reruns_in_flight.contains(session_id) {
            return true;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    false
}

#[tokio::test]
async fn failed_check_follow_up_hides_its_prompt_behind_a_progress_label() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let failed_checks = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None), failed_check("lint", None)],
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, failed_checks)
        .await;

    // Assert
    let user_prompt_count = app
        .sessions
        .session_handles_or_err(&session_id)
        .expect("handles")
        .transcript
        .lock()
        .expect("transcript lock")
        .messages()
        .iter()
        .filter(|message| message.kind == SessionMessageKind::UserPrompt)
        .count();
    assert_eq!(user_prompt_count, 0);
    let progress = app
        .sessions
        .session_for_id(&session_id)
        .and_then(|session| {
            session
                .transient_messages
                .get(TransientMessageSlot::GeneratedTurnProgress)
        })
        .map(|message| message.body.clone());
    assert_eq!(
        progress,
        Some(TransientMessageBody::Loading(
            "Addressing 2 failing checks: test, lint...".to_string()
        ))
    );
}

#[test]
fn retained_keys_match_the_current_head_commit_and_thread_states() {
    // Arrange
    let attention = ReviewRequestAttention {
        head_commit: "abc123".to_string(),
        review_threads: vec![review_thread("thread-1", 2), review_thread("thread-2", 1)],
        ..ReviewRequestAttention::default()
    };
    let handled_keys = [
        "conflict:old456",
        "checks:old456",
        "thread:thread-1:1",
        "thread:thread-gone:1",
        "thread:thread-2:1",
        "checks:abc123",
        "rerun:old456",
        "rerun:abc123",
        "rerun-awaited:old456",
        "rerun-awaited:abc123",
        "rerun-started:old456",
        "rerun-started:abc123",
        "checks-left:old456",
        "checks-left:abc123",
        "conflict:abc123",
    ]
    .map(str::to_string);

    // Act
    let retained_keys = retain_current_keys(&handled_keys, &attention);

    // Assert
    assert_eq!(
        retained_keys,
        [
            "thread:thread-2:1",
            "checks:abc123",
            "rerun:abc123",
            "rerun-awaited:abc123",
            "rerun-started:abc123",
            "checks-left:abc123",
            "conflict:abc123"
        ]
    );
}

#[tokio::test]
async fn attention_drops_stale_keys_without_repeating_handled_follow_ups() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let current_keys = vec![
        "thread:thread-1:1".to_string(),
        "checks:abc123".to_string(),
        "checks-left:abc123".to_string(),
    ];
    let stale_keys = (0..200).flat_map(|index| {
        [
            format!("checks:old{index}"),
            format!("thread:thread-1:{}", index + 2),
        ]
    });
    app.review_watch.handled_keys.insert(
        session_id.clone(),
        current_keys.iter().cloned().chain(stale_keys).collect(),
    );
    let handled_attention = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        head_commit: "abc123".to_string(),
        review_threads: vec![review_thread("thread-1", 1)],
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, handled_attention.clone())
        .await;
    app.apply_review_request_attention(&session_id, handled_attention)
        .await;

    // Assert
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&current_keys)
    );
    let persisted_rows = app
        .services
        .db()
        .reviews()
        .load_session_review_watches()
        .await
        .expect("watch rows");
    assert_eq!(persisted_rows[0].handled_keys, current_keys);
    assert_eq!(handle_status(&app, &session_id), Status::Watching);
    let transcript = transcript(&app, &session_id);
    assert!(!transcript.contains("Addressing"), "{transcript}");
    assert!(!transcript.contains("Fixing"), "{transcript}");
    assert!(!transcript.contains("Leaving"), "{transcript}");
}

/// Builds forge facts reporting only a merge conflict at `abc123`.
fn conflict_attention() -> ReviewRequestAttention {
    ReviewRequestAttention {
        has_merge_conflict: true,
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    }
}

/// Builds forge facts reporting only one reviewer thread.
fn comment_attention() -> ReviewRequestAttention {
    ReviewRequestAttention {
        head_commit: "abc123".to_string(),
        review_threads: vec![review_thread("thread-1", 1)],
        ..ReviewRequestAttention::default()
    }
}

#[tokio::test]
async fn attention_starts_sync_and_review_comment_turns_through_session_workflows() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;

    // Act
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;
    set_session_status_for_test(&mut app, &session_id, Status::Watching);
    app.apply_review_request_attention(&session_id, comment_attention())
        .await;

    // Assert
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Watch] Resolving merge conflicts with `main`"),
        "{transcript}"
    );
    assert!(
        transcript.contains("[Review Watch] Addressing 1 review comment"),
        "{transcript}"
    );
    assert!(!transcript.contains("Could not start"), "{transcript}");
}

#[tokio::test]
async fn attention_addresses_threads_and_failed_checks_in_one_turn() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let head = session_head(&app, &session_id).await;
    let attention = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        head_commit: head.clone(),
        ..comment_attention()
    };

    // Act
    app.apply_review_request_attention(&session_id, attention.clone())
        .await;
    set_session_status_for_test(&mut app, &session_id, Status::Watching);
    app.apply_review_request_attention(&session_id, attention)
        .await;

    // Assert
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec![
            "thread:thread-1:1".to_string(),
            format!("checks:{head}"),
            format!("checks-left:{head}"),
        ])
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript
            .matches("[Review Watch] Addressing 1 review comment and 1 failing check: test")
            .count(),
        1,
        "{transcript}"
    );
    assert!(!transcript.contains("Fixing"), "{transcript}");
    assert!(
        transcript
            .contains("[Review Watch] Leaving 1 failing check to you after automatic attempts"),
        "{transcript}"
    );
}

#[tokio::test]
async fn attention_reports_a_thread_that_no_longer_needs_a_response() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let mut resolved_thread = review_thread("thread-1", 1);
    resolved_thread.is_resolved = true;
    let attention = ReviewRequestAttention {
        head_commit: "abc123".to_string(),
        review_threads: vec![resolved_thread],
        ..ReviewRequestAttention::default()
    };

    // Act
    app.apply_review_request_attention(&session_id, attention)
        .await;

    // Assert
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains(
            "Could not start: no review comment needs a response; retrying on the next check"
        ),
        "{transcript}"
    );
}

#[tokio::test]
async fn attention_retries_follow_ups_that_cannot_start_and_reports_them_once() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    // A running project sync blocks the conflict sync, and a waiting
    // generated turn rejects the review-comment turn.
    app.project_sync_status = Some(ProjectSyncStatus {
        context: ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id: app.active_project_id(),
            project_name: "agentty".to_string(),
        },
        phase: ProjectSyncPhase::Running,
    });
    app.sessions
        .queue_generated_turn(&session_id, 0, "earlier follow-up".to_string());

    // Act
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;
    let conflict_failed_starts = app.review_watch.failed_starts.get(&session_id).cloned();
    app.apply_review_request_attention(&session_id, comment_attention())
        .await;

    // Assert
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&Vec::new())
    );
    assert_eq!(
        conflict_failed_starts,
        Some(vec!["conflict:abc123".to_string()])
    );
    assert_eq!(
        app.review_watch.failed_starts.get(&session_id),
        Some(&vec!["thread:thread-1:1".to_string()])
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript.matches("Resolving merge conflicts").count(),
        1,
        "{transcript}"
    );
    assert_eq!(
        transcript
            .matches("[Review Watch] Could not start:")
            .count(),
        2,
        "{transcript}"
    );
    assert!(
        transcript
            .contains("the session did not accept the review comments; retrying on the next check"),
        "{transcript}"
    );
}

#[tokio::test]
async fn failed_check_follow_up_rejected_by_the_session_starts_on_the_next_report() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let failed_checks = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    };
    // A waiting generated turn rejects the new one.
    app.sessions
        .queue_generated_turn(&session_id, 0, "earlier follow-up".to_string());

    // Act
    app.apply_review_request_attention(&session_id, failed_checks.clone())
        .await;
    let rejected_keys = app.review_watch.handled_keys.get(&session_id).cloned();
    app.sessions
        .update_generated_turn(&session_id, GeneratedTurnUpdate::Skipped);
    app.apply_review_request_attention(&session_id, failed_checks)
        .await;

    // Assert
    assert_eq!(rejected_keys, Some(Vec::new()));
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec!["checks:abc123".to_string()])
    );
    assert!(!app.review_watch.failed_starts.contains_key(&session_id));
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains(
            "[Review Watch] Could not start: the session did not accept the prompt; retrying on \
             the next check"
        ),
        "{transcript}"
    );
    assert_eq!(
        transcript.matches("Fixing 1 failing check").count(),
        1,
        "{transcript}"
    );
}

#[tokio::test]
async fn conflict_follow_up_waits_while_the_review_request_targets_another_branch() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let mut retargeted_review_request = review_request(ForgeKind::GitHub, ReviewRequestState::Open);
    retargeted_review_request.summary.target_branch = "release".to_string();
    app.sessions
        .apply_review_request(&session_id, retargeted_review_request);

    // Act
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;
    let retargeted_keys = app.review_watch.handled_keys.get(&session_id).cloned();
    let retargeted_status = handle_status(&app, &session_id);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Open),
    );
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;

    // Assert
    assert_eq!(retargeted_keys, Some(Vec::new()));
    assert_eq!(retargeted_status, Status::Watching);
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec!["conflict:abc123".to_string()])
    );
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains(
            "[Review Watch] Could not start: the review request targets `release`, but this \
             session syncs with `main`; retrying on the next check"
        ),
        "{transcript}"
    );
}

#[tokio::test]
async fn conflict_follow_up_blocked_by_a_project_sync_starts_on_the_next_report() {
    // Arrange
    let (mut app, _base_dir, pool) = new_git_test_app_with_pool().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    sqlx::query("DROP TABLE session_review_watch")
        .execute(&pool)
        .await
        .expect("drop watch table");
    app.project_sync_status = Some(ProjectSyncStatus {
        context: ProjectSyncContext {
            default_branch: "main".to_string(),
            operation_id: 1,
            project_id: app.active_project_id(),
            project_name: "agentty".to_string(),
        },
        phase: ProjectSyncPhase::Running,
    });

    // Act
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;
    let blocked_keys = app.review_watch.handled_keys.get(&session_id).cloned();
    app.project_sync_status = None;
    app.apply_review_request_attention(&session_id, conflict_attention())
        .await;

    // Assert
    assert_eq!(blocked_keys, Some(Vec::new()));
    assert_eq!(
        app.review_watch.handled_keys.get(&session_id),
        Some(&vec!["conflict:abc123".to_string()])
    );
    assert!(!app.review_watch.failed_starts.contains_key(&session_id));
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("is synchronizing `main`"),
        "{transcript}"
    );
    assert_eq!(
        transcript.matches("Could not start").count(),
        1,
        "{transcript}"
    );
    assert_eq!(
        transcript.matches("Resolving merge conflicts").count(),
        1,
        "{transcript}"
    );
}

/// Builds one completed review-request publish event for `session_id` and its
/// worker `operation_id`.
fn publish_completed_event(
    session_id: &SessionId,
    operation_id: &str,
    is_published: bool,
) -> AppEvent {
    let result = if is_published {
        Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session".to_string(),
            review_request: review_request(ForgeKind::GitHub, ReviewRequestState::Open),
            upstream_reference: "origin/wt/session".to_string(),
        })
    } else {
        Err(BranchPublishTaskFailure::failed(
            PublishBranchAction::PublishPullRequest,
            "push rejected".to_string(),
        ))
    };

    AppEvent::BranchPublishActionCompleted {
        operation_id: Some(operation_id.to_string()),
        result: Box::new(result),
        session_id: session_id.clone(),
    }
}

/// Builds one current-generation review-request status event.
fn status_updated_event(
    app: &App,
    session_id: &SessionId,
    state: ReviewRequestState,
    attention: Option<Result<ReviewRequestAttention, String>>,
) -> AppEvent {
    let outcome = match state {
        ReviewRequestState::Open => SyncReviewRequestOutcome::Open {
            display_id: "#42".to_string(),
            status_summary: None,
        },
        ReviewRequestState::Merged => SyncReviewRequestOutcome::Merged {
            display_id: "#42".to_string(),
            session_head_hash: None,
        },
        ReviewRequestState::Closed => SyncReviewRequestOutcome::Closed {
            display_id: "#42".to_string(),
        },
    };

    AppEvent::ReviewRequestStatusUpdated {
        generation: app.sync_handle.current_generation(),
        result: Ok(SyncReviewRequestTaskResult {
            attention,
            outcome,
            summary: Some(review_request(ForgeKind::GitHub, state).summary),
        }),
        session_id: session_id.clone(),
    }
}

#[tokio::test]
async fn finished_publication_starts_watching_only_when_requested() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;

    // Act
    app.apply_app_events(publish_completed_event(&session_id, "unrequested", true))
        .await;
    let unrequested_is_watched = app.review_watch.is_watched(&session_id);
    app.review_watch
        .watch_on_publish
        .insert("failed".to_string());
    app.apply_app_events(publish_completed_event(&session_id, "failed", false))
        .await;
    let failed_is_watched = app.review_watch.is_watched(&session_id);
    let failed_keeps_request = !app.review_watch.watch_on_publish.is_empty();
    app.review_watch
        .watch_on_publish
        .insert("published".to_string());
    app.apply_app_events(publish_completed_event(&session_id, "published", true))
        .await;

    // Assert
    assert!(!unrequested_is_watched);
    assert!(!failed_is_watched);
    assert!(!failed_keeps_request);
    assert!(app.review_watch.is_watched(&session_id));
    assert!(app.review_watch.watch_on_publish.is_empty());
    assert_eq!(handle_status(&app, &session_id), Status::Watching);
    let transcript = transcript(&app, &session_id);
    let created_notice = transcript
        .find("[Review Request] Created PR")
        .expect("created notice");
    let watching_notice = transcript
        .find("[Review Watch] Watching pull request #42")
        .expect("watching notice");
    assert!(created_notice < watching_notice, "{transcript}");
}

#[tokio::test]
async fn finished_publication_starts_watching_after_its_project_is_unloaded() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.sessions.state_mut().replace_sessions(Vec::new());
    app.review_watch
        .watch_on_publish
        .insert("published".to_string());

    // Act
    app.apply_app_events(publish_completed_event(&session_id, "published", true))
        .await;

    // Assert
    assert!(app.review_watch.is_watched(&session_id));
    assert!(app.review_watch.watch_on_publish.is_empty());
    let persisted_rows = app
        .services
        .db()
        .reviews()
        .load_session_review_watches()
        .await
        .expect("watch rows");
    assert_eq!(
        persisted_rows,
        vec![SessionReviewWatchRow {
            handled_keys: Vec::new(),
            session_id: session_id.to_string(),
        }]
    );
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("session messages");
    assert!(
        persisted_messages.iter().any(|message| message
            .content
            .contains("[Review Watch] Watching pull request #42")),
        "{persisted_messages:?}"
    );
    assert!(transcript(&app, &session_id).contains("[Review Watch] Watching pull request #42"));
}

#[tokio::test]
async fn finished_publication_watches_a_session_without_runtime_handles() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = SessionId::from(app.create_session().await.expect("session"));
    app.sessions.state_mut().replace_sessions(Vec::new());
    app.sessions.session_handles_mut().remove(&session_id);
    app.review_watch
        .watch_on_publish
        .insert("published".to_string());

    // Act
    app.apply_app_events(publish_completed_event(&session_id, "published", true))
        .await;

    // Assert
    assert!(app.review_watch.is_watched(&session_id));
    let persisted_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&session_id)
        .await
        .expect("session messages");
    assert!(
        !persisted_messages
            .iter()
            .any(|message| message.content.contains("[Review Watch]")),
        "{persisted_messages:?}"
    );
}

#[tokio::test]
async fn finished_publication_does_not_watch_a_review_request_that_is_not_open() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.review_watch
        .watch_on_publish
        .insert("published".to_string());

    // Act
    app.apply_app_events(AppEvent::BranchPublishActionCompleted {
        operation_id: Some("published".to_string()),
        result: Box::new(Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session".to_string(),
            review_request: review_request(ForgeKind::GitHub, ReviewRequestState::Merged),
            upstream_reference: "origin/wt/session".to_string(),
        })),
        session_id: session_id.clone(),
    })
    .await;

    // Assert
    assert!(!app.review_watch.is_watched(&session_id));
    assert!(app.review_watch.watch_on_publish.is_empty());
    assert!(!transcript(&app, &session_id).contains("[Review Watch]"));
}

#[tokio::test]
async fn watched_status_updates_resume_watching_start_follow_up_and_stop_when_merged() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    set_session_status_for_test(&mut app, &session_id, Status::Review);
    let attention = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None)],
        head_commit: "abc123".to_string(),
        ..ReviewRequestAttention::default()
    };

    // Act
    let resume_event = status_updated_event(&app, &session_id, ReviewRequestState::Open, None);
    app.apply_app_events(resume_event).await;
    let resumed_status = handle_status(&app, &session_id);
    let follow_up_event = status_updated_event(
        &app,
        &session_id,
        ReviewRequestState::Open,
        Some(Ok(attention)),
    );
    app.apply_app_events(follow_up_event).await;
    let follow_up_transcript = transcript(&app, &session_id);
    let merged_event = status_updated_event(&app, &session_id, ReviewRequestState::Merged, None);
    app.apply_app_events(merged_event).await;

    // Assert
    assert_eq!(resumed_status, Status::Watching);
    assert!(
        follow_up_transcript.contains("[Review Watch] Fixing 1 failing check: test"),
        "{follow_up_transcript}"
    );
    assert!(!app.review_watch.is_watched(&session_id));
}

/// Builds one current-generation open status event whose pull request waits
/// in the forge's merge queue.
fn merge_queue_status_event(app: &App, session_id: &SessionId) -> AppEvent {
    let status_summary = Some(format!(
        "Approved, {}",
        ReviewRequestSummary::GITHUB_MERGE_QUEUE_STATUS_COMPONENT
    ));
    let mut event = status_updated_event(app, session_id, ReviewRequestState::Open, None);
    if let AppEvent::ReviewRequestStatusUpdated {
        result: Ok(task_result),
        ..
    } = &mut event
        && let Some(summary) = &mut task_result.summary
    {
        summary.status_summary = status_summary;
    }

    event
}

#[tokio::test]
async fn forge_merge_queue_entry_holds_the_session_in_queued_until_it_leaves() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;

    // Act
    let queued_event = merge_queue_status_event(&app, &session_id);
    app.apply_app_events(queued_event).await;
    let queued_status = handle_status(&app, &session_id);
    let is_queued_sync_target = App::review_request_sync_targets(&app.sessions)
        .iter()
        .any(|target| target.session_id == session_id && !target.is_watched);
    let dequeued_event = status_updated_event(&app, &session_id, ReviewRequestState::Open, None);
    app.apply_app_events(dequeued_event).await;
    let dequeued_status = handle_status(&app, &session_id);
    let requeued_event = merge_queue_status_event(&app, &session_id);
    app.apply_app_events(requeued_event).await;
    let requeued_status = handle_status(&app, &session_id);
    let merged_event = status_updated_event(&app, &session_id, ReviewRequestState::Merged, None);
    app.apply_app_events(merged_event).await;

    // Assert
    assert_eq!(queued_status, Status::Queued);
    assert!(is_queued_sync_target);
    assert_eq!(dequeued_status, Status::Watching);
    assert_eq!(requeued_status, Status::Queued);
    assert_eq!(handle_status(&app, &session_id), Status::Merged);
    assert!(!app.review_watch.is_watched(&session_id));
}

#[tokio::test]
async fn reconciliation_leaves_sessions_in_agentty_merge_queue_alone() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Queued, ReviewRequestState::Open).await;
    app.merge_queue.enqueue(session_id.clone());

    // Act
    app.reconcile_review_watch_statuses().await;
    let locally_queued_status = handle_status(&app, &session_id);
    app.merge_queue.pop_next();
    app.reconcile_review_watch_statuses().await;

    // Assert
    assert_eq!(locally_queued_status, Status::Queued);
    assert_eq!(handle_status(&app, &session_id), Status::Review);
}

/// Applies one current-generation open status update carrying `attention`.
async fn apply_open_status_update(
    app: &mut App,
    session_id: &SessionId,
    attention: Result<ReviewRequestAttention, String>,
) {
    let event = status_updated_event(app, session_id, ReviewRequestState::Open, Some(attention));
    app.apply_app_events(event).await;
}

/// Counts the lookup-failure notices in one session transcript.
fn lookup_failure_notices(app: &App, session_id: &str) -> usize {
    transcript(app, session_id)
        .matches("[Review Watch] Could not check the review request: checks unavailable")
        .count()
}

#[tokio::test]
async fn failed_attention_lookups_are_reported_once_until_a_lookup_succeeds() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    let failure = || Err("checks unavailable".to_string());

    // Act
    apply_open_status_update(&mut app, &session_id, failure()).await;
    let unwatched_notices = lookup_failure_notices(&app, &session_id);
    app.enable_review_watch(&session_id).await;
    apply_open_status_update(&mut app, &session_id, failure()).await;
    apply_open_status_update(&mut app, &session_id, failure()).await;
    let repeated_failure_notices = lookup_failure_notices(&app, &session_id);
    apply_open_status_update(&mut app, &session_id, Ok(ReviewRequestAttention::default())).await;
    apply_open_status_update(&mut app, &session_id, failure()).await;
    let recurring_failure_notices = lookup_failure_notices(&app, &session_id);
    app.disable_review_watch(&session_id).await;

    // Assert
    assert_eq!(unwatched_notices, 0);
    assert_eq!(repeated_failure_notices, 1);
    assert_eq!(recurring_failure_notices, 2);
    assert!(app.review_watch.failed_lookups.is_empty());
}

#[tokio::test]
async fn closing_a_watched_review_request_stops_watching() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;

    // Act
    let closed_event = status_updated_event(&app, &session_id, ReviewRequestState::Closed, None);
    app.apply_app_events(closed_event).await;

    // Assert
    assert!(!app.review_watch.is_watched(&session_id));
    assert_eq!(handle_status(&app, &session_id), Status::Canceled);
}

#[tokio::test]
async fn focused_review_pauses_watching_until_it_finishes() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;

    // Act
    crate::app::review::mark_session_agent_review(app.sessions.state_mut(), &session_id);
    app.reconcile_review_watch_statuses().await;
    let reviewing_status = handle_status(&app, &session_id);
    crate::app::review::restore_session_review_status(app.sessions.state_mut(), &session_id);
    app.reconcile_review_watch_statuses().await;

    // Assert
    assert_eq!(reviewing_status, Status::AgentReview);
    assert_eq!(handle_status(&app, &session_id), Status::Watching);
}
