//! Background sync tests for review-request status polling, backoff, and
//! session git status collection.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ag_forge::{ForgeKind, MockReviewRequestClient, ReviewRequestSummary};
use ag_git::{BranchTrackingStatus, GitClient, GitError, MockGitClient};
use ag_telemetry::KeyValue;
use mockall::Sequence;
use opentelemetry::global;
use opentelemetry::trace::SpanId;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider, SpanData};
use tokio::sync::{mpsc, watch};

use super::{
    BaseComparisonCache, CommitPair, FETCH_MAX_BACKOFF_PASSES, FailureBackoff, ProjectSyncContext,
    ProjectSyncPhase, ProjectSyncStatus, REVIEW_SYNC_MAX_BACKOFF_PASSES, ReviewPass,
    ReviewRequestPassUpdate, ReviewRequestSyncTarget, SessionGitStatusTarget, SyncContext,
    SyncHandle, SyncMainCompletion, SyncMainRequest, SyncOrchestrator, review_request_repo_url,
    sync_review_request_status, sync_task_result_from_summary,
};
use crate::app::core::SyncReviewRequestTaskResult;
use crate::app::review_watch_config::ReviewWatchConfig;
use crate::app::session_state::SessionGitStatus;
use crate::app::{AppEvent, session};
use crate::domain::agent::AgentModel;
use crate::domain::session::ReviewRequestState;
use crate::infra::fs::MockFsClient;
use crate::test_support::telemetry::TRACER_PROVIDER_LOCK;

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
        is_watched: false,
        linked_review_request: linked,
        published_upstream_ref: Some("origin/wt/session-id".to_string()),
        session_id: session_id.into(),
    }
}

/// Builds one branch-tracking entry for `branch_name` pointing at
/// `commit_hash`.
fn tracked_branch(
    branch_name: &str,
    commit_hash: &str,
    upstream_status: Option<(u32, u32)>,
) -> (String, BranchTrackingStatus) {
    (
        branch_name.to_string(),
        BranchTrackingStatus {
            commit_hash: commit_hash.to_string(),
            upstream_status,
        },
    )
}

/// Builds one git-status target comparing `wt/<session_id>` with `main`.
fn session_git_status_target(session_id: &str) -> SessionGitStatusTarget {
    SessionGitStatusTarget {
        base_branch: "main".to_string(),
        branch_name: format!("wt/{session_id}"),
        session_id: session_id.into(),
    }
}

/// Builds one minimal sync context fixture around mock clients.
fn sync_context_fixture(
    generation: u64,
    review_request_sync_targets: Vec<ReviewRequestSyncTarget>,
) -> SyncContext {
    SyncContext {
        run_client: Arc::new(ag_worker::MockRunClient::new()),
        generation,
        fs_client: Arc::new(MockFsClient::new()),
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

/// Installs an in-memory exporter as the process-global trace provider.
/// Callers hold [`TRACER_PROVIDER_LOCK`] while it is installed.
fn install_span_exporter() -> InMemorySpanExporter {
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );

    exporter
}

/// Returns the only finished span named `name`.
fn finished_span(exporter: &InMemorySpanExporter, name: &str) -> SpanData {
    let spans = exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .filter(|span| span.name == name)
        .collect::<Vec<_>>();
    assert_eq!(spans.len(), 1, "expected one `{name}` span: {spans:?}");

    spans.into_iter().next().expect("one span")
}

/// Asserts `span` is a child of `parent` and records `outcome` and every
/// expected attribute.
fn assert_phase(span: &SpanData, parent: &SpanData, outcome: &str, expected: &[KeyValue]) {
    assert_eq!(span.parent_span_id, parent.span_context.span_id());
    assert!(
        span.attributes
            .contains(&KeyValue::new("agentty.outcome", outcome.to_string())),
        "`{}` outcome: {:?}",
        span.name,
        span.attributes
    );
    for attribute in expected {
        assert!(
            span.attributes.contains(attribute),
            "`{}` lacks {attribute:?}: {:?}",
            span.name,
            span.attributes
        );
    }
}

/// Builds an orchestrator polling contexts from `context_rx`, returning it
/// with the app event receiver.
fn orchestrator_for(
    context_rx: watch::Receiver<SyncContext>,
) -> (SyncOrchestrator, mpsc::UnboundedReceiver<AppEvent>) {
    let (app_event_tx, app_event_rx) = mpsc::unbounded_channel();
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let orchestrator = SyncOrchestrator {
        app_event_tx,
        base_comparison_cache: BaseComparisonCache::default(),
        command_rx,
        context_rx,
        fetch_failures: HashMap::new(),
        fetch_pass_index: 0,
        review_pass_index: 0,
        review_sync_cadences: HashMap::new(),
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };

    (orchestrator, app_event_rx)
}

#[tokio::test]
/// Failed fetch and tracking queries keep the best-effort snapshot but end
/// their phases and the refresh trace as failed, classifying the fetch
/// failure without exporting its text.
async fn refresh_pass_traces_failed_fetch_and_tracking_status() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_span_exporter();
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .once()
        .returning(|_| Box::pin(async { None }));
    mock_git_client.expect_fetch_remote().once().returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "git fetch".to_string(),
                stderr: "git@private.example: Permission denied (publickey).".to_string(),
            })
        })
    });
    mock_git_client
        .expect_branch_tracking_statuses()
        .once()
        .returning(|_| {
            Box::pin(async {
                Err(GitError::CommandFailed {
                    command: "git for-each-ref".to_string(),
                    stderr: "private ref failure".to_string(),
                })
            })
        });
    let context = SyncContext {
        git_client: Arc::new(mock_git_client),
        ..sync_context_fixture(2, Vec::new())
    };
    let (_context_tx, context_rx) = watch::channel(context);
    let (mut orchestrator, mut app_event_rx) = orchestrator_for(context_rx);

    // Act
    orchestrator.run_refresh_pass(ReviewPass::Skip).await;

    // Assert
    assert!(matches!(
        app_event_rx.try_recv().expect("snapshot despite failures"),
        AppEvent::GitStatusUpdated {
            generation: 2,
            status: None,
            ..
        }
    ));
    let refresh = finished_span(&exporter, "sync.refresh");
    assert_eq!(refresh.parent_span_id, SpanId::INVALID);
    assert!(
        refresh
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "failed"))
    );
    assert!(
        refresh
            .attributes
            .contains(&KeyValue::new("agentty.sync.review_pass", false))
    );
    assert_phase(
        &finished_span(&exporter, "git.fetch"),
        &refresh,
        "failed",
        &[
            KeyValue::new("agentty.sync.consecutive_failures", 1_i64),
            KeyValue::new("error.type", "auth"),
        ],
    );
    assert_phase(
        &finished_span(&exporter, "sync.branch_status"),
        &refresh,
        "failed",
        &[KeyValue::new("agentty.sync.targets", 0_i64)],
    );
    assert!(!format!("{:?}", exporter.get_finished_spans()).contains("private"));
}

