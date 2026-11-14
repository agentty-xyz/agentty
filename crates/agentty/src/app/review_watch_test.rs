//! Review-request watch tests.

use ag_forge::{
    ReviewCheckFailure, ReviewComment, ReviewCommentAnchorSide, ReviewCommentThread,
    ReviewRequestAttention,
};

use super::{
    ReviewWatchAction, ReviewWatchState, build_fix_failed_checks_prompt, retain_current_keys,
};
use crate::app::branch_publish::BranchPublishTaskSuccess;
use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::session::SyncReviewRequestOutcome;
use crate::app::sync::{ProjectSyncContext, ProjectSyncPhase, ProjectSyncStatus};
use crate::app::{App, AppEvent, Tab};
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestState, ReviewRequestSummary, SessionId, Status,
};
use crate::domain::session_message::SessionTranscript;
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

/// Builds one failed check with an optional details URL.
fn failed_check(name: &str, web_url: Option<&str>) -> ReviewCheckFailure {
    ReviewCheckFailure {
        name: name.to_string(),
        outcome: "Failure".to_string(),
        web_url: web_url.map(str::to_string),
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
fn plan_prefers_conflicts_then_comments_then_completed_failed_checks() {
    // Arrange
    let attention = busy_attention();
    let conflict_handled = vec!["conflict:abc123".to_string()];
    let comments_handled = vec![
        "conflict:abc123".to_string(),
        "thread:thread-1:1".to_string(),
    ];
    let everything_handled = vec![
        "conflict:abc123".to_string(),
        "thread:thread-1:1".to_string(),
        "checks:abc123".to_string(),
    ];

    // Act
    let conflict = ReviewWatchAction::plan(&attention, &[]);
    let comments = ReviewWatchAction::plan(&attention, &conflict_handled);
    let checks = ReviewWatchAction::plan(&attention, &comments_handled);
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
        Some(ReviewWatchAction::AddressComments {
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
    assert_eq!(nothing, None);
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
        Some(ReviewWatchAction::AddressComments {
            threads: vec![review_thread("thread-1", 2)],
        })
    );
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
            ReviewWatchAction::AddressComments {
                threads: vec![review_thread("thread-1", 1), review_thread("thread-2", 3)],
            },
            "Addressing 2 review comments",
            vec![
                "thread:thread-1:1".to_string(),
                "thread:thread-2:3".to_string(),
            ],
        ),
        (
            ReviewWatchAction::FixChecks {
                failed_checks: vec![failed_check("test", None)],
                head_commit: "abc123".to_string(),
            },
            "Fixing 1 failing check: test",
            vec!["checks:abc123".to_string()],
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

#[tokio::test]
async fn watch_offer_opens_once_for_an_open_unwatched_review_request() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.tabs = crate::app::TabManager::new(Tab::Sessions);
    app.mode = AppMode::List;

    // Act
    app.offer_review_watch(session_id.clone());
    app.offer_review_watch(session_id.clone());
    app.present_review_watch_offer();
    let offered_mode = std::mem::replace(&mut app.mode, AppMode::List);
    app.present_review_watch_offer();

    // Assert
    assert!(matches!(
        &offered_mode,
        AppMode::Confirmation {
            confirmation_intent: ConfirmationIntent::WatchReviewRequest,
            confirmation_message,
            confirmation_title,
            restore_view: None,
            session_id: Some(offered_session_id),
            selected_confirmation_index: 1,
        } if confirmation_title == "Watch PR #42"
            && confirmation_message == "Auto-resolve checks, comments, conflicts?"
            && *offered_session_id == session_id
    ));
    assert!(matches!(app.mode, AppMode::List));
}

#[tokio::test]
async fn watch_offer_waits_for_an_idle_page_and_restores_the_viewed_session() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    let view = ConfirmationViewMode {
        scroll_offset: Some(3),
        session_id: session_id.clone(),
    };
    app.offer_review_watch(session_id);

    // Act
    app.tabs = crate::app::TabManager::new(Tab::Settings);
    app.mode = AppMode::List;
    app.present_review_watch_offer();
    let stays_on_settings = matches!(app.mode, AppMode::List);
    app.mode = view.clone().into_view_mode();
    app.present_review_watch_offer();

    // Assert
    assert!(stays_on_settings);
    assert!(matches!(
        &app.mode,
        AppMode::Confirmation {
            restore_view: Some(restore_view),
            ..
        } if *restore_view == view
    ));
}

#[tokio::test]
async fn watch_offer_skips_sessions_that_can_no_longer_be_watched() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id =
        seed_review_session(&mut app, Status::Review, ReviewRequestState::Merged).await;
    app.tabs = crate::app::TabManager::new(Tab::Sessions);
    app.mode = AppMode::List;
    app.offer_review_watch(SessionId::from("missing-session"));
    app.offer_review_watch(session_id.clone());

    // Act
    app.present_review_watch_offer();
    let merged_stays_closed = matches!(app.mode, AppMode::List);
    app.sessions.apply_review_request(
        &session_id,
        review_request(ForgeKind::GitHub, ReviewRequestState::Open),
    );
    app.enable_review_watch(&session_id).await;
    let watched_opens = app.open_review_watch_confirmation(&session_id, None);

    // Assert
    assert!(merged_stays_closed);
    assert!(app.review_watch.pending_offers.is_empty());
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
    let failed_checks = ReviewRequestAttention {
        failed_checks: vec![failed_check("test", None), failed_check("lint", None)],
        head_commit: "abc123".to_string(),
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
        vec!["checks:abc123".to_string()]
    );
    let transcript = transcript(&app, &session_id);
    assert_eq!(
        transcript
            .matches("[Review Watch] Fixing 2 failing checks: test, lint")
            .count(),
        1,
        "{transcript}"
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
        "conflict:abc123",
    ]
    .map(str::to_string);

    // Act
    let retained_keys = retain_current_keys(&handled_keys, &attention);

    // Assert
    assert_eq!(
        retained_keys,
        ["thread:thread-2:1", "checks:abc123", "conflict:abc123"]
    );
}

#[tokio::test]
async fn attention_drops_stale_keys_without_repeating_handled_follow_ups() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    let current_keys = vec!["thread:thread-1:1".to_string(), "checks:abc123".to_string()];
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
async fn attention_retries_follow_ups_that_cannot_start_and_reports_them_once() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;
    app.enable_review_watch(&session_id).await;
    app.sessions.sessions_mut()[0].parent_session_id = Some(SessionId::from("missing-parent"));

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

/// Builds one completed review-request publish event for `session_id`.
fn publish_completed_event(session_id: &SessionId, is_created: bool) -> AppEvent {
    AppEvent::BranchPublishActionCompleted {
        result: Box::new(Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session".to_string(),
            is_created,
            review_request: review_request(ForgeKind::GitHub, ReviewRequestState::Open),
            upstream_reference: "origin/wt/session".to_string(),
        })),
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
async fn only_a_newly_created_review_request_offers_watching() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_review_session(&mut app, Status::Review, ReviewRequestState::Open).await;

    // Act
    app.apply_app_events(publish_completed_event(&session_id, false))
        .await;
    let refreshed_offers = app.review_watch.pending_offers.len();
    app.apply_app_events(publish_completed_event(&session_id, true))
        .await;

    // Assert
    assert_eq!(refreshed_offers, 0);
    assert_eq!(
        app.review_watch.pending_offers,
        std::collections::VecDeque::from([session_id])
    );
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
