use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_agent as agent;
use ag_agent::{MockOneShotClient, OneShotClient};
use ag_forge as forge;
use ag_git as git;
use ag_git::GitClient;
use tempfile::{TempDir, tempdir};
use tokio::sync::mpsc;

use super::super::{
    MergeTaskInput, RebaseAssistInput, RebaseAssistMode, RebasePlan, SyncAssistClient,
    SyncMainOutcome, SyncRebaseAssistInput,
};
use crate::app::AppEvent;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection};
use crate::domain::session::{PublishedBranchSyncStatus, Status};
use crate::domain::session_message::SessionTranscript;
use crate::infra::db::{AppRepositories, SessionOperationRow};
use crate::infra::fs;
use crate::infra::fs::FsClient;

/// Builds a filesystem mock that delegates operations to local disk.
pub(super) fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_create_dir_all()
        .times(0..)
        .returning(|path| {
            Box::pin(async move {
                tokio::fs::create_dir_all(path)
                    .await
                    .map_err(fs::FsError::from)
            })
        });
    mock_fs_client
        .expect_remove_dir_all()
        .times(0..)
        .returning(|path| {
            Box::pin(async move {
                tokio::fs::remove_dir_all(path)
                    .await
                    .map_err(fs::FsError::from)
            })
        });
    mock_fs_client
        .expect_read_file()
        .times(0..)
        .returning(|path| {
            Box::pin(async move { tokio::fs::read(path).await.map_err(fs::FsError::from) })
        });
    mock_fs_client
        .expect_remove_file()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_is_dir()
        .times(0..)
        .returning(|path| path.is_dir());

    mock_fs_client
}

/// Returns a fresh mocked filesystem client trait object for tests.
pub(super) fn test_fs_client() -> Arc<dyn FsClient> {
    Arc::new(create_passthrough_mock_fs_client())
}

/// Builds a deterministic one-shot boundary for pre-rebase auto-commit.
pub(super) fn test_one_shot_client() -> Arc<dyn OneShotClient> {
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().times(0..).returning(|_| {
        Ok(agent::OneShotSubmission {
            response: ag_protocol::AgentResponse::plain("Existing session commit"),
            stats: agent::SessionStats {
                added_lines: 0,
                deleted_lines: 0,
                diff_state: agent::SessionDiffState::Unknown,
                input_tokens: 0,
                output_tokens: 0,
            },
        })
    });

    Arc::new(one_shot_client)
}

/// Returns the agent selection used by rebase workflow tests.
pub(super) fn test_session_agent() -> AgentSelection {
    AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash)
}

/// Returns an empty forge boundary for tests that do not sync metadata.
pub(super) fn empty_review_request_client() -> Arc<dyn forge::ReviewRequestClient> {
    Arc::new(forge::MockReviewRequestClient::new())
}

pub(super) fn session_operation_row(id: &str, session_id: &str, kind: &str) -> SessionOperationRow {
    SessionOperationRow {
        cancel_requested: false,
        finished_at: None,
        heartbeat_at: None,
        id: id.to_string(),
        kind: kind.to_string(),
        last_error: None,
        queued_at: 0,
        session_id: session_id.to_string(),
        started_at: None,
        status: "queued".to_string(),
    }
}

pub(super) fn empty_transcript() -> Arc<Mutex<SessionTranscript>> {
    Arc::new(Mutex::new(SessionTranscript::default()))
}

/// Builds rebase assistance input with the provided git client for unit
/// tests.
pub(super) async fn build_rebase_assist_input_for_test(
    git_client: Arc<dyn GitClient>,
) -> (TempDir, RebaseAssistInput) {
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let folder = temp_dir.path().to_path_buf();

    (
        temp_dir,
        RebaseAssistInput {
            app_event_tx,
            assist_mode: RebaseAssistMode::OneShot,
            child_pid: Arc::new(Mutex::new(None)),
            db,
            folder,
            fs_client: test_fs_client(),
            git_client,
            id: "session-123".into(),
            one_shot_client: test_one_shot_client(),
            transcript: empty_transcript(),
            rebase_plan: RebasePlan::target("main".to_string()),
            session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
            session_update_versions: Arc::default(),
        },
    )
}