#[tokio::test]
/// A pass superseded by newer polling targets emits nothing and ends its
/// refresh trace as canceled.
async fn superseded_refresh_pass_is_traced_as_canceled() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_span_exporter();
    let (context_tx, _context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
    let context_tx = Arc::new(context_tx);
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .once()
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_fetch_remote()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .once()
        .returning({
            let context_tx = Arc::clone(&context_tx);
            move |_| {
                context_tx.send_modify(|context| context.generation = 1);

                Box::pin(async { Ok(HashMap::new()) })
            }
        });
    let context = SyncContext {
        git_client: Arc::new(mock_git_client),
        ..sync_context_fixture(0, Vec::new())
    };
    context_tx.send_replace(context);
    let (mut orchestrator, mut app_event_rx) = orchestrator_for(context_tx.subscribe());

    // Act
    orchestrator.run_refresh_pass(ReviewPass::Skip).await;

    // Assert
    assert!(app_event_rx.try_recv().is_err());
    let refresh = finished_span(&exporter, "sync.refresh");
    assert!(
        refresh
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "canceled"))
    );
    assert_phase(
        &finished_span(&exporter, "git.fetch"),
        &refresh,
        "completed",
        &[],
    );
    assert_phase(
        &finished_span(&exporter, "sync.branch_status"),
        &refresh,
        "completed",
        &[],
    );
}

#[tokio::test]
/// Consecutive fetch failures skip later fetches while still emitting the
/// branch snapshot, and one success clears only that project's backoff.
async fn refresh_pass_backs_off_failing_fetches_until_one_succeeds() {
    // Arrange
    crate::test_support::enable_tracing_log_fallback();
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_span_exporter();
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(5)
        .returning(|_| Box::pin(async { None }));
    let mut fetch_attempts = 0_u8;
    mock_git_client
        .expect_fetch_remote()
        .times(4)
        .returning(move |_| {
            fetch_attempts = fetch_attempts.saturating_add(1);
            let result = if fetch_attempts <= 2 {
                Err(GitError::CommandFailed {
                    command: "git fetch".to_string(),
                    stderr: "ssh: Could not resolve hostname private.example".to_string(),
                })
            } else {
                Ok(())
            };

            Box::pin(async move { result })
        });
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(5)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    let context = SyncContext {
        git_client: Arc::new(mock_git_client),
        ..sync_context_fixture(0, Vec::new())
    };
    let (_context_tx, context_rx) = watch::channel(context);
    let (mut orchestrator, mut app_event_rx) = orchestrator_for(context_rx);
    orchestrator.fetch_failures.insert(
        2,
        FailureBackoff {
            consecutive_failures: 9,
            retry_after_pass: u64::MAX,
        },
    );

    // Act
    for _ in 0..5 {
        orchestrator.run_refresh_pass(ReviewPass::Skip).await;
    }

    // Assert
    for _ in 0..5 {
        assert!(matches!(
            app_event_rx.try_recv(),
            Ok(AppEvent::GitStatusUpdated { .. })
        ));
    }
    let spans = exporter.get_finished_spans().expect("finished spans");
    let fetches = spans
        .iter()
        .filter(|span| span.name == "git.fetch")
        .collect::<Vec<_>>();
    assert_eq!(fetches.len(), 4);
    assert!(
        fetches[1]
            .attributes
            .contains(&KeyValue::new("agentty.sync.consecutive_failures", 2_i64))
    );
    assert!(
        fetches[1]
            .attributes
            .contains(&KeyValue::new("error.type", "network"))
    );
    let backed_off_refreshes = spans
        .iter()
        .filter(|span| {
            span.name == "sync.refresh"
                && span
                    .attributes
                    .contains(&KeyValue::new("agentty.sync.fetch_backed_off", true))
        })
        .collect::<Vec<_>>();
    assert_eq!(backed_off_refreshes.len(), 1);
    assert!(
        backed_off_refreshes[0]
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "completed"))
    );
    assert!(!format!("{spans:?}").contains("private"));
    assert_eq!(
        orchestrator
            .fetch_failures
            .keys()
            .copied()
            .collect::<Vec<_>>(),
        vec![2]
    );
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
    let (sync_handle, orchestrator) =
        SyncHandle::unstarted(app_event_tx.clone(), sync_context.clone());
    orchestrator.spawn();
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
/// A context published before the orchestrator spawns waits for it, so the
/// immediate first pass reads that context instead of the initial one.
async fn unstarted_orchestrator_first_pass_reads_context_published_before_spawn() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .once()
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_fetch_remote()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .once()
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    let initial_context = SyncContext {
        git_client: Arc::new(mock_git_client),
        ..sync_context_fixture(0, Vec::new())
    };
    let reconciled_context = initial_context.clone();
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();
    let (sync_handle, orchestrator) = SyncHandle::unstarted(app_event_tx, initial_context);
    sync_handle.publish_refresh_context(reconciled_context);

    // Act
    orchestrator.spawn();
    let event = tokio::time::timeout(Duration::from_secs(1), app_event_rx.recv())
        .await
        .expect("the first pass should run at once")
        .expect("the orchestrator should emit a git status");

    // Assert
    assert!(matches!(
        event,
        AppEvent::GitStatusUpdated { generation: 1, .. }
    ));
}

