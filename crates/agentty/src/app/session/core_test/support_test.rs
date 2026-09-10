use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ag_agent::{AppServerClient, MockAgentBackend, MockOneShotClient};
use ag_forge::{
    ReviewComment, ReviewCommentAnchorSide, ReviewCommentSnapshot, ReviewCommentThread,
};
use ag_git as git;
use ag_protocol::AgentResponse;

use super::super::{SessionDefaults, SessionManager, session_folder};
use crate::app::{App, AppServices, ReviewCacheEntry, SessionState};
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::permission::PermissionMode;
use crate::domain::session::{
    SESSION_DATA_DIR, Session, SessionHandles, SessionId, SessionRole, SessionSize, SessionStats,
    Status,
};
use crate::domain::session_message::SessionTranscript;
use crate::domain::transient_message::{
    TransientMessageBody, TransientMessageSlot, TransientMessageStore,
};
use crate::infra::clock::{Clock, RealClock};
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::infra::fs::FsClient;

/// Builds one loading focused-review entry with a stable test profile.
pub(super) fn test_loading_review(diff_hash: u64) -> ReviewCacheEntry {
    ReviewCacheEntry::Loading {
        diff_hash,
        review_agent: (
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            ReasoningLevel::High,
            SpeedMode::Normal,
        ),
    }
}

/// Builds a filesystem mock that delegates operations to local disk.
pub(super) fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_cleanup_agent_artifacts()
        .returning(|root| fs::FsClient::cleanup_agent_artifacts(&fs::RealFsClient, root));
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
        .returning(|path| {
            Box::pin(async move {
                match tokio::fs::remove_file(path).await {
                    Ok(()) => Ok(()),
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                    Err(error) => Err(fs::FsError::from(error)),
                }
            })
        });
    mock_fs_client
        .expect_is_dir()
        .times(0..)
        .returning(|path| path.is_dir());

    mock_fs_client
}

/// Mutable clock used to test cache TTL behavior without sleeping.
pub(super) struct TestClock {
    pub(super) instant: Mutex<Instant>,
    pub(super) system_time: Mutex<SystemTime>,
}

impl TestClock {
    /// Creates a clock pinned to the provided time pair.
    pub(super) fn new(instant: Instant, system_time: SystemTime) -> Self {
        Self {
            instant: Mutex::new(instant),
            system_time: Mutex::new(system_time),
        }
    }

    /// Advances both clock domains by the provided duration.
    pub(super) fn advance(&self, duration: Duration) {
        if let Ok(mut instant) = self.instant.lock() {
            *instant += duration;
        }

        if let Ok(mut system_time) = self.system_time.lock() {
            *system_time += duration;
        }
    }
}

impl Clock for TestClock {
    fn now_instant(&self) -> Instant {
        *self
            .instant
            .lock()
            .expect("test clock instant lock should not be poisoned")
    }

    fn now_system_time(&self) -> SystemTime {
        *self
            .system_time
            .lock()
            .expect("test clock system-time lock should not be poisoned")
    }
}

pub(super) fn create_mock_backend() -> MockAgentBackend {
    let mut mock = MockAgentBackend::new();
    mock.expect_build_command().returning(|request| {
        let mut cmd = Command::new("sh");
        cmd.arg("-c")
            .arg("printf '{\"answer\":\"mock-start\",\"questions\":[]}'")
            .current_dir(request.folder)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        Ok(cmd)
    });
    mock
}

/// Allows branch discovery calls to fall back to defaults in tests that do
/// not care about exact detected refs.
pub(super) fn allow_detect_git_info(mock: &mut git::MockGitClient) {
    allow_detect_git_info_with_head_hash(mock, true);
}

/// Expects one successful advisory pre-commit readiness check.
pub(super) fn expect_pre_commit_hook_ready(mock: &mut git::MockGitClient) {
    mock.expect_check_pre_commit_hook_ready()
        .once()
        .returning(|_| Box::pin(async { Ok(()) }));
}

