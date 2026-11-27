//! Pull-request merge tests: admission, the background merge against a
//! mocked forge, and how its outcome reaches the transcript and the session
//! status.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_forge::{
    ForgeRemote, MergeReviewRequestInput, MockReviewRequestClient, ReviewRequestError,
    ReviewRequestMergeMode, ReviewRequestMergeOutcome,
};
use ag_session::{ReviewRequestMerge, SessionError};
use tokio::sync::oneshot;

use super::{
    BRANCH_OPERATION_RUNNING, BYPASS_NOT_NEEDED, BYPASS_REQUIRED, ReviewRequestMergeCompletion,
};
use crate::app::App;
use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::session::SyncReviewRequestOutcome;
use crate::domain::session::{
    ForgeKind, ReviewRequest, ReviewRequestMergeBlocker, ReviewRequestState, ReviewRequestSummary,
    Session, SessionId, Status,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transient_message::{TransientMessageBody, TransientMessageSlot};
use crate::test_support::{new_git_test_app, set_session_status_for_test};

/// Status a ready pull request reports.
const READY: &str = "Approved, Mergeable, PR ready";

/// Status a pull request that merges only by bypass reports.
const BYPASS_READY: &str = "Review required, Blocked, PR ready with bypass";

/// Merge commit the mocked forge reports.
const MERGE_COMMIT: &str = "fedcba9876543210fedcba9876543210fedcba98";

/// Builds pull request `#42` into `main` in `state` with `status_summary`.
fn pull_request(state: ReviewRequestState, status_summary: &str) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: "#42".to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session".to_string(),
        state,
        status_summary: Some(status_summary.to_string()),
        target_branch: "main".to_string(),
        title: "Merge it".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
    }
}

/// Creates one session resting in `Review` whose linked pull request
/// reports `status_summary`.
async fn seed_session(app: &mut App, status_summary: &str) -> SessionId {
    let [session_id] = seed_sessions(app, [status_summary]).await;

    session_id
}

/// Creates one session per status, each resting in `Review` with a linked
/// pull request that reports it.
///
/// Every session is created before any is linked, since creating one
/// reloads the in-memory sessions.
async fn seed_sessions<const COUNT: usize>(
    app: &mut App,
    status_summaries: [&str; COUNT],
) -> [SessionId; COUNT] {
    let mut session_ids = Vec::with_capacity(COUNT);
    for _ in status_summaries {
        session_ids.push(SessionId::from(
            app.create_session().await.expect("session"),
        ));
    }
    for (session_id, status_summary) in session_ids.iter().zip(status_summaries) {
        set_session_status_for_test(app, session_id, Status::Review);
        app.sessions.apply_review_request(
            session_id,
            ReviewRequest {
                last_refreshed_at: 0,
                summary: pull_request(ReviewRequestState::Open, status_summary),
            },
        );
    }

    session_ids.try_into().expect("one session per status")
}

/// Returns the GitHub remote of the mocked forge.
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

/// Builds a forge that resolves the remote, records each merge input, merges
/// with `outcome`, and refreshes the pull request as `refreshed`.
fn forge(
    outcome: Result<ReviewRequestMergeOutcome, ReviewRequestError>,
    refreshed: Result<ReviewRequestSummary, ReviewRequestError>,
    inputs: &Arc<Mutex<Vec<MergeReviewRequestInput>>>,
) -> MockReviewRequestClient {
    let mut client = MockReviewRequestClient::new();
    client
        .expect_detect_remote()
        .returning(|_| Ok(github_remote()));
    let inputs = Arc::clone(inputs);
    client
        .expect_merge_review_request()
        .once()
        .returning(move |_, input| {
            inputs.lock().expect("inputs").push(input);
            let outcome = outcome.clone();

            Box::pin(async move { outcome })
        });
    client
        .expect_refresh_review_request()
        .once()
        .withf(|_, display_id| display_id == "#42")
        .returning(move |_, _| {
            let refreshed = refreshed.clone();

            Box::pin(async move { refreshed })
        });

    client
}

/// Returns the transcript text of one session's live handle.
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

/// Returns the loading label of the session's running merge, if any.
fn merge_loading_label(app: &App, session_id: &str) -> Option<String> {
    let message = app
        .sessions
        .session_for_id(session_id)?
        .transient_messages
        .get(TransientMessageSlot::ReviewRequestMerge)?;

    match &message.body {
        TransientMessageBody::Loading(label) => Some(label.clone()),
        _ => None,
    }
}