#[tokio::test]
/// Successful manual syncs preserve their captured review results, emit
/// them only with the matching project operation, and clear the project's
/// fetch backoff.
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
    let (_context_tx, context_rx) = watch::channel(sync_context.clone());
    let (mut orchestrator, _orchestrator_event_rx) = orchestrator_for(context_rx);
    let backoff = FailureBackoff {
        consecutive_failures: 4,
        retry_after_pass: u64::MAX,
    };
    orchestrator.fetch_failures.insert(1, backoff);
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
    assert!(orchestrator.fetch_failures.is_empty());
}

#[tokio::test]
/// Manual sync runs as one root trace that parents its review refresh and
/// fails with the sync.
async fn run_sync_main_traces_its_phases_beneath_one_failed_root() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_span_exporter();
    let mut sync_context = sync_context_fixture(0, Vec::new());
    sync_context.project_branch_name = None;
    let (_context_tx, context_rx) = watch::channel(sync_context.clone());
    let (mut orchestrator, _orchestrator_event_rx) = orchestrator_for(context_rx);
    let (app_event_tx, mut app_event_rx) = mpsc::unbounded_channel();

    // Act
    orchestrator
        .run_sync_main(SyncMainRequest {
            app_event_tx,
            operation: project_sync_context(),
            session_model: AgentModel::Gemini38Flash,
            sync_context,
        })
        .await;

    // Assert
    assert!(matches!(
        app_event_rx.try_recv(),
        Ok(AppEvent::SyncMainCompleted {
            completion: SyncMainCompletion { result: Err(_), .. }
        })
    ));
    let sync_main = finished_span(&exporter, "project.sync_main");
    assert_eq!(sync_main.parent_span_id, SpanId::INVALID);
    assert!(
        sync_main
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "failed"))
    );
    assert_phase(
        &finished_span(&exporter, "sync.review_requests"),
        &sync_main,
        "completed",
        &[
            KeyValue::new("agentty.sync.failures", 0_i64),
            KeyValue::new("agentty.sync.targets", 0_i64),
        ],
    );
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
        base_comparison_cache: BaseComparisonCache::default(),
        command_rx,
        context_rx,
        fetch_failures: HashMap::new(),
        fetch_pass_index: 0,
        review_pass_index: 0,
        review_sync_cadences: HashMap::new(),
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let updates = vec![
        ReviewRequestPassUpdate {
            result: Ok(SyncReviewRequestTaskResult {
                attention: None,
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
                attention: None,
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
fn failure_backoff_skipped_passes_grows_exponentially_and_caps() {
    // Arrange
    let review_cap = REVIEW_SYNC_MAX_BACKOFF_PASSES;
    let fetch_cap = FETCH_MAX_BACKOFF_PASSES;

    // Act / Assert
    assert_eq!(FailureBackoff::skipped_passes(1, review_cap), 0);
    assert_eq!(FailureBackoff::skipped_passes(2, review_cap), 1);
    assert_eq!(FailureBackoff::skipped_passes(3, review_cap), 3);
    assert_eq!(FailureBackoff::skipped_passes(4, review_cap), 7);
    assert_eq!(FailureBackoff::skipped_passes(50, review_cap), 7);
    assert_eq!(FailureBackoff::skipped_passes(5, fetch_cap), 15);
    assert_eq!(FailureBackoff::skipped_passes(u32::MAX, fetch_cap), 15);
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
        base_comparison_cache: BaseComparisonCache::default(),
        command_rx,
        context_rx,
        fetch_failures: HashMap::new(),
        fetch_pass_index: 0,
        review_pass_index: 0,
        review_sync_cadences: HashMap::new(),
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
        base_comparison_cache: BaseComparisonCache::default(),
        command_rx,
        context_rx,
        fetch_failures: HashMap::new(),
        fetch_pass_index: 0,
        review_pass_index: 0,
        review_sync_cadences: HashMap::new(),
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let target = review_request_sync_target("session-1", None);
    let failure: Result<SyncReviewRequestTaskResult, String> = Err("gh exploded".to_string());
    let success = Ok(SyncReviewRequestTaskResult {
        attention: None,
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

#[test]
/// Every second periodic tick includes a scheduled review-request pass.
fn review_pass_for_tick_schedules_every_second_tick() {
    // Arrange / Act / Assert
    assert_eq!(ReviewPass::for_tick(0), ReviewPass::Scheduled);
    assert_eq!(ReviewPass::for_tick(1), ReviewPass::Skip);
    assert_eq!(ReviewPass::for_tick(2), ReviewPass::Scheduled);
}

#[test]
/// A target whose result repeats for the idle threshold is polled once per
/// idle interval, unless its watched checks are still running or it waits in
/// the merge queue.
fn record_review_sync_outcome_idles_unchanged_targets_unless_checks_run() {
    // Arrange
    let open_result = sync_task_result_from_summary(
        test_review_request_summary("#42", ReviewRequestState::Open),
        None,
    );
    let mut queued_summary = test_review_request_summary("#42", ReviewRequestState::Open);
    queued_summary.status_summary = Some("Mergeable, In merge queue".to_string());
    let queued_result = sync_task_result_from_summary(queued_summary, None);
    let watched_result = |has_pending_checks| SyncReviewRequestTaskResult {
        attention: Some(Ok(ag_forge::ReviewRequestAttention {
            has_pending_checks,
            head_commit: "abc1234".to_string(),
            ..ag_forge::ReviewRequestAttention::default()
        })),
        ..open_result.clone()
    };
    let cases = [
        ("unwatched", open_result.clone(), true),
        ("watched_checks_settled", watched_result(false), true),
        ("watched_checks_running", watched_result(true), false),
        ("in_merge_queue", queued_result, false),
    ];
    let target = review_request_sync_target(
        "session-1",
        Some(linked_review_request("#42", ReviewRequestState::Open)),
    );
    for (case, task_result, expects_idle) in cases {
        let (_context_tx, context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
        let (mut orchestrator, _app_event_rx) = orchestrator_for(context_rx);
        let result = Ok(task_result);

        // Act
        for review_pass_index in 0..3 {
            orchestrator.record_review_sync_outcome(&target, &result, review_pass_index);
        }
        let idle_below_threshold = orchestrator.is_target_idle(&target, 3);
        orchestrator.record_review_sync_outcome(&target, &result, 3);

        // Assert
        assert!(!idle_below_threshold, "{case}");
        assert_eq!(
            orchestrator.is_target_idle(&target, 4),
            expects_idle,
            "{case}"
        );
        assert_eq!(
            orchestrator.is_target_idle(&target, 7),
            expects_idle,
            "{case}"
        );
        assert!(!orchestrator.is_target_idle(&target, 8), "{case}");
    }
}

#[test]
/// A changed polling key, a changed result, or a failure returns an idle
/// target to the full cadence, while an unchanged idle poll keeps it idle.
fn idle_review_target_returns_to_full_cadence_on_change() {
    // Arrange
    let (_context_tx, context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
    let (mut orchestrator, _app_event_rx) = orchestrator_for(context_rx);
    let target = review_request_sync_target(
        "session-1",
        Some(linked_review_request("#42", ReviewRequestState::Open)),
    );
    let mut watched_target = target.clone();
    watched_target.is_watched = true;
    let unchanged = Ok(sync_task_result_from_summary(
        test_review_request_summary("#42", ReviewRequestState::Open),
        None,
    ));
    let changed = Ok(sync_task_result_from_summary(
        ReviewRequestSummary {
            status_summary: Some("Approved".to_string()),
            ..test_review_request_summary("#42", ReviewRequestState::Open)
        },
        None,
    ));
    let failure: Result<SyncReviewRequestTaskResult, String> = Err("gh exploded".to_string());
    for review_pass_index in 0..4 {
        orchestrator.record_review_sync_outcome(&target, &unchanged, review_pass_index);
    }

    // Act
    let idle_after_key_change = orchestrator.is_target_idle(&watched_target, 4);
    orchestrator.record_review_sync_outcome(&target, &unchanged, 8);
    let idle_after_unchanged_idle_poll = orchestrator.is_target_idle(&target, 12);
    orchestrator.record_review_sync_outcome(&target, &changed, 13);
    let idle_after_changed_result = orchestrator.is_target_idle(&target, 14);
    orchestrator.record_review_sync_outcome(&target, &failure, 14);

    // Assert
    assert!(!idle_after_key_change);
    assert!(idle_after_unchanged_idle_poll);
    assert!(!idle_after_changed_result);
    assert!(orchestrator.review_sync_cadences.is_empty());
}

#[tokio::test]
/// Scheduled passes leave an idle target without forge calls and count it,
/// while a forced pass polls it at once.
async fn collect_review_request_updates_skips_idle_targets_unless_forced() {
    // Arrange
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_repo_url().times(5).returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    let mut mock_review_request_client = MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .times(5)
        .returning(|repo_url| ag_forge::detect_remote(&repo_url));
    mock_review_request_client
        .expect_refresh_review_request()
        .times(5)
        .returning(|_, _| {
            Box::pin(async { Ok(test_review_request_summary("#42", ReviewRequestState::Open)) })
        });
    let context = SyncContext {
        git_client: Arc::new(mock_git_client),
        review_request_client: Arc::new(mock_review_request_client),
        ..sync_context_fixture(
            0,
            vec![review_request_sync_target(
                "session-1",
                Some(linked_review_request("#42", ReviewRequestState::Open)),
            )],
        )
    };
    let (_context_tx, context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
    let (mut orchestrator, _app_event_rx) = orchestrator_for(context_rx);
    for _ in 0..4 {
        orchestrator
            .collect_review_request_updates(&context, true, false)
            .await;
    }

    // Act
    let (scheduled_updates, scheduled_idle_targets) = orchestrator
        .collect_review_request_updates(&context, true, false)
        .await;
    let (forced_updates, forced_idle_targets) = orchestrator
        .collect_review_request_updates(&context, true, true)
        .await;

    // Assert
    assert!(scheduled_updates.is_empty());
    assert_eq!(scheduled_idle_targets, 1);
    assert_eq!(forced_updates.len(), 1);
    assert_eq!(forced_idle_targets, 0);
}

#[tokio::test]
/// Verifies session git-status selection maps branch comparisons back to
/// session ids, comparing by name when the base is not a local branch.
async fn session_git_statuses_collects_all_target_statuses() {
    // Arrange
    let repo_root = Path::new("/tmp/sync-session-statuses");
    let branch_tracking_statuses = HashMap::from([
        tracked_branch("wt/session-a", "a1", Some((7, 0))),
        tracked_branch("wt/session-b", "b1", Some((0, 4))),
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
    let statuses = BaseComparisonCache::default()
        .session_git_statuses(
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
    let statuses = BaseComparisonCache::default()
        .session_git_statuses(
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
/// ahead/behind information, and is retried on the next pass instead of
/// being reused.
async fn session_git_statuses_keeps_failed_conflict_probe_unknown() {
    // Arrange
    let repo_root = Path::new("/tmp/sync-session-conflict-error");
    let branch_tracking_statuses = HashMap::from([
        tracked_branch("main", "m1", None),
        tracked_branch("wt/session-a", "a1", None),
    ]);
    let session_git_status_targets = vec![session_git_status_target("session-a")];
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_get_ref_ahead_behind()
        .times(2)
        .returning(|_, _, _| Box::pin(async { Ok((1, 2)) }));
    mock_git_client
        .expect_has_merge_conflicts()
        .times(2)
        .returning(|_, _, _| {
            Box::pin(async {
                Err(GitError::OutputParse(
                    "failed to compute merge tree".to_string(),
                ))
            })
        });
    let mut base_comparison_cache = BaseComparisonCache::default();

    // Act
    for _ in 0..2 {
        let statuses = base_comparison_cache
            .session_git_statuses(
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
                base_status: Some((1, 2)),
                has_merge_conflict: None,
                remote_status: None,
            })
        );
    }
    assert!(base_comparison_cache.comparisons.is_empty());
}

#[tokio::test]
/// Verifies unchanged commit pairs are compared once across passes and
/// shared between sessions, and branches at their base tip skip Git.
async fn session_git_statuses_reuses_unchanged_commit_comparisons() {
    // Arrange
    let repo_root = Path::new("/tmp/sync-session-cache");
    let branch_tracking_statuses = HashMap::from([
        tracked_branch("main", "m1", None),
        tracked_branch("wt/session-a", "a1", Some((1, 0))),
        tracked_branch("wt/session-b", "a1", None),
        tracked_branch("wt/session-c", "m1", None),
    ]);
    let session_git_status_targets = vec![
        session_git_status_target("session-a"),
        session_git_status_target("session-b"),
        session_git_status_target("session-c"),
    ];
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_get_ref_ahead_behind()
        .once()
        .withf(|repo_path, left_ref, right_ref| {
            repo_path == Path::new("/tmp/sync-session-cache")
                && left_ref == "a1"
                && right_ref == "m1"
        })
        .returning(|_, _, _| Box::pin(async { Ok((2, 1)) }));
    mock_git_client
        .expect_has_merge_conflicts()
        .once()
        .withf(|_, source_branch, target_branch| source_branch == "a1" && target_branch == "m1")
        .returning(|_, _, _| Box::pin(async { Ok(true) }));
    let mut base_comparison_cache = BaseComparisonCache::default();
    let diverged = SessionGitStatus {
        base_status: Some((2, 1)),
        has_merge_conflict: Some(true),
        remote_status: None,
    };

    // Act
    for _ in 0..2 {
        let statuses = base_comparison_cache
            .session_git_statuses(
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
                remote_status: Some((1, 0)),
                ..diverged
            })
        );
        assert_eq!(statuses.get("session-b"), Some(&diverged));
        assert_eq!(
            statuses.get("session-c"),
            Some(&SessionGitStatus {
                base_status: Some((0, 0)),
                has_merge_conflict: Some(false),
                remote_status: None,
            })
        );
    }
}

#[tokio::test]
/// Verifies moved commits and a different repository are compared again,
/// and only the latest pass's commit pairs stay cached.
async fn session_git_statuses_recompares_moved_commits_and_other_repositories() {
    // Arrange
    let session_git_status_targets = vec![session_git_status_target("session-a")];
    let mut mock_git_client = MockGitClient::new();
    let mut sequence = Sequence::new();
    for (repo_root, head_commit, ahead) in [
        ("/tmp/sync-repo-a", "a1", 1),
        ("/tmp/sync-repo-a", "a2", 2),
        ("/tmp/sync-repo-b", "a2", 3),
    ] {
        mock_git_client
            .expect_get_ref_ahead_behind()
            .once()
            .in_sequence(&mut sequence)
            .withf(move |repo_path, left_ref, right_ref| {
                repo_path == Path::new(repo_root) && left_ref == head_commit && right_ref == "m1"
            })
            .returning(move |_, _, _| Box::pin(async move { Ok((ahead, 0)) }));
    }
    let mut base_comparison_cache = BaseComparisonCache::default();
    let mut base_statuses = Vec::new();

    // Act
    for (repo_root, head_commit) in [
        ("/tmp/sync-repo-a", "a1"),
        ("/tmp/sync-repo-a", "a2"),
        ("/tmp/sync-repo-b", "a2"),
    ] {
        let branch_tracking_statuses = HashMap::from([
            tracked_branch("main", "m1", None),
            tracked_branch("wt/session-a", head_commit, None),
        ]);
        let statuses = base_comparison_cache
            .session_git_statuses(
                &branch_tracking_statuses,
                Path::new(repo_root),
                &session_git_status_targets,
                &mock_git_client,
            )
            .await;
        base_statuses.push(statuses["session-a"].base_status);
    }

    // Assert
    assert_eq!(base_statuses, [Some((1, 0)), Some((2, 0)), Some((3, 0))]);
    assert_eq!(
        base_comparison_cache.comparisons.keys().collect::<Vec<_>>(),
        [&CommitPair {
            base_commit: "m1".to_string(),
            head_commit: "a2".to_string(),
        }]
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
        &review_sync_target(folder, Some(linked), None, false),
        &mock_git_client,
        &mock_review_request_client,
        &Ok(ReviewWatchConfig::default()),
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
        &review_sync_target(folder, Some(linked), None, false),
        &mock_git_client,
        &mock_review_request_client,
        &Ok(ReviewWatchConfig::default()),
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

#[tokio::test]
/// Verifies refresh, discovery, and fallback failures surface as sync errors
/// while discovered or missing review requests map to their outcomes.
async fn sync_review_request_status_maps_refresh_and_discovery_outcomes() {
    // Arrange
    let gitlab_linked = crate::domain::session::ReviewRequest {
        last_refreshed_at: 0,
        summary: ReviewRequestSummary {
            web_url: "https://gitlab.com/org/repo/-/merge_requests/5".to_string(),
            ..test_review_request_summary_with_forge(
                "!5",
                ReviewRequestState::Open,
                ForgeKind::GitLab,
            )
        },
    };
    let cases: [(
        &str,
        Option<crate::domain::session::ReviewRequest>,
        Result<session::SyncReviewRequestOutcome, &str>,
    ); 6] = [
        (
            "refresh_error",
            Some(linked_review_request("#42", ReviewRequestState::Open)),
            Err("GitHub review-request operation failed: refresh failed"),
        ),
        (
            "unpublished",
            None,
            Err("Session branch has not been published yet"),
        ),
        (
            "find_error",
            None,
            Err("GitHub review-request operation failed: lookup failed"),
        ),
        (
            "found_open",
            None,
            Ok(session::SyncReviewRequestOutcome::Open {
                display_id: "#42".to_string(),
                status_summary: None,
            }),
        ),
        (
            "not_found",
            None,
            Ok(session::SyncReviewRequestOutcome::NoReviewRequest),
        ),
        (
            "fallback_detect_error",
            Some(gitlab_linked),
            Err("GitLab review-request operation failed: remote rejected"),
        ),
    ];
    for (case, linked, expected) in cases {
        let (mock_git_client, mock_review_request_client) = review_status_sync_clients(case);
        let published_upstream_ref =
            (case != "unpublished").then(|| "origin/wt/session-id".to_string());

        // Act
        let result = sync_review_request_status(
            &review_sync_target(
                PathBuf::from("/tmp/session-worktree"),
                linked,
                published_upstream_ref,
                false,
            ),
            &mock_git_client,
            &mock_review_request_client,
            &Ok(ReviewWatchConfig::default()),
        )
        .await;

        // Assert
        match expected {
            Ok(expected_outcome) => {
                assert_eq!(result.expect(case).outcome, expected_outcome, "{case}");
            }
            Err(expected_error) => {
                let error = result.expect_err(case);
                assert!(error.contains(expected_error), "{case}: {error}");
            }
        }
    }
}

/// Builds one forge operation failure carrying `message`.
fn forge_failure(forge_kind: ForgeKind, message: &str) -> ag_forge::ReviewRequestError {
    ag_forge::ReviewRequestError::OperationFailed {
        forge_kind,
        message: message.to_string(),
    }
}

/// Builds Git and forge clients that drive one review-status sync `case`.
fn review_status_sync_clients(case: &'static str) -> (MockGitClient, MockReviewRequestClient) {
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_repo_url()
        .once()
        .returning(move |_| {
            Box::pin(async move {
                if case == "fallback_detect_error" {
                    return Err(GitError::CommandFailed {
                        command: "git remote get-url origin".to_string(),
                        stderr: "worktree removed".to_string(),
                    });
                }

                Ok("https://github.com/agentty-xyz/agentty.git".to_string())
            })
        });
    let mut mock_review_request_client = MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(move |repo_url| {
            if case == "fallback_detect_error" {
                assert_eq!(repo_url, "https://gitlab.com/org/repo");

                return Err(forge_failure(ForgeKind::GitLab, "remote rejected"));
            }

            ag_forge::detect_remote(&repo_url)
        });
    mock_review_request_client
        .expect_refresh_review_request()
        .times(usize::from(case == "refresh_error"))
        .returning(move |_, _| {
            Box::pin(async move { Err(forge_failure(ForgeKind::GitHub, "refresh failed")) })
        });
    mock_review_request_client
        .expect_find_by_source_branch()
        .times(usize::from(matches!(
            case,
            "find_error" | "found_open" | "not_found"
        )))
        .withf(|_, source_branch| source_branch == "wt/session-id")
        .returning(move |_, _| {
            Box::pin(async move {
                match case {
                    "find_error" => Err(forge_failure(ForgeKind::GitHub, "lookup failed")),
                    "found_open" => Ok(Some(test_review_request_summary(
                        "#42",
                        ReviewRequestState::Open,
                    ))),
                    _ => Ok(None),
                }
            })
        });

    (mock_git_client, mock_review_request_client)
}

#[test]
/// Verifies GitLab merge-request URLs with and without the `/-/` separator
/// resolve back to their repository URL.
fn review_request_repo_url_supports_gitlab_merge_request_paths() {
    // Arrange
    let cases = [
        (
            "https://gitlab.com/org/repo/-/merge_requests/5/",
            Some("https://gitlab.com/org/repo"),
        ),
        (
            "https://gitlab.example.com/org/repo/merge_requests/6",
            Some("https://gitlab.example.com/org/repo"),
        ),
        ("https://gitlab.example.com/org/repo", None),
    ];

    for (web_url, expected_repo_url) in cases {
        let review_request = crate::domain::session::ReviewRequest {
            last_refreshed_at: 0,
            summary: ReviewRequestSummary {
                web_url: web_url.to_string(),
                ..test_review_request_summary_with_forge(
                    "!5",
                    ReviewRequestState::Open,
                    ForgeKind::GitLab,
                )
            },
        };

        // Act
        let repo_url = review_request_repo_url(&review_request);

        // Assert
        assert_eq!(repo_url.as_deref(), expected_repo_url, "{web_url}");
    }
}

#[tokio::test]
/// Backed-off targets are skipped, and stale contexts stop the pass before
/// or after a target refresh without returning updates.
async fn collect_review_request_pass_updates_skips_backed_off_and_stale_targets() {
    // Arrange
    let targets = vec![review_request_sync_target("session-1", None)];
    let (context_tx, context_rx) = watch::channel(sync_context_fixture(0, targets.clone()));
    let context_tx = Arc::new(context_tx);
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let mut orchestrator = SyncOrchestrator {
        app_event_tx,
        base_comparison_cache: BaseComparisonCache::default(),
        command_rx,
        context_rx,
        fetch_failures: HashMap::new(),
        fetch_pass_index: 0,
        review_pass_index: 0,
        review_sync_cadences: HashMap::new(),
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };
    let failure: Result<SyncReviewRequestTaskResult, String> = Err("gh exploded".to_string());
    orchestrator.record_review_sync_outcome(&targets[0], &failure, 0);
    orchestrator.record_review_sync_outcome(&targets[0], &failure, 1);
    let backed_off_context = sync_context_fixture(0, targets.clone());
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_repo_url().once().returning({
        let context_tx = Arc::clone(&context_tx);
        move |_| {
            context_tx.send_modify(|context| context.generation = 1);

            Box::pin(async {
                Err(GitError::CommandFailed {
                    command: "git remote get-url origin".to_string(),
                    stderr: "not a git repository".to_string(),
                })
            })
        }
    });
    let stale_after_refresh_context = SyncContext {
        git_client: Arc::new(mock_git_client),
        ..sync_context_fixture(0, targets.clone())
    };
    let stale_before_refresh_context = sync_context_fixture(0, targets);

    // Act
    let backed_off_updates = orchestrator
        .collect_review_request_pass_updates(&backed_off_context, true, false)
        .await;
    orchestrator.review_sync_failures.clear();
    let stale_after_refresh_updates = orchestrator
        .collect_review_request_pass_updates(&stale_after_refresh_context, true, false)
        .await;
    let stale_before_refresh_updates = orchestrator
        .collect_review_request_pass_updates(&stale_before_refresh_context, true, false)
        .await;

    // Assert
    assert!(backed_off_updates.is_empty());
    assert!(stale_after_refresh_updates.is_empty());
    assert!(stale_before_refresh_updates.is_empty());
    assert!(orchestrator.review_sync_failures.is_empty());
}

#[tokio::test]
/// A git-only refresh pass emits just the git snapshot, while a
/// review-including pass also emits one review-request update per target.
async fn refresh_pass_emits_review_updates_only_when_requested() {
    // Arrange
    let _provider_guard = TRACER_PROVIDER_LOCK.lock().await;
    let exporter = install_span_exporter();
    let mut mock_git_client = MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(2)
        .returning(|_| Box::pin(async { None }));
    mock_git_client
        .expect_fetch_remote()
        .times(2)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_branch_tracking_statuses()
        .times(2)
        .returning(|_| {
            Box::pin(async { Ok(HashMap::from([tracked_branch("main", "m1", Some((1, 0)))])) })
        });
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async {
            Err(GitError::CommandFailed {
                command: "git remote get-url origin".to_string(),
                stderr: "not a git repository".to_string(),
            })
        })
    });
    let context = SyncContext {
        git_client: Arc::new(mock_git_client),
        ..sync_context_fixture(3, vec![review_request_sync_target("session-1", None)])
    };
    let (_context_tx, context_rx) = watch::channel(context);
    let (mut orchestrator, mut app_event_rx) = orchestrator_for(context_rx);
    crate::test_support::enable_tracing_log_fallback();

    // Act
    orchestrator.run_refresh_pass(ReviewPass::Skip).await;
    let git_only_events = std::iter::from_fn(|| app_event_rx.try_recv().ok()).count();
    orchestrator.run_refresh_pass(ReviewPass::Scheduled).await;

    // Assert
    assert_eq!(git_only_events, 1);
    assert!(matches!(
        app_event_rx
            .try_recv()
            .expect("git status should be emitted"),
        AppEvent::GitStatusUpdated {
            generation: 3,
            status: Some((1, 0)),
            ..
        }
    ));
    assert!(matches!(
        app_event_rx.try_recv().expect("review update should be emitted"),
        AppEvent::ReviewRequestStatusUpdated {
            generation: 3,
            result: Err(error),
            session_id,
        } if session_id.as_str() == "session-1"
            && error.contains("Failed to resolve repository remote")
    ));
    assert!(app_event_rx.try_recv().is_err());
    let review_requests = finished_span(&exporter, "sync.review_requests");
    let refreshes = exporter
        .get_finished_spans()
        .expect("finished spans")
        .into_iter()
        .filter(|span| span.name == "sync.refresh")
        .collect::<Vec<_>>();
    assert_eq!(refreshes.len(), 2);
    assert!(
        refreshes[0]
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "completed"))
    );
    assert!(
        refreshes[1]
            .attributes
            .contains(&KeyValue::new("agentty.sync.review_pass", true))
    );
    assert_phase(
        &review_requests,
        &refreshes[1],
        "failed",
        &[
            KeyValue::new("agentty.sync.failures", 1_i64),
            KeyValue::new("agentty.sync.idle_targets", 0_i64),
            KeyValue::new("agentty.sync.targets", 1_i64),
        ],
    );
    assert!(
        refreshes[1]
            .attributes
            .contains(&KeyValue::new("agentty.outcome", "failed"))
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

/// Builds one review-status sync target with explicit polling inputs.
fn review_sync_target(
    folder: PathBuf,
    linked: Option<crate::domain::session::ReviewRequest>,
    published_upstream_ref: Option<String>,
    is_watched: bool,
) -> ReviewRequestSyncTarget {
    ReviewRequestSyncTarget {
        folder,
        is_watched,
        linked_review_request: linked,
        published_upstream_ref,
        session_id: "session-1".into(),
    }
}

/// Builds mock clients whose linked review request refreshes into `state`.
fn watched_review_status_sync_clients(
    state: ReviewRequestState,
) -> (MockGitClient, MockReviewRequestClient) {
    let mut mock_git_client = MockGitClient::new();
    mock_git_client.expect_repo_url().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });
    mock_git_client
        .expect_head_hash()
        .returning(|_| Box::pin(async { Ok("abc1234def5678".to_string()) }));
    let mut mock_review_request_client = MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .returning(|repo_url| ag_forge::detect_remote(&repo_url));
    mock_review_request_client
        .expect_refresh_review_request()
        .returning(move |_, _| {
            Box::pin(async move { Ok(test_review_request_summary("#42", state)) })
        });

    (mock_git_client, mock_review_request_client)
}

#[tokio::test]
/// A watched open review request also reports the forge facts that may need
/// automatic follow-up; unwatched and merged targets never fetch them.
async fn sync_review_request_status_reports_attention_only_for_watched_open_requests() {
    // Arrange
    let attention = ag_forge::ReviewRequestAttention {
        has_merge_conflict: true,
        head_commit: "abc1234".to_string(),
        ..ag_forge::ReviewRequestAttention::default()
    };
    let cases = [
        ("watched_open", true, ReviewRequestState::Open, true),
        ("unwatched_open", false, ReviewRequestState::Open, false),
        ("watched_merged", true, ReviewRequestState::Merged, false),
    ];
    let review_watch_config = Ok(ReviewWatchConfig {
        trusted_bots: vec!["greptile-apps".to_string()],
    });
    for (case, is_watched, state, expects_attention) in cases {
        let (mock_git_client, mut mock_review_request_client) =
            watched_review_status_sync_clients(state);
        if expects_attention {
            let attention = attention.clone();
            mock_review_request_client
                .expect_fetch_review_request_attention()
                .once()
                .withf(|_, display_id, trusted_bots| {
                    display_id == "#42" && trusted_bots == &["greptile-apps".to_string()]
                })
                .returning(move |_, _, _| {
                    let attention = attention.clone();

                    Box::pin(async move { Ok(attention) })
                });
        }
        let target = review_sync_target(
            PathBuf::from("/tmp/session-worktree"),
            Some(linked_review_request("#42", ReviewRequestState::Open)),
            None,
            is_watched,
        );

        // Act
        let result = sync_review_request_status(
            &target,
            &mock_git_client,
            &mock_review_request_client,
            &review_watch_config,
        )
        .await
        .expect(case);

        // Assert
        assert_eq!(
            result.attention,
            expects_attention.then(|| Ok(attention.clone())),
            "{case}"
        );
    }
}

#[tokio::test]
/// A failed follow-up lookup, or a configuration that cannot be read, is
/// reported next to the refreshed summary, so watching never costs a session
/// its ordinary review-request refresh.
async fn sync_review_request_status_keeps_summary_when_attention_lookup_fails() {
    // Arrange
    let cases = [
        (Ok(ReviewWatchConfig::default()), "checks unavailable"),
        (
            Err("Invalid `.agentty/review-watch.json`".to_string()),
            "Invalid `.agentty/review-watch.json`",
        ),
    ];
    for (review_watch_config, expected_error) in cases {
        let (mock_git_client, mut mock_review_request_client) =
            watched_review_status_sync_clients(ReviewRequestState::Open);
        mock_review_request_client
            .expect_fetch_review_request_attention()
            .times(usize::from(review_watch_config.is_ok()))
            .returning(|_, _, _| {
                Box::pin(async { Err(forge_failure(ForgeKind::GitHub, "checks unavailable")) })
            });
        let target = review_sync_target(
            PathBuf::from("/tmp/session-worktree"),
            Some(linked_review_request("#42", ReviewRequestState::Open)),
            None,
            true,
        );

        // Act
        let result = sync_review_request_status(
            &target,
            &mock_git_client,
            &mock_review_request_client,
            &review_watch_config,
        )
        .await
        .expect("attention failure should not fail the sync");

        // Assert
        assert_eq!(
            result.summary,
            Some(test_review_request_summary("#42", ReviewRequestState::Open))
        );
        let error = result
            .attention
            .expect("attention lookup should be reported")
            .expect_err("attention lookup should fail");
        assert!(error.contains(expected_error), "{error}");
    }
}

#[tokio::test]
/// A pass reads the project's review-watch configuration only when one of
/// its targets is watched.
async fn collect_review_request_pass_updates_loads_configuration_only_for_watched_targets() {
    // Arrange
    let mut fs_client = MockFsClient::new();
    fs_client
        .expect_read_file_prefix()
        .once()
        .withf(|path, _| path == Path::new("/tmp/project/.agentty/review-watch.json"))
        .returning(|_, _| Box::pin(async { Ok(br#"{"trusted_bots":[""]}"#.to_vec()) }));
    let (mock_git_client, mock_review_request_client) =
        watched_review_status_sync_clients(ReviewRequestState::Open);
    let mut watched_target = review_request_sync_target(
        "session-1",
        Some(linked_review_request("#42", ReviewRequestState::Open)),
    );
    watched_target.is_watched = true;
    let watched_context = SyncContext {
        fs_client: Arc::new(fs_client),
        git_client: Arc::new(mock_git_client),
        review_request_client: Arc::new(mock_review_request_client),
        ..sync_context_fixture(0, vec![watched_target])
    };
    let unwatched_context = sync_context_fixture(0, Vec::new());
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let (_command_tx, command_rx) = mpsc::unbounded_channel();
    let (_context_tx, context_rx) = watch::channel(sync_context_fixture(0, Vec::new()));
    let mut orchestrator = SyncOrchestrator {
        app_event_tx,
        base_comparison_cache: BaseComparisonCache::default(),
        command_rx,
        context_rx,
        fetch_failures: HashMap::new(),
        fetch_pass_index: 0,
        review_pass_index: 0,
        review_sync_cadences: HashMap::new(),
        review_sync_failures: HashMap::new(),
        tick_index: 0,
    };

    // Act
    let unwatched_updates = orchestrator
        .collect_review_request_pass_updates(&unwatched_context, true, false)
        .await;
    let watched_updates = orchestrator
        .collect_review_request_pass_updates(&watched_context, true, false)
        .await;

    // Assert
    assert!(unwatched_updates.is_empty());
    assert_eq!(watched_updates.len(), 1);
    let error = watched_updates[0]
        .result
        .as_ref()
        .expect("refresh should succeed")
        .attention
        .clone()
        .expect("watched target should report attention")
        .expect_err("invalid configuration should be reported");
    assert!(
        error.starts_with("`.agentty/review-watch.json` needs nonempty"),
        "{error}"
    );
}

#[test]
/// Starting or stopping a watch changes what each pass fetches, so it must
/// register as a polling-input change.
fn same_polling_inputs_detects_watch_change() {
    // Arrange
    let linked = linked_review_request("#42", ReviewRequestState::Open);
    let unwatched_context = sync_context_fixture(
        0,
        vec![review_request_sync_target(
            "session-1",
            Some(linked.clone()),
        )],
    );
    let mut watched_target = review_request_sync_target("session-1", Some(linked));
    watched_target.is_watched = true;
    let watched_context = sync_context_fixture(0, vec![watched_target]);

    // Act
    let same_inputs = unwatched_context.same_polling_inputs(&watched_context);

    // Assert
    assert!(!same_inputs);
}