pub(super) fn allow_detect_git_info_with_head_hash(
    mock: &mut git::MockGitClient,
    allow_head_hash: bool,
) {
    mock.expect_detect_git_info().times(0..).returning(|path| {
        let branch_name = path
            .file_name()
            .and_then(|file_name| file_name.to_str())
            .filter(|folder_name| folder_name.len() == 8)
            .map_or_else(
                || "main".to_string(),
                |folder_name| format!("wt/{folder_name}"),
            );

        Box::pin(async move { Some(branch_name) })
    });
    mock.expect_main_repo_root().times(0..).returning(|path| {
        let repo_root = path
            .parent()
            .map(std::path::Path::to_path_buf)
            .unwrap_or(path);

        Box::pin(async move { Ok(repo_root) })
    });
    mock.expect_main_checkout_working_tree()
        .times(0..)
        .returning(|path| {
            let repo_root = path
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or(path);

            Box::pin(async move { Ok(Some(repo_root)) })
        });
    mock.expect_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock.expect_tracked_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    if allow_head_hash {
        mock.expect_head_hash()
            .times(0..)
            .returning(|_| Box::pin(async { Ok("main-before".to_string()) }));
    }
    mock.expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock.expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock.expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    mock.expect_get_ahead_behind()
        .times(0..)
        .returning(|_| Box::pin(async { Ok((0, 0)) }));
}

/// Builds a merge-focused mock git client for no-op merge scenarios,
/// including both main-checkout preflight and session-worktree clean
/// checks for each merge.
pub(super) fn create_mock_git_client_for_successful_noop_merges(
    expected_merge_count: usize,
    repo_root: PathBuf,
) -> git::MockGitClient {
    let mut mock = git::MockGitClient::new();
    allow_detect_git_info(&mut mock);
    mock.expect_find_git_repo_root()
        .times(expected_merge_count)
        .returning(move |_| {
            let repo_root = repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock.expect_is_worktree_clean()
        .times(expected_merge_count * 2)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock.expect_is_rebase_in_progress()
        .times(expected_merge_count)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock.expect_rebase_start()
        .times(expected_merge_count)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock.expect_squash_merge_diff()
        .times(expected_merge_count)
        .returning(|_, _, _| Box::pin(async { Ok(String::new()) }));
    mock.expect_remove_worktree()
        .times(expected_merge_count)
        .returning(|worktree_path| {
            Box::pin(async move {
                let fs_client = create_passthrough_mock_fs_client();
                let _ = fs_client.remove_dir_all(worktree_path).await;

                Ok(())
            })
        });
    mock.expect_delete_branch()
        .times(expected_merge_count)
        .returning(|_, _| Box::pin(async { Ok(()) }));

    mock
}

/// Builds a permissive mock git client for session tests.
///
/// The mock returns successful defaults and performs lightweight
/// filesystem side effects for worktree creation/removal.
pub(super) fn create_default_mock_git_client(repo_root: PathBuf) -> git::MockGitClient {
    let mut mock = git::MockGitClient::new();

    setup_mock_worktree_expectations(&mut mock, repo_root);
    setup_mock_merge_and_rebase_expectations(&mut mock);
    setup_mock_commit_and_branch_expectations(&mut mock);

    mock
}

/// Configures worktree, repo discovery, and remote expectations.
pub(super) fn setup_mock_worktree_expectations(mock: &mut git::MockGitClient, repo_root: PathBuf) {
    let find_repo_root = repo_root.clone();

    mock.expect_detect_git_info().times(0..).returning({
        let repo_root = repo_root.clone();

        move |path| {
            let branch_name = if path == repo_root {
                "main".to_string()
            } else {
                path.file_name()
                    .and_then(|file_name| file_name.to_str())
                    .map_or_else(
                        || "main".to_string(),
                        |folder_name| format!("wt/{folder_name}"),
                    )
            };

            Box::pin(async move { Some(branch_name) })
        }
    });
    mock.expect_current_upstream_reference()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
    mock.expect_find_git_repo_root()
        .times(0..)
        .returning(move |_| {
            let repo_root = find_repo_root.clone();
            Box::pin(async move { Some(repo_root) })
        });
    mock.expect_create_worktree()
        .times(0..)
        .returning(|_, worktree_path, _, _| {
            Box::pin(async move {
                let fs_client = create_passthrough_mock_fs_client();
                fs_client
                    .create_dir_all(worktree_path.clone())
                    .await
                    .map_err(|error| {
                        git::GitError::OutputParse(format!(
                            "Failed to create mock worktree directory: {error}"
                        ))
                    })?;
                fs_client
                    .create_dir_all(worktree_path.join(SESSION_DATA_DIR))
                    .await
                    .map_err(|error| {
                        git::GitError::OutputParse(format!(
                            "Failed to create mock session data directory: {error}"
                        ))
                    })?;

                Ok(())
            })
        });
    mock.expect_remove_worktree()
        .times(0..)
        .returning(|worktree_path| {
            Box::pin(async move {
                let fs_client = create_passthrough_mock_fs_client();
                let _ = fs_client.remove_dir_all(worktree_path).await;

                Ok(())
            })
        });
    mock.expect_pull_rebase().times(0..).returning(|_| {
        Box::pin(async {
            Err(git::GitError::OutputParse(
                "No upstream branch configured for pull".to_string(),
            ))
        })
    });
    mock.expect_push_current_branch()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("origin/main".to_string()) }));
    mock.expect_fetch_remote()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock.expect_branch_tracking_statuses()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(HashMap::new()) }));
    mock.expect_get_ahead_behind()
        .times(0..)
        .returning(|_| Box::pin(async { Ok((0, 0)) }));
    mock.expect_get_ref_ahead_behind()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok((0, 0)) }));
    mock.expect_list_upstream_commit_titles()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock.expect_list_local_commit_titles()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
    mock.expect_repo_url()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("https://example.invalid/repo.git".to_string()) }));
    expect_shared_repo_resolvers(mock, repo_root);
}

