use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ag_forge::{ForgeKind, MockReviewRequestClient, ReviewRequestSummary};
use ag_git::{GitClient, GitError, MockGitClient};
use tokio::sync::{mpsc, watch};

use super::{
    ProjectSyncContext, ProjectSyncPhase, ProjectSyncStatus, ReviewRequestPassUpdate,
    ReviewRequestSyncTarget, SessionGitStatusTarget, SyncContext, SyncHandle, SyncMainCompletion,
    SyncMainRequest, SyncOrchestrator, review_sync_backoff_passes, session_git_statuses,
    sync_review_request_status, sync_task_result_from_summary,
};
use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppEvent, session};
use crate::domain::agent::AgentModel;
use crate::domain::session::ReviewRequestState;

/// Builds one test review request summary for sync tests.
fn test_review_request_summary(
    display_id: &str,
    state: ReviewRequestState,
) -> ReviewRequestSummary {
    test_review_request_summary_with_forge(display_id, state, ForgeKind::GitHub)
}

/// Builds one test review request summary for sync tests with a
/// caller-specified forge family.
fn test_review_request_summary_with_forge(
    display_id: &str,
    state: ReviewRequestState,
    forge_kind: ForgeKind,
) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: display_id.to_string(),
        forge_kind,
        source_branch: "wt/session-id".to_string(),
        state,
        status_summary: None,
        target_branch: "main".to_string(),
        title: "feat".to_string(),
        web_url: String::new(),
    }
}

/// Builds one linked `ReviewRequest` fixture for sync tests.
fn linked_review_request(
    display_id: &str,
    state: ReviewRequestState,
) -> crate::domain::session::ReviewRequest {
    crate::domain::session::ReviewRequest {
        last_refreshed_at: 0,
        summary: test_review_request_summary(display_id, state),
    }
}

/// Builds one review-request sync target fixture.
fn review_request_sync_target(
    session_id: &str,
    linked: Option<crate::domain::session::ReviewRequest>,
) -> ReviewRequestSyncTarget {
    ReviewRequestSyncTarget {
        folder: PathBuf::from("/tmp/session-worktree"),
        linked_review_request: linked,
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        session_id: session_id.into(),
    }
}

/// Builds one minimal sync context fixture around mock clients.
fn sync_context_fixture(
    generation: u64,
    review_request_sync_targets: Vec<ReviewRequestSyncTarget>,
) -> SyncContext {
    SyncContext {
        generation,
        git_client: Arc::new(MockGitClient::new()),
        project_branch_name: Some("main".to_string()),
        project_id: 1,
        project_name: "agentty".to_string(),
        review_request_client: Arc::new(MockReviewRequestClient::new()),
        review_request_sync_targets,
        session_git_status_targets: Vec::new(),
        working_dir: PathBuf::from("/tmp/project"),
    }
}

/// Builds one explicit-sync operation fixture.
fn project_sync_context() -> ProjectSyncContext {
    ProjectSyncContext {
        default_branch: "main".to_string(),
        operation_id: 7,
        project_id: 1,
        project_name: "agentty".to_string(),
    }
}

#[test]
/// Only active phases hold the base-checkout operation guard.
fn project_sync_status_is_running_only_for_active_phases() {
    // Arrange
    let mut status = ProjectSyncStatus {
        context: project_sync_context(),
        phase: ProjectSyncPhase::Running,
    };

    // Act / Assert
    assert!(status.is_running());
    status.phase = ProjectSyncPhase::ResolvingConflicts {
        conflicted_file_count: 1,
    };
    assert!(status.is_running());
    status.phase = ProjectSyncPhase::Complete {
        deferred_session_count: 0,
        pulled_commits: Some(0),
        pushed_commits: Some(0),
        resolved_conflict_count: 0,
    };
    assert!(!status.is_running());
}