/// Applies queued app events for up to five seconds until the session's
/// merge finished, returning whether it did.
async fn wait_for_merge(app: &mut App, session_id: &str) -> bool {
    for _ in 0..500 {
        app.process_pending_app_events().await;
        if merge_loading_label(app, session_id).is_none() {
            return true;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    false
}

/// Returns the commit at the head of one session worktree.
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

#[tokio::test]
async fn ready_merge_merges_at_the_session_head_and_marks_the_session_merged() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let head = session_head(&app, &session_id).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Merged {
            merge_commit: Some(MERGE_COMMIT.to_string()),
        }),
        Ok(pull_request(ReviewRequestState::Merged, "Approved")),
        &inputs,
    )));
    let (reply_tx, reply_rx) = oneshot::channel();

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, Some(reply_tx))
        .await;
    let loading_label = merge_loading_label(&app, &session_id);
    let status_while_merging = handle_status(&app, &session_id);
    let availability_while_merging = app
        .sessions
        .session_for_id(&session_id)
        .map(Session::review_request_merge_availability);
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert_eq!(
        loading_label.as_deref(),
        Some("Merging PR #42 into `main`...")
    );
    // Waiting in `Queued` keeps replies, syncs, and follow-ups off the
    // branch until the merge settles.
    assert_eq!(status_while_merging, Status::Queued);
    assert_eq!(
        availability_while_merging,
        Some(Err(ReviewRequestMergeBlocker::Merging))
    );
    assert!(finished);
    assert_eq!(
        inputs.lock().expect("inputs").as_slice(),
        [MergeReviewRequestInput {
            display_id: "#42".to_string(),
            expected_head_commit: Some(head),
            mode: ReviewRequestMergeMode::Ready,
        }]
    );
    assert_eq!(
        reply_rx.await.expect("reply"),
        Ok(ReviewRequestMerge {
            outcome: ReviewRequestMergeOutcome::Merged {
                merge_commit: Some(MERGE_COMMIT.to_string()),
            },
            review_request: Some(pull_request(ReviewRequestState::Merged, "Approved")),
        })
    );
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Request] Merged PR #42 into `main` as `fedcba9`"),
        "{transcript}"
    );
    assert_eq!(handle_status(&app, &session_id), Status::Merged);
}

#[tokio::test]
async fn queued_merge_moves_the_session_to_queued() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Queued),
        Ok(pull_request(
            ReviewRequestState::Open,
            "Approved, Mergeable, In merge queue",
        )),
        &inputs,
    )));

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, None)
        .await;
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert!(finished);
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Request] Added PR #42 to the merge queue of `main`"),
        "{transcript}"
    );
    assert_eq!(handle_status(&app, &session_id), Status::Queued);
}

#[tokio::test]
async fn session_stays_queued_through_event_batches_until_the_merge_settles() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let (release_tx, release_rx) = oneshot::channel::<()>();
    let release_rx = Arc::new(Mutex::new(Some(release_rx)));
    let mut client = MockReviewRequestClient::new();
    client
        .expect_detect_remote()
        .returning(|_| Ok(github_remote()));
    client
        .expect_merge_review_request()
        .once()
        .returning(move |_, _| {
            let release_rx = release_rx.lock().expect("release").take();

            Box::pin(async move {
                if let Some(release_rx) = release_rx {
                    let _ = release_rx.await;
                }

                Ok(ReviewRequestMergeOutcome::Merged { merge_commit: None })
            })
        });
    client
        .expect_refresh_review_request()
        .once()
        .returning(|_, _| {
            Box::pin(async { Ok(pull_request(ReviewRequestState::Merged, "Approved")) })
        });
    app.services.set_review_request_client(Arc::new(client));

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, None)
        .await;
    for _ in 0..5 {
        app.process_pending_app_events().await;
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    app.reconcile_review_watch_statuses().await;
    let status_while_merging = handle_status(&app, &session_id);
    let _ = release_tx.send(());
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert_eq!(status_while_merging, Status::Queued);
    assert!(finished);
    assert_eq!(handle_status(&app, &session_id), Status::Merged);
}

#[tokio::test]
async fn bypass_merge_asks_the_forge_to_bypass_and_says_so_while_it_runs() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, BYPASS_READY).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Merged { merge_commit: None }),
        Ok(pull_request(ReviewRequestState::Merged, "Review required")),
        &inputs,
    )));

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), true, None)
        .await;
    let loading_label = merge_loading_label(&app, &session_id);
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert_eq!(
        loading_label.as_deref(),
        Some("Merging PR #42 into `main` by bypassing branch protections...")
    );
    assert!(finished);
    assert_eq!(
        inputs.lock().expect("inputs")[0].mode,
        ReviewRequestMergeMode::Bypass
    );
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Review Request] Merged PR #42 into `main`\n"),
        "{transcript}"
    );
}