/// Registers the admin-root and main-working-checkout resolver expectations
/// backed by the same `repo_root`, treating the shared repository as non-bare.
pub(super) fn expect_shared_repo_resolvers(mock: &mut git::MockGitClient, repo_root: PathBuf) {
    mock.expect_main_checkout_working_tree()
        .times(0..)
        .returning({
            let repo_root = repo_root.clone();

            move |_| {
                let repo_root = repo_root.clone();
                Box::pin(async move { Ok(Some(repo_root)) })
            }
        });
    mock.expect_main_repo_root().times(0..).returning(move |_| {
        let repo_root = repo_root.clone();
        Box::pin(async move { Ok(repo_root) })
    });
}

/// Configures merge, rebase, and conflict resolution expectations.
pub(super) fn setup_mock_merge_and_rebase_expectations(mock: &mut git::MockGitClient) {
    mock.expect_squash_merge_diff()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok(String::new()) }));
    mock.expect_squash_merge()
        .times(0..)
        .returning(|_, _, _, _| Box::pin(async { Ok(git::SquashMergeOutcome::Committed) }));
    mock.expect_rebase()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    mock.expect_rebase_start()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock.expect_rebase_onto_start()
        .times(0..)
        .returning(|_, _, _| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock.expect_run_pre_commit_hook()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock.expect_rebase_continue()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(git::RebaseStepResult::Completed) }));
    mock.expect_abort_rebase()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock.expect_is_rebase_in_progress()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock.expect_has_unmerged_paths()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(false) }));
    mock.expect_list_staged_conflict_marker_files()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));
    mock.expect_list_conflicted_files()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(Vec::new()) }));
}

/// Builds a synthetic git-diff payload from a session worktree.
///
/// Production tests rely on file-edit volume to estimate session size.
/// This helper counts lines in non-metadata files so mocked git clients
/// can still drive size-related assertions without invoking shell `git`.
pub(super) async fn synthetic_diff_from_session_folder(folder: &Path) -> String {
    let fs_client = create_passthrough_mock_fs_client();
    let line_count = count_non_metadata_lines(&fs_client, folder).await;

    synthetic_added_line_diff(line_count)
}