/// Builds merge-task input with injected git client for deterministic
/// workflow tests.
pub(super) async fn build_merge_task_input_for_test(
    git_client: Arc<dyn GitClient>,
) -> (TempDir, MergeTaskInput) {
    let (app_event_tx, _app_event_rx) = mpsc::unbounded_channel();
    let db = AppRepositories::in_memory().await.expect("db should open");
    let temp_dir = tempdir().expect("failed to create temporary test directory");
    let folder = temp_dir.path().join("session-worktree");
    let repo_root = temp_dir.path().join("repo-root");

    (
        temp_dir,
        MergeTaskInput {
            app_event_tx,
            archive_diff: false,
            base_branch: "main".to_string(),
            child_pid: Arc::new(Mutex::new(None)),
            clock: Arc::new(crate::infra::clock::RealClock),
            db,
            folder,
            fs_client: test_fs_client(),
            git_client,
            id: "session-123".into(),
            one_shot_client: test_one_shot_client(),
            transcript: empty_transcript(),
            repo_root,
            session_update_versions: Arc::default(),
            session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
            source_branch: "wt/session-123".to_string(),
            status: Arc::new(Mutex::new(Status::Merging)),
        },
    )
}

pub(super) async fn assert_managed_merge_metadata(
    db: &AppRepositories,
    expected_merged_commit_hash: &str,
) {
    let merged_commit_hash = db
        .sessions()
        .load_session_merged_commit_hash("session-123")
        .await
        .expect("failed to load merged commit hash");
    let archived_diff = db
        .sessions()
        .load_session_archived_diff("session-123")
        .await
        .expect("failed to load archived diff");

    assert_eq!(
        merged_commit_hash.as_deref(),
        Some(expected_merged_commit_hash)
    );
    assert_eq!(archived_diff.as_deref(), Some("diff --git a/file b/file"));
}

/// Builds sync rebase assistance input with injected git and assistance
/// clients for project-level conflict tests.
pub(super) fn build_sync_rebase_input_for_test(
    folder: PathBuf,
    git_client: Arc<dyn GitClient>,
    sync_assist_client: Arc<dyn SyncAssistClient>,
) -> SyncRebaseAssistInput {
    SyncRebaseAssistInput {
        event_context: None,
        base_branch: "main".to_string(),
        folder,
        fs_client: test_fs_client(),
        git_client,
        session_agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
        sync_assist_client,
    }
}

/// Builds a git client mock for a successful project sync that stops on
/// one conflict, receives assistance, and then pushes.
pub(super) fn successful_sync_conflict_git_client() -> git::MockGitClient {
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_find_git_repo_root()
        .times(1)
        .returning(|folder| Box::pin(async move { Some(folder) }));
    mock_git_client
        .expect_is_worktree_clean()
        .times(1)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock_git_client
        .expect_get_ahead_behind()
        .times(1)
        .return_once(|_| Box::pin(async { Ok((1, 2)) }));
    mock_git_client
        .expect_list_upstream_commit_titles()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Ok(vec![
                    "Update changelog format".to_string(),
                    "Fix sync status copy".to_string(),
                ])
            })
        });
    mock_git_client
        .expect_get_ahead_behind()
        .times(1)
        .return_once(|_| Box::pin(async { Ok((1, 0)) }));
    mock_git_client
        .expect_list_local_commit_titles()
        .times(1)
        .returning(|_| Box::pin(async { Ok(vec!["Refine sync conflict messaging".to_string()]) }));
    mock_git_client
        .expect_pull_rebase()
        .times(1)
        .returning(|_| {
            Box::pin(async {
                Ok(git::PullRebaseResult::Conflict {
                    detail: "CONFLICT (content): Merge conflict in src/lib.rs".to_string(),
                })
            })
        });
    mock_git_client
        .expect_list_conflicted_files()
        .times(1)
        .returning(|_| Box::pin(async { Ok(vec!["src/lib.rs".to_string()]) }));
    mock_git_client
        .expect_list_staged_conflict_marker_files()
        .times(2)
        .returning(|_, _| Box::pin(async { Ok(vec![]) }));
    mock_git_client
        .expect_stage_all()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_has_unmerged_paths()
        .times(1)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock_git_client
        .expect_run_pre_commit_hook()
        .times(1)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_git_client
        .expect_rebase_continue()
        .times(1)
        .returning(|_| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock_git_client
        .expect_push_current_branch()
        .times(1)
        .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
    mock_git_client.expect_abort_rebase().times(0);

    mock_git_client
}