#[tokio::test]
async fn forge_refusal_lands_in_the_transcript_and_keeps_the_session_in_review() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let reason = "Pull request #42 is no longer ready to merge (Approved, Checks pending)";
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Refused {
            reason: reason.to_string(),
        }),
        Ok(pull_request(
            ReviewRequestState::Open,
            "Approved, Checks pending",
        )),
        &inputs,
    )));
    let (reply_tx, reply_rx) = oneshot::channel();

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, Some(reply_tx))
        .await;
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert!(finished);
    assert!(matches!(
        reply_rx.await.expect("reply"),
        Ok(ReviewRequestMerge {
            outcome: ReviewRequestMergeOutcome::Refused { .. },
            review_request: Some(_),
        })
    ));
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains(&format!("[Merge Error] Did not merge PR #42: {reason}")),
        "{transcript}"
    );
    assert_eq!(handle_status(&app, &session_id), Status::Review);
    assert_eq!(
        app.sessions
            .session_for_id(&session_id)
            .and_then(|session| session.review_request.as_ref())
            .and_then(|review_request| review_request.summary.status_summary.as_deref()),
        Some("Approved, Checks pending")
    );
}

#[tokio::test]
async fn forge_failure_answers_the_caller_and_keeps_the_last_status_without_a_refresh() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, BYPASS_READY).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    let failure = ReviewRequestError::OperationFailed {
        forge_kind: ForgeKind::GitHub,
        message: "merge pull request: HTTP 502\nPull request #42 was marked ready for review and \
                  stays ready for review."
            .to_string(),
    };
    app.services.set_review_request_client(Arc::new(forge(
        Err(failure.clone()),
        Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "refresh pull request: HTTP 502".to_string(),
        }),
        &inputs,
    )));
    let (reply_tx, reply_rx) = oneshot::channel();

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), true, Some(reply_tx))
        .await;
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert!(finished);
    assert_eq!(
        reply_rx.await.expect("reply"),
        Err(SessionError::Operation(failure.detail_message()))
    );
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Merge Error] Could not merge PR #42: GitHub review-request"),
        "{transcript}"
    );
    assert!(
        transcript.contains("stays ready for review"),
        "{transcript}"
    );
    assert_eq!(handle_status(&app, &session_id), Status::Review);
}

#[tokio::test]
async fn unresolvable_remote_fails_before_asking_the_forge_to_merge() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let mut client = MockReviewRequestClient::new();
    client
        .expect_detect_remote()
        .returning(|repo_url| Err(ReviewRequestError::UnsupportedRemote { repo_url }));
    client.expect_merge_review_request().never();
    client.expect_refresh_review_request().never();
    app.services.set_review_request_client(Arc::new(client));

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, None)
        .await;
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert!(finished);
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains("[Merge Error] Could not merge PR #42: Review requests are only"),
        "{transcript}"
    );
    assert_eq!(handle_status(&app, &session_id), Status::Review);
}

#[tokio::test]
async fn unreadable_session_head_fails_without_merging_but_still_refreshes() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let folder = app
        .sessions
        .session_for_id(&session_id)
        .expect("session")
        .folder
        .clone();
    std::fs::remove_dir_all(&folder).expect("remove the session worktree");
    let mut client = MockReviewRequestClient::new();
    client
        .expect_detect_remote()
        .returning(|_| Ok(github_remote()));
    client.expect_merge_review_request().never();
    client
        .expect_refresh_review_request()
        .once()
        .returning(|_, _| Box::pin(async { Ok(pull_request(ReviewRequestState::Open, READY)) }));
    app.services.set_review_request_client(Arc::new(client));

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, None)
        .await;
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert!(finished);
    let transcript = transcript(&app, &session_id);
    assert!(
        transcript.contains(
            "[Merge Error] Could not merge PR #42: Could not read the session branch head"
        ),
        "{transcript}"
    );
}

#[tokio::test]
async fn merge_is_refused_unless_the_bypass_choice_matches_the_readiness() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let [ready_session_id, bypass_session_id] =
        seed_sessions(&mut app, [READY, BYPASS_READY]).await;
    let mut client = MockReviewRequestClient::new();
    client.expect_merge_review_request().never();
    app.services.set_review_request_client(Arc::new(client));

    // Act
    let bypass_not_needed = app
        .start_review_request_merge(ready_session_id.clone(), true, None)
        .await;
    let bypass_required = app
        .start_review_request_merge(bypass_session_id.clone(), false, None)
        .await;

    // Assert
    assert_eq!(
        bypass_not_needed,
        Err(SessionError::Operation(BYPASS_NOT_NEEDED.to_string()))
    );
    assert_eq!(
        bypass_required,
        Err(SessionError::Operation(BYPASS_REQUIRED.to_string()))
    );
    assert_eq!(merge_loading_label(&app, &ready_session_id), None);
    assert_eq!(merge_loading_label(&app, &bypass_session_id), None);
}