/// Counts lines across one worktree while ignoring session metadata.
pub(super) async fn count_non_metadata_lines(fs_client: &dyn fs::FsClient, root: &Path) -> usize {
    let mut pending_entries = vec![root.to_path_buf()];
    let mut line_count = 0;

    while let Some(entry) = pending_entries.pop() {
        if !fs_client.is_dir(entry.clone()) {
            line_count += count_file_lines(fs_client, &entry).await;

            continue;
        }

        if is_session_metadata_dir(&entry) {
            continue;
        }

        pending_entries.extend(child_paths(&entry));
    }

    line_count
}

/// Counts UTF-8-lossy text lines in one file, returning zero on read error.
pub(super) async fn count_file_lines(fs_client: &dyn fs::FsClient, path: &Path) -> usize {
    fs_client
        .read_file(path.to_path_buf())
        .await
        .map_or(0, |content| {
            String::from_utf8_lossy(&content).lines().count()
        })
}

/// Returns whether `path` points at Agentty's session metadata directory.
pub(super) fn is_session_metadata_dir(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name == SESSION_DATA_DIR)
}

/// Returns direct child paths for a directory, or an empty list if
/// unreadable.
pub(super) fn child_paths(path: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(path).map_or_else(
        |_| Vec::new(),
        |entries| {
            entries
                .filter_map(Result::ok)
                .map(|dir_entry| dir_entry.path())
                .collect()
        },
    )
}

/// Builds a git-diff body with one added-line marker per counted line.
pub(super) fn synthetic_added_line_diff(line_count: usize) -> String {
    match line_count {
        0 => String::new(),
        _ => "+\n".repeat(line_count),
    }
}

/// Configures commit, staging, and branch operation expectations.
pub(super) fn setup_mock_commit_and_branch_expectations(mock: &mut git::MockGitClient) {
    mock.expect_commit_all()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    mock.expect_commit_all_preserving_single_commit()
        .times(0..)
        .returning(|_, _, _, _| {
            Box::pin(async {
                Err(git::GitError::OutputParse(
                    "Nothing to commit: no changes detected".to_string(),
                ))
            })
        });
    mock.expect_stage_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock.expect_head_short_hash()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("abc1234".to_string()) }));
    mock.expect_head_hash()
        .times(0..)
        .returning(|_| Box::pin(async { Ok("parent-tip".to_string()) }));
    mock.expect_ref_hash()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok("parent-tip".to_string()) }));
    mock.expect_delete_branch()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(()) }));
    mock.expect_diff().times(0..).returning(|folder, _| {
        Box::pin(async move { Ok(synthetic_diff_from_session_folder(&folder).await) })
    });
    mock.expect_is_worktree_clean()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(true) }));
    mock.expect_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock.expect_tracked_worktree_status()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(String::new()) }));
    mock.expect_has_commits_since()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(false) }));
    mock.expect_head_commit_message()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(None) }));
}