/// Returns the expected result for the successful assisted sync fixture.
pub(super) fn successful_sync_conflict_outcome() -> SyncMainOutcome {
    SyncMainOutcome {
        default_branch: "main".to_string(),
        deferred_merged_session_ids: Vec::new(),
        pulled_commit_titles: vec![
            "Update changelog format".to_string(),
            "Fix sync status copy".to_string(),
        ],
        pulled_commits: Some(2),
        pushed_commit_titles: vec!["Refine sync conflict messaging".to_string()],
        pushed_commits: Some(1),
        resolved_conflict_files: vec!["src/lib.rs".to_string()],
    }
}

/// Returns a git mock whose published-branch push waits for an explicit
/// release after notifying the test that push execution has started.
pub(super) fn blocking_auto_push_git_client(
    push_started: Arc<tokio::sync::Notify>,
    release_push: Arc<tokio::sync::Notify>,
) -> git::MockGitClient {
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess-reb".to_string()) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(|session_folder, remote_branch_name| {
            session_folder.ends_with("sess-rebase") && remote_branch_name == "wt/sess-rebase"
        })
        .returning(move |_, _| {
            let push_started = Arc::clone(&push_started);
            let release_push = Arc::clone(&release_push);

            Box::pin(async move {
                push_started.notify_one();
                release_push.notified().await;

                Ok("origin/wt/sess-rebase".to_string())
            })
        });

    mock_git_client
}

/// Inserts one published rebasing session linked to an open GitHub review
/// request.
pub(super) async fn insert_published_rebase_session_with_review_request(db: &AppRepositories) {
    let project_id = db
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    db.sessions()
        .insert_session(
            "sess-rebase",
            "gemini-3.8-flash",
            "main",
            "Rebasing",
            project_id,
        )
        .await
        .expect("failed to insert session");
    db.sessions()
        .update_session_published_upstream_ref(
            "sess-rebase",
            Some("origin/wt/sess-rebase".to_string()),
        )
        .await
        .expect("failed to set published upstream ref");
    db.reviews()
        .update_session_review_request("sess-rebase", Some(linked_github_review_request()))
        .await
        .expect("failed to persist review request");
}

/// Returns one linked GitHub review request fixture for metadata-sync
/// tests.
pub(super) fn linked_github_review_request() -> crate::domain::session::ReviewRequest {
    crate::domain::session::ReviewRequest {
        last_refreshed_at: 100,
        summary: forge::ReviewRequestSummary {
            display_id: "#42".to_string(),
            forge_kind: forge::ForgeKind::GitHub,
            source_branch: "wt/sess-rebase".to_string(),
            state: crate::domain::session::ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Old title".to_string(),
            web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
        },
    }
}