#[tokio::test]
async fn merge_is_refused_while_the_session_cannot_merge_now() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let [
        not_ready_session_id,
        busy_session_id,
        merging_session_id,
        locally_queued_session_id,
        locked_session_id,
    ] = seed_sessions(&mut app, ["Checks pending", READY, READY, READY, READY]).await;
    set_session_status_for_test(&mut app, &busy_session_id, Status::InProgress);
    app.sessions
        .start_review_request_merge(&merging_session_id, "Merging...".to_string());
    app.merge_queue.enqueue(locally_queued_session_id.clone());
    let _branch_operation_guard = app
        .sessions
        .session_handles_or_err(&locked_session_id)
        .expect("handles")
        .branch_operation_lock
        .clone()
        .try_lock_owned()
        .expect("lock");
    let mut client = MockReviewRequestClient::new();
    client.expect_merge_review_request().never();
    app.services.set_review_request_client(Arc::new(client));

    // Act
    let mut refusals = Vec::new();
    for session_id in [
        SessionId::from("missing-session"),
        not_ready_session_id,
        busy_session_id,
        merging_session_id,
        locally_queued_session_id,
        locked_session_id,
    ] {
        refusals.push(
            app.start_review_request_merge(session_id, false, None)
                .await,
        );
    }

    // Assert
    let busy = || {
        Err(SessionError::Operation(
            "The session has work in progress; merge its pull request once it finishes".to_string(),
        ))
    };
    assert_eq!(
        refusals,
        [
            Err(SessionError::NotFound),
            Err(SessionError::Operation(
                "The session's pull request is not ready to merge".to_string()
            )),
            busy(),
            Err(SessionError::Operation(
                "The session's pull request is already merging".to_string()
            )),
            busy(),
            Err(SessionError::Operation(
                BRANCH_OPERATION_RUNNING.to_string()
            )),
        ]
    );
}

#[tokio::test]
async fn refusal_answers_a_waiting_caller_on_its_reply() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let (reply_tx, reply_rx) = oneshot::channel();

    // Act
    let admission = app
        .start_review_request_merge(SessionId::from("missing-session"), false, Some(reply_tx))
        .await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert_eq!(reply_rx.await.expect("reply"), Err(SessionError::NotFound));
}

#[tokio::test]
async fn terminal_admission_reports_a_refusal_to_its_caller() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, "Checks pending").await;

    // Act
    let admission = app.admit_review_request_merge(session_id, false).await;

    // Assert
    assert_eq!(
        admission,
        Err(SessionError::Operation(
            "The session's pull request is not ready to merge".to_string()
        ))
    );
}

#[tokio::test]
async fn session_service_merges_and_answers_with_the_forge_outcome() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Queued),
        Ok(pull_request(ReviewRequestState::Open, "In merge queue")),
        &inputs,
    )));
    let service = app.session_service();
    let supports = service.supports(ag_session::SessionOperation::MergeReviewRequest);

    // Act
    let merged = app
        .drive_session_request(service.merge_review_request(&session_id, false))
        .await;

    // Assert
    assert!(supports);
    assert_eq!(
        merged,
        Ok(ReviewRequestMerge {
            outcome: ReviewRequestMergeOutcome::Queued,
            review_request: Some(pull_request(ReviewRequestState::Open, "In merge queue")),
        })
    );
}

#[tokio::test]
async fn refused_merge_releases_a_session_whose_page_is_not_shown() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Refused {
            reason: "Pull request #42 is closed".to_string(),
        }),
        Ok(pull_request(ReviewRequestState::Open, READY)),
        &inputs,
    )));
    let (reply_tx, reply_rx) = oneshot::channel();

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, Some(reply_tx))
        .await;
    // The terminal shows another project's page while the merge runs.
    app.sessions.state_mut().replace_sessions(Vec::new());
    let reply = reply_rx.await.expect("reply");
    app.process_pending_app_events().await;

    // Assert
    assert_eq!(admission, Ok(()));
    assert!(matches!(
        reply,
        Ok(ReviewRequestMerge {
            outcome: ReviewRequestMergeOutcome::Refused { .. },
            ..
        })
    ));
    assert_eq!(handle_status(&app, &session_id), Status::Review);
    assert!(app.review_request_merges.is_empty());
}