#[tokio::test]
/// The production runner forwards manual requests through the live
/// orchestrator queue and emits an operation-scoped terminal event.
async fn orchestrator_runner_forwards_manual_sync_request() {
    // Arrange
    let mut sync_context = sync_context_fixture(0, Vec::new());
    sync_context.project_branch_name = None;
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let sync_handle = SyncHandle::spawn(app_event_tx.clone(), sync_context.clone());
    let runner = sync_handle.sync_main_runner();
    let operation = project_sync_context();

    // Act
    runner.start_sync_main(
        app_event_tx,
        operation.clone(),
        AgentModel::Gemini38Flash,
        sync_context,
    );
    let event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
        .await
        .expect("manual sync should complete")
        .expect("orchestrator should emit a completion");

    // Assert
    assert!(matches!(
        event,
        AppEvent::SyncMainCompleted {
            completion: SyncMainCompletion {
                operation: completed_operation,
                result: Err(_),
                ..
            }
        } if completed_operation == operation
    ));
}

#[tokio::test]
/// Successful manual syncs preserve their captured review results and
/// emit them only with the matching project operation.
async fn run_sync_main_emits_captured_review_updates_after_success() {
    // Arrange
    let working_dir = PathBuf::from("/tmp/project");
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "git remote get-url origin".to_string(),
                stderr: "not a git repository".to_string(),
            })
        })
    });
    let repo_root = working_dir.clone();
    mock_git_client
        .expect_find_git_repo_root()
        .once()
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock_git_client
        .expect_is_worktree_clean()
        .once()
        .returning(|_| Box::pin(async { Ok(true) }));
    let mut ahead_behind_calls = 0_u8;
    mock_git_client
        .expect_get_ahead_behind()
        .times(2)
        .returning(move |_| {
            ahead_behind_calls = ahead_behind_calls.saturating_add(1);
            let status = if ahead_behind_calls == 1 {
                (1, 2)
            } else {
                (0, 0)
            };

            Box::pin(async move { Ok(status) })
        });
    mock_git_client
        .expect_list_upstream_commit_titles()
        .once()
        .returning(|_| Box::pin(async { Ok(vec!["remote fix".to_string()]) }));
    mock_git_client
        .expect_pull_rebase()
        .once()
        .returning(|_| Box::pin(async { Ok(ag_git::PullRebaseResult::Completed) }));
    mock_git_client
        .expect_list_local_commit_titles()
        .once()
        .returning(|_| Box::pin(async { Ok(vec!["local work".to_string()]) }));
    mock_git_client
        .expect_push_current_branch()
        .once()
        .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
    let git_client: Arc<dyn GitClient> = Arc::new(mock_git_client);
    let sync_context = SyncContext {
        git_client: Arc::clone(&git_client),
        review_request_sync_targets: vec![review_request_sync_target("session-1", None)],
        working_dir,
        ..sync_context_fixture(0, Vec::new())
    };
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let (_context_tx, context_rx) = watch::channel(sync_context.clone());
    let mut orchestrator = SyncOrchestrator {
        app_event_tx: app_event_tx.clone(),
        command_rx,
        context_rx,
        review_pass_index: 0,
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let operation = project_sync_context();

    // Act
    orchestrator
        .run_sync_main(SyncMainRequest {
            app_event_tx,
            operation: operation.clone(),
            session_model: AgentModel::Gemini38Flash,
            sync_context,
        })
        .await;
    let event = app_event_rx
        .recv()
        .await
        .expect("manual sync should emit a completion");

    // Assert
    assert!(matches!(
        event,
        AppEvent::SyncMainCompleted {
            completion: SyncMainCompletion {
                operation: completed_operation,
                result: Ok(_),
                review_request_updates,
            }
        } if completed_operation == operation
            && review_request_updates.len() == 1
            && review_request_updates[0].session_id.as_str() == "session-1"
            && review_request_updates[0].result.is_err()
    ));
}