/// Returns one git client mock for post-rebase review-request metadata
/// sync.
pub(super) fn metadata_sync_git_client() -> git::MockGitClient {
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_head_commit_message()
        .once()
        .withf(|session_folder| session_folder.ends_with("sess-rebase"))
        .returning(|_| {
            Box::pin(async {
                Ok(Some(
                    "Refresh queued sync metadata\n\n- Preserve sync details.".to_string(),
                ))
            })
        });
    mock_git_client
        .expect_in_progress_operation()
        .once()
        .returning(|_| Box::pin(async { Ok(None) }));
    mock_git_client
        .expect_detect_git_info()
        .once()
        .returning(|_| Box::pin(async { Some("wt/sess-reb".to_string()) }));
    mock_git_client
        .expect_push_current_branch_to_remote_branch()
        .once()
        .withf(|session_folder, remote_branch_name| {
            session_folder.ends_with("sess-rebase") && remote_branch_name == "wt/sess-rebase"
        })
        .returning(|_, _| Box::pin(async { Ok("origin/wt/sess-rebase".to_string()) }));
    mock_git_client.expect_repo_url().once().returning(|_| {
        Box::pin(async { Ok("https://github.com/agentty-xyz/agentty.git".to_string()) })
    });

    mock_git_client
}

/// Returns one GitHub forge remote fixture for metadata-sync tests.
pub(super) fn github_forge_remote() -> forge::ForgeRemote {
    forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: forge::ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty.git".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    }
}

/// Returns one review-request client mock for post-rebase metadata sync.
pub(super) fn metadata_sync_review_request_client(
    folder: PathBuf,
) -> forge::MockReviewRequestClient {
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .returning(|_| Ok(github_forge_remote()));
    mock_review_request_client
        .expect_review_request_metadata()
        .once()
        .returning(|_, _| {
            Box::pin(async {
                Ok(forge::ReviewRequestMetadata {
                    body: "Old details.".to_string(),
                    title: "Old title".to_string(),
                })
            })
        });
    mock_review_request_client
        .expect_sync_review_request_metadata()
        .once()
        .withf(move |remote, display_id, input| {
            remote.command_working_directory.as_deref() == Some(folder.as_path())
                && display_id == "#42"
                && input.title.as_ref().is_some_and(|title| {
                    title.current == "Old title" && title.desired == "Old title"
                })
                && input.body.as_ref().is_some_and(|body| {
                    body.current == "Old details."
                        && body.desired == "Old details.\n\n- Preserve sync details."
                })
        })
        .returning(|_, _, _| {
            Box::pin(async {
                Ok(forge::ReviewRequestSummary {
                    display_id: "#42".to_string(),
                    forge_kind: forge::ForgeKind::GitHub,
                    source_branch: "wt/sess-rebase".to_string(),
                    state: crate::domain::session::ReviewRequestState::Open,
                    status_summary: Some("Open".to_string()),
                    target_branch: "main".to_string(),
                    title: "Old title".to_string(),
                    web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
                })
            })
        });

    mock_review_request_client
}

/// Returns one semantic metadata evaluator for post-rebase sync.
pub(super) fn metadata_sync_one_shot_client() -> Arc<dyn OneShotClient> {
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client.expect_submit().once().returning(|_| {
            Ok(agent::OneShotSubmission {
                response: ag_protocol::AgentResponse::plain(
                    r#"{"title":"Old title","description":"Old details.\n\n- Preserve sync details.","is_title_change_significant":false}"#,
                ),
                stats: agent::SessionStats::default(),
            })
        });

    Arc::new(one_shot_client)
}

/// Collects the in-progress and terminal published-branch sync states.
pub(super) async fn collect_published_branch_sync_statuses(
    app_event_rx: &mut mpsc::UnboundedReceiver<AppEvent>,
) -> Vec<PublishedBranchSyncStatus> {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        let mut sync_events = Vec::new();
        while sync_events.len() < 2 {
            let event = app_event_rx.recv().await.expect("missing app event");
            if let AppEvent::PublishedBranchSyncUpdated { sync_status, .. } = event {
                sync_events.push(sync_status);
            }
        }

        sync_events
    })
    .await
    .expect("timed out waiting for sync events")
}