#[tokio::test]
async fn refused_merge_of_a_watched_session_returns_it_to_watching() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    app.enable_review_watch(&session_id).await;
    let inputs = Arc::new(Mutex::new(Vec::new()));
    app.services.set_review_request_client(Arc::new(forge(
        Ok(ReviewRequestMergeOutcome::Refused {
            reason: "Pull request #42 is closed".to_string(),
        }),
        Err(ReviewRequestError::OperationFailed {
            forge_kind: ForgeKind::GitHub,
            message: "refresh pull request: HTTP 502".to_string(),
        }),
        &inputs,
    )));
    let status_before = handle_status(&app, &session_id);

    // Act
    let admission = app
        .start_review_request_merge(session_id.clone(), false, None)
        .await;
    let finished = wait_for_merge(&mut app, &session_id).await;

    // Assert
    assert_eq!(status_before, Status::Watching);
    assert_eq!(admission, Ok(()));
    assert!(finished);
    assert_eq!(handle_status(&app, &session_id), Status::Watching);
}

#[test]
fn only_a_queued_or_unconfirmed_merge_leaves_the_session_queued() {
    // Arrange
    let refreshed = |status_summary: &str| SyncReviewRequestTaskResult {
        attention: None,
        outcome: SyncReviewRequestOutcome::Open {
            display_id: "#42".to_string(),
            status_summary: Some(status_summary.to_string()),
        },
        summary: Some(pull_request(ReviewRequestState::Open, status_summary)),
    };
    let completion = |outcome, refreshed| ReviewRequestMergeCompletion {
        display_id: "#42".to_string(),
        outcome,
        refreshed,
        short_name: "PR",
        target_branch: "main".to_string(),
    };
    let completions = [
        completion(
            Ok(ReviewRequestMergeOutcome::Queued),
            Some(refreshed("In merge queue")),
        ),
        completion(
            Ok(ReviewRequestMergeOutcome::Queued),
            Some(refreshed(READY)),
        ),
        completion(Ok(ReviewRequestMergeOutcome::Queued), None),
        completion(
            Ok(ReviewRequestMergeOutcome::Merged { merge_commit: None }),
            None,
        ),
        completion(
            Ok(ReviewRequestMergeOutcome::Refused {
                reason: "closed".to_string(),
            }),
            None,
        ),
        completion(Err("HTTP 502".to_string()), None),
    ];

    // Act
    let stays_queued = completions
        .iter()
        .map(ReviewRequestMergeCompletion::leaves_session_queued)
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(stays_queued, [true, false, true, true, false, false]);
}

#[tokio::test]
async fn completion_for_a_session_that_is_no_longer_loaded_changes_nothing() {
    // Arrange
    let (mut app, _base_dir) = new_git_test_app().await;
    let session_id = seed_session(&mut app, READY).await;
    let completion = ReviewRequestMergeCompletion {
        display_id: "#42".to_string(),
        outcome: Err("HTTP 502".to_string()),
        refreshed: None,
        short_name: "PR",
        target_branch: "main".to_string(),
    };

    // Act
    app.apply_review_request_merge_completion(SessionId::from("missing-session"), completion)
        .await;

    // Assert
    assert_eq!(handle_status(&app, &session_id), Status::Review);
    assert!(!transcript(&app, &session_id).contains("[Merge Error]"));
}

#[test]
fn completion_notices_name_the_pull_request_and_its_outcome() {
    // Arrange
    let completion = |outcome| ReviewRequestMergeCompletion {
        display_id: "#42".to_string(),
        outcome,
        refreshed: None,
        short_name: "PR",
        target_branch: "main".to_string(),
    };
    let outcomes = [
        Ok(ReviewRequestMergeOutcome::Merged {
            merge_commit: Some("abc".to_string()),
        }),
        Ok(ReviewRequestMergeOutcome::Merged { merge_commit: None }),
        Ok(ReviewRequestMergeOutcome::Queued),
        Ok(ReviewRequestMergeOutcome::Refused {
            reason: "Pull request #42 is closed".to_string(),
        }),
        Err("HTTP 502".to_string()),
    ];

    // Act
    let notices = outcomes.map(|outcome| completion(outcome).notice());

    // Assert
    assert_eq!(
        notices,
        [
            "\n[Review Request] Merged PR #42 into `main` as `abc`\n",
            "\n[Review Request] Merged PR #42 into `main`\n",
            "\n[Review Request] Added PR #42 to the merge queue of `main`\n",
            "\n[Merge Error] Did not merge PR #42: Pull request #42 is closed\n",
            "\n[Merge Error] Could not merge PR #42: HTTP 502\n",
        ]
        .map(str::to_string)
    );
}