#[test]
/// Each collected update keeps its session, generation, and result when
/// forwarded to the foreground event queue.
fn emit_review_request_pass_updates_preserves_batch_routing() {
    // Arrange
    let context = sync_context_fixture(7, Vec::new());
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let (_context_tx, context_rx) = watch::channel(context.clone());
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let orchestrator = SyncOrchestrator {
        app_event_tx,
        command_rx,
        context_rx,
        review_pass_index: 0,
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let updates = vec![
        ReviewRequestPassUpdate {
            result: Ok(SyncReviewRequestTaskResult {
                outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
                summary: None,
            }),
            target: review_request_sync_target("session-1", None),
        },
        ReviewRequestPassUpdate {
            result: Err("forge unavailable".to_string()),
            target: review_request_sync_target("session-2", None),
        },
    ];

    // Act
    orchestrator.emit_review_request_pass_updates(&context, updates);

    // Assert
    assert!(matches!(
        app_event_rx.try_recv().expect("first update should be emitted"),
        AppEvent::ReviewRequestStatusUpdated {
            generation: 7,
            result: Ok(SyncReviewRequestTaskResult {
                outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
                summary: None,
            }),
            session_id,
        } if session_id.as_str() == "session-1"
    ));
    assert!(matches!(
        app_event_rx.try_recv().expect("second update should be emitted"),
        AppEvent::ReviewRequestStatusUpdated {
            generation: 7,
            result: Err(error),
            session_id,
        } if session_id.as_str() == "session-2" && error == "forge unavailable"
    ));
    assert!(app_event_rx.try_recv().is_err());
}

#[test]
/// Same polling inputs keep the published generation stable while changed
/// inputs bump it, so in-flight results are only discarded when targets
/// actually moved.
fn publish_context_bumps_generation_only_on_changed_inputs() {
    // Arrange
    let (command_tx, _command_rx) = mpsc::unbounded_channel();
    let initial_context = sync_context_fixture(0, Vec::new());
    let (context_tx, context_rx) = watch::channel(initial_context);
    let sync_handle = SyncHandle::new(command_tx, context_tx);

    // Act
    sync_handle.publish_context(sync_context_fixture(0, Vec::new()));
    let unchanged_generation = context_rx.borrow().generation;
    sync_handle.publish_context(sync_context_fixture(
        0,
        vec![review_request_sync_target("session-1", None)],
    ));
    let changed_generation = context_rx.borrow().generation;

    // Assert
    assert_eq!(unchanged_generation, 0);
    assert_eq!(changed_generation, 1);
}

#[test]
/// Refresh-timestamp churn on a linked review request must not register
/// as a polling-input change, otherwise every successful refresh would
/// invalidate in-flight results of the same pass.
fn same_polling_inputs_ignores_linked_refresh_timestamp() {
    // Arrange
    let mut earlier_linked = linked_review_request("#42", ReviewRequestState::Open);
    earlier_linked.last_refreshed_at = 100;
    let mut later_linked = linked_review_request("#42", ReviewRequestState::Open);
    later_linked.last_refreshed_at = 200;
    let earlier_context = sync_context_fixture(
        0,
        vec![review_request_sync_target(
            "session-1",
            Some(earlier_linked),
        )],
    );
    let later_context = sync_context_fixture(
        0,
        vec![review_request_sync_target("session-1", Some(later_linked))],
    );

    // Act
    let same_inputs = earlier_context.same_polling_inputs(&later_context);

    // Assert
    assert!(same_inputs);
}

#[test]
/// A linked review request changing state is a polling-input change so
/// the generation bumps and stale results are discarded.
fn same_polling_inputs_detects_linked_state_change() {
    // Arrange
    let open_context = sync_context_fixture(
        0,
        vec![review_request_sync_target(
            "session-1",
            Some(linked_review_request("#42", ReviewRequestState::Open)),
        )],
    );
    let merged_context = sync_context_fixture(
        0,
        vec![review_request_sync_target(
            "session-1",
            Some(linked_review_request("#42", ReviewRequestState::Merged)),
        )],
    );

    // Act
    let same_inputs = open_context.same_polling_inputs(&merged_context);

    // Assert
    assert!(!same_inputs);
}

#[test]
/// Backoff windows grow exponentially with consecutive failures and stay
/// capped so failing targets are retried within a bounded interval.
fn review_sync_backoff_passes_grows_exponentially_and_caps() {
    // Arrange / Act / Assert
    assert_eq!(review_sync_backoff_passes(1), 0);
    assert_eq!(review_sync_backoff_passes(2), 1);
    assert_eq!(review_sync_backoff_passes(3), 3);
    assert_eq!(review_sync_backoff_passes(4), 7);
    assert_eq!(review_sync_backoff_passes(50), 7);
}

#[tokio::test]
/// Repeated failures back the target off and surface one workflow notice
/// at the threshold instead of retrying silently at full rate.
async fn record_review_sync_outcome_backs_off_and_notifies_after_threshold() {
    // Arrange
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let (_context_tx, context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let mut orchestrator = SyncOrchestrator {
        app_event_tx,
        command_rx,
        context_rx,
        review_pass_index: 0,
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let target = review_request_sync_target("session-1", None);
    let failure: Result<SyncReviewRequestTaskResult, String> = Err("gh exploded".to_string());

    // Act
    orchestrator.record_review_sync_outcome(&target, &failure, 0);
    let retryable_next_pass = !orchestrator.is_target_backed_off(&target.session_id, 1);
    orchestrator.record_review_sync_outcome(&target, &failure, 1);
    orchestrator.record_review_sync_outcome(&target, &failure, 3);

    // Assert
    assert!(retryable_next_pass);
    assert!(orchestrator.is_target_backed_off(&target.session_id, 6));
    assert!(!orchestrator.is_target_backed_off(&target.session_id, 7));
    let notice_event = app_event_rx
        .try_recv()
        .expect("third consecutive failure should surface one notice");
    assert!(matches!(
        notice_event,
        AppEvent::SessionWorkflowNoticeUpdated { ref notice, ref session_id }
            if session_id.as_str() == "session-1"
                && notice.contains("3 consecutive failures")
                && notice.contains("gh exploded")
    ));
    assert!(
        app_event_rx.try_recv().is_err(),
        "only the threshold failure should emit a notice"
    );
}

#[tokio::test]
/// One success clears the failure state so the target is polled at the
/// normal cadence again.
async fn record_review_sync_outcome_success_resets_backoff() {
    // Arrange
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let (_context_tx, context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let mut orchestrator = SyncOrchestrator {
        app_event_tx,
        command_rx,
        context_rx,
        review_pass_index: 0,
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let target = review_request_sync_target("session-1", None);
    let failure: Result<SyncReviewRequestTaskResult, String> = Err("gh exploded".to_string());
    let success = Ok(SyncReviewRequestTaskResult {
        outcome: session::SyncReviewRequestOutcome::NoReviewRequest,
        summary: None,
    });

    // Act
    orchestrator.record_review_sync_outcome(&target, &failure, 0);
    orchestrator.record_review_sync_outcome(&target, &failure, 1);
    orchestrator.record_review_sync_outcome(&target, &success, 4);

    // Assert
    assert!(!orchestrator.is_target_backed_off(&target.session_id, 5));
    assert!(orchestrator.review_sync_failures.is_empty());
}

#[tokio::test]
/// Verifies session git-status selection maps branch comparisons back to
/// session ids.
async fn session_git_statuses_collects_all_target_statuses() {
    // Arrange
    let repo_root = Path::new("/tmp/sync-session-statuses");
    let branch_tracking_statuses = HashMap::from([
        ("wt/session-a".to_string(), Some((7, 0))),
        ("wt/session-b".to_string(), Some((0, 4))),
    ]);
    let session_git_status_targets = vec![
        SessionGitStatusTarget {
            base_branch: "main".to_string(),
            branch_name: "wt/session-a".to_string(),
            session_id: "session-a".into(),
        },
        SessionGitStatusTarget {
            base_branch: "develop".to_string(),
            branch_name: "wt/session-b".to_string(),
            session_id: "session-b".into(),
        },
    ];
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(2)
        .returning(|_, left_ref, right_ref| {
            Box::pin(async move {
                match (left_ref.as_str(), right_ref.as_str()) {
                    ("wt/session-a", "main") => Ok((2, 1)),
                    ("wt/session-b", "develop") => Ok((0, 0)),
                    _ => Err(GitError::OutputParse("unexpected ref pair".to_string())),
                }
            })
        });
    mock_git_client
        .expect_has_merge_conflicts()
        .once()
        .withf(|repo_path, source_branch, target_branch| {
            repo_path == Path::new("/tmp/sync-session-statuses")
                && source_branch == "wt/session-a"
                && target_branch == "main"
        })
        .returning(|_, _, _| Box::pin(async { Ok(true) }));

    // Act
    let statuses = session_git_statuses(
        &branch_tracking_statuses,
        repo_root,
        &session_git_status_targets,
        &mock_git_client,
    )
    .await;

    // Assert
    assert_eq!(
        statuses.get("session-a"),
        Some(&SessionGitStatus {
            base_status: Some((2, 1)),
            has_merge_conflict: Some(true),
            remote_status: Some((7, 0)),
        })
    );
    assert_eq!(
        statuses.get("session-b"),
        Some(&SessionGitStatus {
            base_status: Some((0, 0)),
            has_merge_conflict: Some(false),
            remote_status: Some((0, 4)),
        })
    );
}

#[tokio::test]
/// Verifies session branches without tracked status degrade to `None`
/// without affecting the rest of the snapshot.
async fn session_git_statuses_keeps_failed_targets_as_none() {
    // Arrange
    let repo_root = Path::new("/tmp/sync-session-statuses-error");
    let branch_tracking_statuses = HashMap::new();
    let session_git_status_targets = vec![SessionGitStatusTarget {
        base_branch: "main".to_string(),
        branch_name: "wt/session-a".to_string(),
        session_id: "session-a".into(),
    }];
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_get_ref_ahead_behind()
        .once()
        .returning(|_, _, _| {
            Box::pin(async {
                Err(GitError::OutputParse(
                    "failed to compare session branch".to_string(),
                ))
            })
        });

    // Act
    let statuses = session_git_statuses(
        &branch_tracking_statuses,
        repo_root,
        &session_git_status_targets,
        &mock_git_client,
    )
    .await;

    // Assert
    assert_eq!(
        statuses.get("session-a"),
        Some(&SessionGitStatus {
            base_status: None,
            has_merge_conflict: None,
            remote_status: None,
        })
    );
}

#[tokio::test]
/// Verifies a failed conflict probe stays unknown without dropping valid
/// ahead/behind information.
async fn session_git_statuses_keeps_failed_conflict_probe_unknown() {
    // Arrange
    let repo_root = Path::new("/tmp/sync-session-conflict-error");
    let session_git_status_targets = vec![SessionGitStatusTarget {
        base_branch: "main".to_string(),
        branch_name: "wt/session-a".to_string(),
        session_id: "session-a".into(),
    }];
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_get_ref_ahead_behind()
        .once()
        .returning(|_, _, _| Box::pin(async { Ok((1, 2)) }));
    mock_git_client
        .expect_has_merge_conflicts()
        .once()
        .returning(|_, _, _| {
            Box::pin(async {
                Err(GitError::OutputParse(
                    "failed to compute merge tree".to_string(),
                ))
            })
        });

    // Act
    let statuses = session_git_statuses(
        &HashMap::new(),
        repo_root,
        &session_git_status_targets,
        &mock_git_client,
    )
    .await;

    // Assert
    assert_eq!(
        statuses.get("session-a"),
        Some(&SessionGitStatus {
            base_status: Some((1, 2)),
            has_merge_conflict: None,
            remote_status: None,
        })
    );
}

#[test]
/// Verifies open summaries keep their forge status text in the sync
/// outcome.
fn sync_task_result_from_open_summary_maps_open_outcome() {
    // Arrange
    let mut summary = test_review_request_summary("#42", ReviewRequestState::Open);
    summary.status_summary = Some("Checks passing".to_string());

    // Act
    let result = sync_task_result_from_summary(summary, None);

    // Assert
    assert_eq!(
        result.outcome,
        session::SyncReviewRequestOutcome::Open {
            display_id: "#42".to_string(),
            status_summary: Some("Checks passing".to_string()),
        }
    );
    assert!(result.summary.is_some());
}

#[tokio::test]
/// Verifies refresh requests inherit the session worktree directory when
/// building the forge remote.
async fn sync_review_request_status_attaches_worktree_to_detected_remote() {
    // Arrange
    let folder = PathBuf::from("/tmp/session-worktree");
    let linked = crate::domain::session::ReviewRequest {
        last_refreshed_at: 42,
        summary: test_review_request_summary("#42", ReviewRequestState::Open),
    };
    let expected_remote = ag_forge::ForgeRemote {
        command_working_directory: Some(folder.clone()),
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    };
    let expected_summary = test_review_request_summary("#42", ReviewRequestState::Merged);
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_repo_url()
        .once()
        .withf({
            let folder = folder.clone();
            move |candidate_folder| candidate_folder == &folder
        })
        .returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
        });
    mock_git_client
        .expect_head_hash()
        .once()
        .returning(|_| Box::pin(async { Ok("abc1234def5678".to_string()) }));
    let mut mock_review_request_client = MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty.git")
        .returning(|_| {
            Ok(ag_forge::ForgeRemote {
                command_working_directory: None,
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
                namespace: "agentty-xyz".to_string(),
                project: "agentty".to_string(),
                repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty".to_string(),
            })
        });
    mock_review_request_client
        .expect_refresh_review_request()
        .once()
        .withf({
            let expected_remote = expected_remote.clone();
            move |candidate_remote, display_id| {
                candidate_remote == &expected_remote && display_id == "#42"
            }
        })
        .returning({
            let expected_summary = expected_summary.clone();
            move |_, _| {
                let expected_summary = expected_summary.clone();

                Box::pin(async move { Ok(expected_summary) })
            }
        });

    // Act
    let result = sync_review_request_status(
        folder,
        &mock_git_client,
        Some(linked),
        None,
        &mock_review_request_client,
    )
    .await
    .expect("sync should succeed");

    // Assert
    assert_eq!(
        result.outcome,
        session::SyncReviewRequestOutcome::Merged {
            display_id: "#42".to_string(),
            session_head_hash: Some("abc1234def5678".to_string()),
        }
    );
}

#[tokio::test]
/// Verifies linked review-request sync can still observe terminal PR state
/// after the session worktree remote can no longer be resolved.
async fn sync_review_request_status_falls_back_to_linked_web_url() {
    // Arrange
    let folder = PathBuf::from("/tmp/missing-session-worktree");
    let linked = crate::domain::session::ReviewRequest {
        last_refreshed_at: 42,
        summary: ReviewRequestSummary {
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
        },
    };
    let expected_remote = ag_forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    };
    let expected_summary = test_review_request_summary("#42", ReviewRequestState::Merged);
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_repo_url()
        .once()
        .withf({
            let folder = folder.clone();
            move |candidate_folder| candidate_folder == &folder
        })
        .returning(|_| {
            Box::pin(async {
                Err(GitError::CommandFailed {
                    command: "git remote get-url origin".to_string(),
                    stderr: "not a git repository".to_string(),
                })
            })
        });
    mock_git_client.expect_head_hash().once().returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "git rev-parse HEAD".to_string(),
                stderr: "not a git repository".to_string(),
            })
        })
    });
    let mut mock_review_request_client = MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty")
        .returning({
            let expected_remote = expected_remote.clone();
            move |_| Ok(expected_remote.clone())
        });
    mock_review_request_client
        .expect_refresh_review_request()
        .once()
        .withf({
            let expected_remote = expected_remote.clone();
            move |candidate_remote, display_id| {
                candidate_remote == &expected_remote && display_id == "#42"
            }
        })
        .returning({
            let expected_summary = expected_summary.clone();
            move |_, _| {
                let expected_summary = expected_summary.clone();

                Box::pin(async move { Ok(expected_summary) })
            }
        });

    // Act
    let result = sync_review_request_status(
        folder,
        &mock_git_client,
        Some(linked),
        None,
        &mock_review_request_client,
    )
    .await
    .expect("sync should use the linked review-request URL fallback");

    // Assert
    assert_eq!(
        result.outcome,
        session::SyncReviewRequestOutcome::Merged {
            display_id: "#42".to_string(),
            session_head_hash: None,
        }
    );
}

#[test]
/// Verifies merged summaries map to the merged sync outcome.
fn sync_task_result_from_merged_summary_maps_merged_outcome() {
    // Arrange
    let summary = test_review_request_summary("#99", ReviewRequestState::Merged);
    let session_head_hash = Some("9f00d1".to_string());

    // Act
    let result = sync_task_result_from_summary(summary, session_head_hash.clone());

    // Assert
    assert_eq!(
        result.outcome,
        session::SyncReviewRequestOutcome::Merged {
            display_id: "#99".to_string(),
            session_head_hash
        }
    );
    assert!(result.summary.is_some());
}

#[test]
/// Verifies closed summaries map to the canceled-session sync outcome.
fn sync_task_result_from_closed_summary_maps_closed_outcome() {
    // Arrange
    let summary = test_review_request_summary("#7", ReviewRequestState::Closed);

    // Act
    let result = sync_task_result_from_summary(summary, None);

    // Assert
    assert_eq!(
        result.outcome,
        session::SyncReviewRequestOutcome::Closed {
            display_id: "#7".to_string(),
        }
    );
    assert!(result.summary.is_some());
}