/// Replaces app-level git dependencies with the provided mock client.
pub(super) fn install_mock_git_client(app: &mut App, mock_git_client: git::MockGitClient) {
    let mock_git_client: Arc<dyn git::GitClient> = Arc::new(mock_git_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let app_server_client_override = app.services.app_server_client_override();
    let fs_client = app.services.fs_client();
    let review_request_client = app.services.review_request_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        crate::app::service::AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client: Arc::clone(&mock_git_client),
            one_shot_client_override: Some(auto_commit_one_shot_client()),
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
    app.sessions.git_client = mock_git_client;
}

/// Builds a deterministic one-shot boundary for app-level auto-commit tests.
pub(super) fn auto_commit_one_shot_client() -> Arc<dyn ag_agent::OneShotClient> {
    let mut one_shot_client = MockOneShotClient::new();
    one_shot_client
        .expect_submit()
        .times(0..)
        .returning(|request| {
            if request
                .prompt
                .contains("Generate a concise, commit-style title")
            {
                return Err(ag_agent::OneShotError::new(
                    "title generation is disabled in this fixture",
                ));
            }

            Ok(ag_agent::OneShotSubmission {
                response: AgentResponse::plain("Existing session commit"),
                stats: ag_agent::SessionStats {
                    added_lines: 0,
                    deleted_lines: 0,
                    diff_state: ag_agent::SessionDiffState::Unknown,
                    input_tokens: 0,
                    output_tokens: 0,
                },
            })
        });

    Arc::new(one_shot_client)
}

/// Builds a test app with a caller-provided database, git context, and
/// app-server boundary.
pub(super) async fn new_test_app_with_db_and_app_server(
    path: PathBuf,
    working_dir: PathBuf,
    git_branch: Option<String>,
    db: AppRepositories,
    app_server_client: Arc<dyn AppServerClient>,
) -> App {
    let clients =
        crate::test_support::test_app_clients().with_app_server_client_override(app_server_client);
    let mut app = App::new_with_clients(path, working_dir.clone(), git_branch, db, clients)
        .await
        .expect("failed to build app");
    let mock_git_client = create_default_mock_git_client(working_dir);
    install_mock_git_client(&mut app, mock_git_client);

    app
}

/// Builds a test app with a caller-provided database and git context.
pub(super) async fn new_test_app_with_db(
    path: PathBuf,
    working_dir: PathBuf,
    git_branch: Option<String>,
    db: AppRepositories,
) -> App {
    new_test_app_with_db_and_app_server(
        path,
        working_dir,
        git_branch,
        db,
        crate::test_support::mock_app_server(),
    )
    .await
}

/// Builds a test app rooted at `path` with no branch-specific git context.
pub(super) async fn new_test_app(path: PathBuf) -> App {
    let working_dir = PathBuf::from("/tmp/test");
    let db = AppRepositories::in_memory().await.expect("db should open");

    new_test_app_with_db(path, working_dir, None, db).await
}

/// Builds a test app rooted at `path` with mock git branch context.
pub(super) async fn new_test_app_with_git(path: &Path) -> App {
    let db = AppRepositories::in_memory().await.expect("db should open");
    new_test_app_with_git_and_db(path, db).await
}

/// Builds a test app rooted at `path` with mock git branch context and a
/// caller-provided database handle.
pub(super) async fn new_test_app_with_git_and_db(path: &Path, db: AppRepositories) -> App {
    new_test_app_with_db(
        path.to_path_buf(),
        path.to_path_buf(),
        Some("main".to_string()),
        db,
    )
    .await
}

/// Adds a manual review session snapshot for tests that do not require
/// status customization.
pub(super) fn add_manual_session(app: &mut App, base_path: &Path, id: &str, prompt: &str) {
    add_manual_session_with_status(app, base_path, id, prompt, Status::Review);
}

/// Adds a manual session snapshot with an explicit status.
pub(super) fn add_manual_session_with_status(
    app: &mut App,
    base_path: &Path,
    id: &str,
    prompt: &str,
    status: Status,
) {
    let folder = session_folder(base_path, id);
    let data_dir = folder.join(SESSION_DATA_DIR);
    std::fs::create_dir_all(&data_dir).expect("failed to create data dir");
    app.sessions
        .session_handles_mut()
        .insert(id.to_string().into(), SessionHandles::new(status));
    app.sessions.push_session(Session {
        base_branch: "main".to_string(),
        created_at: 0,
        draft_attachments: Vec::new(),
        folder,
        follow_up_tasks: Vec::new(),
        id: id.into(),
        in_progress_started_at: None,
        in_progress_total_seconds: 0,
        is_draft: false,
        controller_session_id: None,
        orchestration_progress: None,
        role: SessionRole::default(),
        agent: crate::domain::agent::AgentSelection::new(
            crate::domain::agent::AgentKind::Antigravity,
            crate::domain::agent::AgentModel::Gemini38Flash,
        ),
        parent_session_id: None,
        permission_mode: PermissionMode::AutoEdit,
        personality_id: None,
        project_name: String::new(),
        prompt: prompt.to_string(),
        queued_messages: Vec::new(),
        reasoning_level_override: None,
        response_style: crate::domain::agent::ResponseStyle::default(),
        published_upstream_ref: None,
        questions: Vec::new(),
        review_request: None,
        size: SessionSize::Xs,
        speed_mode: crate::domain::agent::SpeedMode::default(),
        stats: SessionStats::default(),
        status,
        title: Some(prompt.to_string()),
        transcript: None,
        updated_at: 0,
        transient_messages: TransientMessageStore::default(),
    });
    if app.sessions.selected_session_index().is_none() {
        app.sessions.select_session_index(Some(0));
    }
}

/// Builds a minimal `SessionManager` for reducer tests that only need one
/// in-memory session snapshot.
pub(super) fn test_session_manager(
    session_id: &str,
    reasoning_level_override: Option<ReasoningLevel>,
) -> SessionManager {
    test_session_manager_with_clock(session_id, reasoning_level_override, Arc::new(RealClock))
}

/// Builds a minimal `SessionManager` using the provided clock.
pub(super) fn test_session_manager_with_clock(
    session_id: &str,
    reasoning_level_override: Option<ReasoningLevel>,
    clock: Arc<dyn Clock>,
) -> SessionManager {
    let mut handles = HashMap::new();
    handles.insert(
        session_id.to_string().into(),
        SessionHandles::new(Status::Review),
    );

    let state = SessionState::new(
        handles,
        vec![Session {
            base_branch: "main".to_string(),
            created_at: 0,
            draft_attachments: Vec::new(),
            folder: PathBuf::from(format!("/tmp/{session_id}")),
            follow_up_tasks: Vec::new(),
            id: session_id.into(),
            in_progress_started_at: None,
            in_progress_total_seconds: 0,
            is_draft: false,
            controller_session_id: None,
            orchestration_progress: None,
            role: SessionRole::default(),
            agent: crate::domain::agent::AgentSelection::new(
                crate::domain::agent::AgentKind::Codex,
                AgentModel::Gpt56Sol,
            ),
            parent_session_id: None,
            permission_mode: PermissionMode::AutoEdit,
            personality_id: None,
            project_name: "project".to_string(),
            prompt: String::new(),
            queued_messages: Vec::new(),
            reasoning_level_override,
            response_style: crate::domain::agent::ResponseStyle::default(),
            published_upstream_ref: None,
            questions: Vec::new(),
            review_request: None,
            size: SessionSize::Xs,
            speed_mode: crate::domain::agent::SpeedMode::default(),
            stats: SessionStats::default(),
            status: Status::Review,
            title: Some("Title".to_string()),
            transcript: None,
            updated_at: 0,
            transient_messages: TransientMessageStore::default(),
        }],
        crate::domain::selection::SelectionState::default(),
        clock,
        1,
        0,
    );

    SessionManager::new(
        SessionDefaults {
            model: AgentModel::Gpt56Sol,
        },
        Arc::new(git::MockGitClient::new()),
        state,
        Vec::new(),
    )
}

/// Helper: creates a session and starts it with the given prompt (two-step
/// flow).
pub(super) async fn create_and_start_session(app: &mut App, prompt: &str) {
    let session_id = app
        .create_session()
        .await
        .expect("failed to create session");
    let start_backend = create_mock_backend();
    app.sessions
        .reply_with_backend(
            &app.services,
            &session_id,
            prompt,
            Arc::new(start_backend),
            AgentModel::ClaudeOpus5,
        )
        .await;
}

pub(super) async fn wait_for_status(app: &mut App, session_id: &str, expected: Status) {
    wait_for_status_with_retries(app, session_id, expected, 2000, false).await;
}

pub(super) async fn wait_for_status_with_retries(
    app: &mut App,
    session_id: &str,
    expected: Status,
    retries: usize,
    process_events_each_iteration: bool,
) {
    for _ in 0..retries {
        if process_events_each_iteration {
            app.process_pending_app_events().await;
        }
        app.sessions.sync_from_handles();
        let Some(session) = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            continue;
        };
        if session.status == expected {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session while waiting for status");
    assert_eq!(
        session.status,
        expected,
        "session transcript while waiting for status: {}",
        session_replay_text(session)
    );
}

pub(super) fn session_replay_text(session: &Session) -> String {
    session
        .transcript
        .as_ref()
        .and_then(SessionTranscript::replay_text)
        .unwrap_or_default()
}

/// Waits until background cleanup removes `path`.
pub(super) async fn wait_for_path_absent(path: &Path) {
    for _ in 0..500 {
        if !path.exists() {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }

    assert!(!path.exists(), "timed out waiting for path cleanup");
}

pub(super) async fn wait_for_output_contains(
    app: &mut App,
    session_id: &str,
    expected_output: &str,
    retries: usize,
) {
    for _ in 0..retries {
        app.sessions.sync_from_handles();
        let Some(session) = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            break;
        };
        if session_replay_text(session).contains(expected_output) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session while waiting for output");
    assert!(
        session_replay_text(session).contains(expected_output),
        "expected output to contain: {expected_output}, actual output: {}",
        session_replay_text(session)
    );
}

/// Waits for output while draining app events that may start follow-up
/// background work.
pub(super) async fn wait_for_output_contains_after_events(
    app: &mut App,
    session_id: &str,
    expected_output: &str,
    retries: usize,
) {
    for _ in 0..retries {
        app.process_pending_app_events().await;
        app.sessions.sync_from_handles();
        let Some(session) = app
            .sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
        else {
            break;
        };
        if session_replay_text(session).contains(expected_output) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    app.process_pending_app_events().await;
    app.sessions.sync_from_handles();
    let session = app
        .sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .expect("missing session while waiting for output");
    assert!(
        session_replay_text(session).contains(expected_output),
        "expected output to contain: {expected_output}, actual output: {}",
        session_replay_text(session)
    );
}

/// Returns the current session status or `Done` when session is missing.
pub(super) fn session_status_or_done(app: &App, session_id: &str) -> Status {
    app.sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .map_or(Status::Done, |session| session.status)
}

/// Returns whether a session currently has `Done` status.
pub(super) fn is_session_done(app: &App, session_id: &str) -> bool {
    app.sessions
        .sessions()
        .iter()
        .find(|session| session.id == session_id)
        .is_some_and(|session| session.status == Status::Done)
}

/// Waits for the first merge to finish and asserts second merge is queued
/// first instead of starting prematurely.
pub(super) async fn wait_for_first_merge_to_complete_before_second_starts(
    app: &mut App,
    first_session_id: &str,
    second_session_id: &str,
) {
    let mut first_merge_completed = false;
    let mut first_merge_pending_observed = false;
    let mut second_merge_was_queued = false;

    for _ in 0..5000 {
        app.process_pending_app_events().await;
        app.sessions.sync_from_handles();

        let first_status = session_status_or_done(app, first_session_id);
        let second_status = session_status_or_done(app, second_session_id);
        if second_status == Status::Queued {
            second_merge_was_queued = true;
        }
        if first_status == Status::Done {
            first_merge_completed = true;

            break;
        }
        first_merge_pending_observed = true;

        assert_ne!(
            second_status,
            Status::Merging,
            "second merge started before first completed"
        );

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(
        first_merge_completed,
        "first merge did not complete within timeout"
    );
    if first_merge_pending_observed {
        assert!(
            second_merge_was_queued,
            "second merge never entered queued status before first completed"
        );
    }
}

/// Waits for the queued second merge to enter `Merging` or `Done`.
pub(super) async fn wait_for_second_merge_to_start(app: &mut App, second_session_id: &str) {
    let mut second_merge_started = false;

    for _ in 0..5000 {
        app.process_pending_app_events().await;
        app.sessions.sync_from_handles();

        let second_status = session_status_or_done(app, second_session_id);
        if matches!(second_status, Status::Merging | Status::Done) {
            second_merge_started = true;

            break;
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }

    assert!(
        second_merge_started,
        "second merge did not start after first completed"
    );
}

/// Waits until both provided sessions are marked as `Done`.
pub(super) async fn wait_for_all_sessions_done(
    app: &mut App,
    first_session_id: &str,
    second_session_id: &str,
) {
    for _ in 0..5000 {
        app.process_pending_app_events().await;
        app.sessions.sync_from_handles();

        if is_session_done(app, first_session_id) && is_session_done(app, second_session_id) {
            return;
        }

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}

/// Prepares one review-ready session with persisted focused-review output.
pub(super) async fn prepare_review_comment_resolution_session(app: &mut App) -> SessionId {
    let session_id: SessionId = app
        .create_session()
        .await
        .expect("failed to create session")
        .into();
    app.sessions.sessions_mut()[0].status = Status::Review;
    if let Some(handles) = app.sessions.session_handles().get(session_id.as_str()) {
        *handles
            .status
            .lock()
            .expect("status lock should be available") = Status::Review;
    }
    app.services
        .db()
        .sessions()
        .update_session_status_with_timing_at(&session_id, "Review", 0)
        .await
        .expect("failed to persist review status");
    app.review_cache.insert(
        session_id.clone(),
        ReviewCacheEntry::Ready {
            diff_hash: 42,
            text: "Focused review".to_string(),
        },
    );
    app.services
        .db()
        .sessions()
        .update_session_focused_review(
            &session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some("42".to_string()),
            Some("Focused review".to_string()),
        )
        .await
        .expect("failed to persist focused review");

    session_id
}

/// Builds one actionable inline review thread for session-resolution tests.
pub(super) fn review_comment_resolution_snapshot() -> ReviewCommentSnapshot {
    ReviewCommentSnapshot {
        pr_level_comments: Vec::new(),
        threads: vec![ReviewCommentThread {
            anchor_side: ReviewCommentAnchorSide::New,
            comments: vec![ReviewComment {
                author: "reviewer".to_string(),
                authored_by_current_user: false,
                body: "Add validation.".to_string(),
            }],
            id: "thread-42".to_string(),
            is_outdated: Some(false),
            is_resolved: false,
            line: Some(12),
            path: "src/main.rs".to_string(),
            start_line: Some(11),
        }],
    }
}

pub(super) fn review_message_body<'a>(app: &'a App, session_id: &str) -> &'a TransientMessageBody {
    &app.sessions
        .session_or_err(session_id)
        .expect("review session should remain loaded")
        .transient_messages
        .get(TransientMessageSlot::Review)
        .expect("review output should remain visible after refresh")
        .body
}

/// Forces one session refresh to observe a failed primary row query.
pub(super) async fn refresh_with_session_table_unavailable(app: &mut App, pool: &sqlx::SqlitePool) {
    sqlx::query("ALTER TABLE session RENAME TO unavailable_session")
        .execute(pool)
        .await
        .expect("session table should become temporarily unavailable");
    app.refresh_sessions_now().await;
    sqlx::query("ALTER TABLE unavailable_session RENAME TO session")
        .execute(pool)
        .await
        .expect("session table should become available again");
}

pub(super) fn assert_sync_waits_without_canceling_turn(app: &mut App, session_id: &str) {
    app.sessions.sync_from_handles();
    assert_eq!(app.sessions.sessions()[0].status, Status::InProgress);
    let active_turn_was_cancelled = app
        .sessions
        .session_handles()
        .get(session_id)
        .expect("missing session handles")
        .cancel_token
        .lock()
        .expect("cancel token lock should not be poisoned")
        .is_cancelled();
    assert!(!active_turn_was_cancelled);
    assert!(!session_replay_text(&app.sessions.sessions()[0]).contains("Successfully synced"));
    assert!(matches!(
        app.sessions.sessions()[0]
            .transient_messages
            .get(TransientMessageSlot::SyncQueue)
            .map(|message| &message.body),
        Some(TransientMessageBody::Queued(_))
    ));
}

impl SessionManager {
    /// Appends one loaded session snapshot and updates stable id lookups.
    pub(crate) fn push_session(&mut self, session: Session) {
        self.state.push_session(session);
    }

    /// Synchronizes all loaded session snapshots from live runtime handles.
    pub(crate) fn sync_from_handles(&mut self) {
        self.state.sync_from_handles();
    }

    /// Returns mutable runtime handles keyed by stable session id.
    pub(crate) fn session_handles_mut(
        &mut self,
    ) -> &mut HashMap<SessionId, crate::domain::session::SessionHandles> {
        self.state.handles_mut()
    }
}
