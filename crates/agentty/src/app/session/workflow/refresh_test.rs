use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

use ag_forge as forge;
use ag_git as git;
use tempfile::tempdir;
use tokio::sync::mpsc;

use super::super::SESSION_REFRESH_INTERVAL;
use crate::app::session::{Clock, SessionDefaults};
use crate::app::{AppServices, ProjectManager, SessionManager, SessionState};
use crate::domain::agent::AgentKind;
use crate::domain::input::InputState;
use crate::domain::selection::SelectionState;
use crate::domain::session::{
    ForgeKind, PublishBranchAction, ReviewRequest, ReviewRequestState, ReviewRequestSummary,
    Session, SessionHandles, SessionId, Status,
};
use crate::infra::db::AppRepositories;
use crate::infra::fs;
use crate::presentation::app_mode::{
    AppMode, ConfirmationViewMode, DiffFocus, DiffLineComments, DiffPreview, DiffSidebarFocus,
    HelpContext,
};
use crate::presentation::help_action::ViewSessionState;

/// Builds a filesystem mock that delegates directory checks to local disk.
fn create_passthrough_mock_fs_client() -> fs::MockFsClient {
    let mut mock_fs_client = fs::MockFsClient::new();
    mock_fs_client
        .expect_create_dir_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
    mock_fs_client
        .expect_remove_dir_all()
        .times(0..)
        .returning(|_| Box::pin(async { Ok(()) }));
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

/// Persists one session row that matches the in-memory fixture.
async fn database_with_session(session: &Session) -> AppRepositories {
    let database = AppRepositories::in_memory().await.expect("db should open");
    let project_id = database
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to upsert project");
    database
        .sessions()
        .insert_session(
            &session.id,
            session.agent.model().as_str(),
            &session.base_branch,
            &session.status.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert session");
    if let Some(review_request) = &session.review_request {
        database
            .reviews()
            .update_session_review_request(&session.id, Some(review_request.clone()))
            .await
            .expect("failed to persist session review request");
    }

    database
}

/// Builds app services with caller-provided git and forge boundaries.
fn test_services(
    database: &AppRepositories,
    git_client: Arc<dyn git::GitClient>,
    review_request_client: Arc<dyn forge::ReviewRequestClient>,
) -> AppServices {
    let (event_tx, _event_rx) = mpsc::unbounded_channel();

    AppServices::new_with_agent_clis(
        PathBuf::from("/tmp/agentty-tests"),
        Arc::new(crate::infra::clock::RealClock),
        event_tx,
        crate::app::service::AppServiceDeps {
            app_server_client_override: Some(crate::test_support::mock_app_server()),
            available_agent_kinds: crate::domain::agent::AgentKind::ALL.to_vec(),
            clipboard_image_client_override: None,
            fs_client: Arc::new(create_passthrough_mock_fs_client()),
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: database.clone(),
            review_request_client,
        },
        crate::domain::agent::AgentCliInfo::from_kinds(crate::domain::agent::AgentKind::ALL),
    )
}

/// Builds one session manager with deterministic time and one session.
fn session_manager_with_session(clock: Arc<dyn Clock>, session: Session) -> SessionManager {
    let mut handles = HashMap::new();
    handles.insert(
        session.id.clone(),
        SessionHandles::new_with_transcript(
            session.status,
            session.transcript.clone().unwrap_or_default(),
        ),
    );

    SessionManager::new(
        SessionDefaults {
            model: AgentKind::Antigravity.default_model(),
        },
        Arc::new(git::MockGitClient::new()),
        SessionState::new(
            handles,
            vec![session],
            SelectionState::default(),
            clock,
            1,
            0,
        ),
        Vec::new(),
    )
}

/// Builds one session fixture with optional linked review-request data.
fn test_session(folder: PathBuf, review_request: Option<ReviewRequest>, status: Status) -> Session {
    crate::test_support::SessionFixtureBuilder::new()
        .folder(folder)
        .prompt("Implement forge review support")
        .review_request(review_request)
        .status(status)
        .title(Some("Add forge review support".to_string()))
        .build()
}

/// Builds one normalized GitHub review-request summary.
fn review_request_summary(display_id: &str, state: ReviewRequestState) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: display_id.to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session-".to_string(),
        state,
        status_summary: Some("Checks pending".to_string()),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
        web_url: format!(
            "https://github.com/agentty-xyz/agentty/pull/{}",
            &display_id[1..]
        ),
    }
}

#[tokio::test]
async fn review_request_remote_uses_live_worktree_for_detected_remote() {
    // Arrange
    let session_folder = PathBuf::from("/tmp/session");
    let session = test_session(session_folder.clone(), None, Status::Review);
    let database = database_with_session(&session).await;
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let clock: Arc<dyn Clock> = fake_clock;
    let session_manager = session_manager_with_session(clock, session);
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_repo_url()
        .once()
        .withf({
            let session_folder = session_folder.clone();
            move |candidate_folder| candidate_folder == &session_folder
        })
        .returning(|_| {
            Box::pin(async { Ok("https://github.com/agentty-xyz/agentty".to_string()) })
        });
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .once()
        .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty")
        .returning(|_| {
            Ok(forge::ForgeRemote {
                command_working_directory: None,
                forge_kind: ForgeKind::GitHub,
                host: "github.com".to_string(),
                namespace: "agentty-xyz".to_string(),
                project: "agentty".to_string(),
                repo_url: "https://github.com/agentty-xyz/agentty".to_string(),
                web_url: "https://github.com/agentty-xyz/agentty".to_string(),
            })
        });
    let services = test_services(
        &database,
        Arc::new(mock_git_client),
        Arc::new(mock_review_request_client),
    );

    // Act
    let remote = session_manager
        .review_request_remote(&services, &session_manager.state.sessions[0], None)
        .await
        .expect("remote should resolve");

    // Assert
    assert_eq!(remote.command_working_directory, Some(session_folder));
    assert_eq!(remote.forge_kind, ForgeKind::GitHub);
}

#[test]
fn review_request_repo_url_derives_gitlab_project_url() {
    // Arrange
    let review_request = ReviewRequest {
        last_refreshed_at: 10,
        summary: ReviewRequestSummary {
            display_id: "!42".to_string(),
            forge_kind: ForgeKind::GitLab,
            source_branch: "wt/session-".to_string(),
            state: ReviewRequestState::Open,
            status_summary: Some("Draft".to_string()),
            target_branch: "main".to_string(),
            title: "Add forge review support".to_string(),
            web_url: "https://gitlab.com/agentty-xyz/agentty/-/merge_requests/42".to_string(),
        },
    };

    // Act
    let repo_url = SessionManager::review_request_repo_url(&review_request);

    // Assert
    assert_eq!(
        repo_url.as_deref(),
        Some("https://gitlab.com/agentty-xyz/agentty")
    );
}

#[tokio::test]
async fn test_refresh_review_request_updates_done_session_from_stored_link_when_worktree_is_missing()
 {
    // Arrange
    let MissingWorktreeReviewRefreshFixture {
        database,
        services,
        mut session_manager,
    } = missing_worktree_review_refresh_fixture().await;

    // Act
    let review_request = session_manager
        .refresh_review_request(&services, "session-id")
        .await
        .expect("linked review request should refresh");
    let persisted_row = database
        .sessions()
        .load_sessions()
        .await
        .expect("failed to load session rows")
        .into_iter()
        .find(|row| row.id == "session-id")
        .expect("session row should exist");

    // Assert
    assert_eq!(review_request.last_refreshed_at, 77);
    assert_eq!(review_request.summary.state, ReviewRequestState::Merged);
    assert_eq!(
        session_manager.state.sessions[0]
            .review_request
            .as_ref()
            .map(|review_request| review_request.summary.state),
        Some(ReviewRequestState::Merged)
    );
    assert_eq!(
        persisted_row
            .review_request
            .as_ref()
            .map(|row| row.state.as_str()),
        Some("Merged")
    );
}

/// Test fixture for refreshing a stored review request after its worktree
/// has been deleted.
struct MissingWorktreeReviewRefreshFixture {
    /// Repository bundle containing the linked review request session.
    database: AppRepositories,
    /// App services wired with git and forge mocks.
    services: AppServices,
    /// Session manager seeded with the linked session.
    session_manager: SessionManager,
}

/// Builds the missing-worktree review refresh fixture.
async fn missing_worktree_review_refresh_fixture() -> MissingWorktreeReviewRefreshFixture {
    let temp_dir = tempdir().expect("failed to create temp dir");
    let missing_folder = temp_dir.path().join("missing-session-folder");
    let linked_review_request = ReviewRequest {
        last_refreshed_at: 12,
        summary: review_request_summary("#42", ReviewRequestState::Open),
    };
    let session = test_session(
        missing_folder.clone(),
        Some(linked_review_request),
        Status::Done,
    );
    let database = database_with_session(&session).await;
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(
        now,
        SystemTime::UNIX_EPOCH + Duration::from_secs(77),
    ));
    let clock: Arc<dyn Clock> = fake_clock;
    let session_manager = session_manager_with_session(clock, session);
    let remote = forge::ForgeRemote {
        command_working_directory: None,
        forge_kind: ForgeKind::GitHub,
        host: "github.com".to_string(),
        namespace: "agentty-xyz".to_string(),
        project: "agentty".to_string(),
        repo_url: "https://github.com/agentty-xyz/agentty".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty".to_string(),
    };
    let refreshed_summary = ReviewRequestSummary {
        display_id: "#42".to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session-".to_string(),
        state: ReviewRequestState::Merged,
        status_summary: Some("Approved and merged".to_string()),
        target_branch: "main".to_string(),
        title: "Add forge review support".to_string(),
        web_url: "https://github.com/agentty-xyz/agentty/pull/42".to_string(),
    };
    let mut mock_git_client = git::MockGitClient::new();
    mock_git_client
        .expect_repo_url()
        .times(1)
        .withf({
            let missing_folder = missing_folder.clone();
            move |candidate_folder| candidate_folder == &missing_folder
        })
        .returning(|_| {
            Box::pin(async {
                Err(ag_git::GitError::Io(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "missing worktree",
                )))
            })
        });
    let mut mock_review_request_client = forge::MockReviewRequestClient::new();
    mock_review_request_client
        .expect_detect_remote()
        .times(1)
        .withf(|repo_url| repo_url == "https://github.com/agentty-xyz/agentty")
        .returning({
            let remote = remote.clone();
            move |_| Ok(remote.clone())
        });
    mock_review_request_client
        .expect_refresh_review_request()
        .times(1)
        .withf(move |candidate_remote, display_id| {
            candidate_remote == &remote && display_id == "#42"
        })
        .returning(move |_, _| {
            let refreshed_summary = refreshed_summary.clone();

            Box::pin(async move { Ok(refreshed_summary) })
        });
    let services = test_services(
        &database,
        Arc::new(mock_git_client),
        Arc::new(mock_review_request_client),
    );

    MissingWorktreeReviewRefreshFixture {
        database,
        services,
        session_manager,
    }
}

#[test]
fn test_is_session_refresh_due_returns_false_before_deadline() {
    // Arrange
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let clock: Arc<dyn Clock> = fake_clock;
    let session_manager = session_manager_fixture(clock);

    // Act
    let refresh_due = session_manager.is_session_refresh_due();
    let wall_clock = session_manager.state().clock.now_system_time();

    // Assert
    assert!(!refresh_due);
    assert_eq!(wall_clock, SystemTime::UNIX_EPOCH);
}

#[test]
fn preserve_live_orchestration_progress_keeps_active_board_and_respects_terminal_clear() {
    // Arrange
    let mut session = test_session(PathBuf::from("/tmp/session"), None, Status::Review);
    session.orchestration_progress = Some("2 running, 0 waiting on you".to_string());
    let live_progress = HashMap::from([(
        session.id.clone(),
        "Phase: Running\n- protocol [protocol]: running".to_string(),
    )]);

    // Act
    SessionManager::preserve_live_orchestration_progress(
        std::slice::from_mut(&mut session),
        &live_progress,
    );
    let active_progress = session.orchestration_progress.clone();
    session.orchestration_progress = None;
    SessionManager::preserve_live_orchestration_progress(
        std::slice::from_mut(&mut session),
        &live_progress,
    );

    // Assert
    assert_eq!(
        active_progress.as_deref(),
        Some("Phase: Running\n- protocol [protocol]: running")
    );
    assert!(session.orchestration_progress.is_none());
}

#[test]
fn test_is_session_refresh_due_returns_true_at_deadline() {
    // Arrange
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let clock: Arc<dyn Clock> = fake_clock.clone();
    let session_manager = session_manager_fixture(clock);
    fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

    // Act
    let refresh_due = session_manager.is_session_refresh_due();

    // Assert
    assert!(refresh_due);
}

#[test]
fn mode_session_id_uses_view_info_popup_restore_view() {
    // Arrange
    let mode = AppMode::ViewInfoPopup {
        is_loading: false,
        loading_label: "Refreshing review request...".to_string(),
        message: "Review request refreshed.".to_string(),
        restore_view: ConfirmationViewMode {
            scroll_offset: Some(2),
            session_id: "popup-session".into(),
        },
        title: "Review request refreshed".to_string(),
    };

    // Act
    let session_id = SessionManager::mode_session_id(&mode);

    // Assert
    assert_eq!(session_id.map(SessionId::as_str), Some("popup-session"));
}

#[test]
fn mode_session_id_uses_view_help_context() {
    // Arrange
    let mode = AppMode::Help {
        context: HelpContext::View {
            can_fork_session: false,
            can_merge_session_branch: false,
            can_mutate_session_branch: false,
            can_open_worktree: false,
            can_rebase_session_branch: false,
            can_show_diff: true,
            can_reply_to_session: false,
            can_start_staged_session: false,
            can_view_review_comments: false,
            publish_pull_request_action: None,
            scroll_offset: Some(2),
            session_id: "help-session".into(),
            session_state: ViewSessionState::Review,
        },
        scroll_offset: 0,
    };

    // Act
    let session_id = SessionManager::mode_session_id(&mode);

    // Assert
    assert_eq!(session_id.map(SessionId::as_str), Some("help-session"));
}

#[test]
fn mode_session_id_uses_diff_and_session_overlay_contexts() {
    // Arrange
    let modes = [
        AppMode::DiffLoading {
            fallback_view_scroll_offset: None,
            request_id: 1,
            restore: None,
            session_id: "loading-session".into(),
            sidebar_focus: DiffSidebarFocus::Files,
        },
        AppMode::Diff {
            diff: String::new(),
            file_explorer_selected_index: 0,
            focus: DiffFocus::Files,
            line_comments: DiffLineComments::default(),
            selected_diff_line_index: 0,
            preview: DiffPreview::default(),
            review_comments: None,
            restore: None,
            scroll_cache: None,
            scroll_offset: 0,
            session_id: "diff-session".into(),
        },
        AppMode::LaunchConfigurationSelector {
            commands: vec!["cargo test".to_string()],
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "launch-session".into(),
            },
            selected_command_index: 0,
        },
        AppMode::PublishBranchInput {
            default_branch_name: "wt/session".to_string(),
            input: InputState::default(),
            locked_upstream_ref: None,
            publish_branch_action: PublishBranchAction::Push,
            restore_view: ConfirmationViewMode {
                scroll_offset: None,
                session_id: "publish-session".into(),
            },
        },
    ];

    // Act
    let session_ids = modes
        .iter()
        .map(SessionManager::mode_session_id)
        .map(|session_id| session_id.map(SessionId::as_str))
        .collect::<Vec<_>>();

    // Assert
    assert_eq!(
        session_ids,
        [
            Some("loading-session"),
            Some("diff-session"),
            Some("launch-session"),
            Some("publish-session"),
        ]
    );
}

#[test]
fn test_ensure_mode_session_exists_closes_missing_diff_comments() {
    // Arrange
    let now = Instant::now();
    let clock: Arc<dyn Clock> = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let session_manager = session_manager_fixture(clock);
    let mut mode = AppMode::Diff {
        diff: String::new(),
        file_explorer_selected_index: 0,
        focus: DiffFocus::Files,
        line_comments: DiffLineComments::default(),
        selected_diff_line_index: 0,
        preview: DiffPreview::default(),
        review_comments: Some(crate::presentation::app_mode::DiffReviewComments::loading(
            1,
        )),
        restore: None,
        scroll_cache: None,
        session_id: "missing-session".into(),
        scroll_offset: 0,
    };

    // Act
    session_manager.ensure_mode_session_exists(&mut mode);

    // Assert
    assert!(matches!(mode, AppMode::List));
}

/// Builds a session manager with deterministic time and empty state.
fn session_manager_fixture(clock: Arc<dyn Clock>) -> SessionManager {
    let git_client: Arc<dyn git::GitClient> = Arc::new(git::MockGitClient::new());

    SessionManager::new(
        SessionDefaults {
            model: AgentKind::Antigravity.default_model(),
        },
        git_client,
        SessionState::new(
            HashMap::new(),
            Vec::new(),
            SelectionState::default(),
            clock,
            0,
            0,
        ),
        Vec::new(),
    )
}

/// Builds an empty project manager rooted at a temporary working directory.
fn empty_project_manager(working_dir: PathBuf) -> ProjectManager {
    ProjectManager::new(
        1,
        "project".to_string(),
        Some("main".to_string()),
        None,
        Vec::new(),
        working_dir,
    )
}

#[tokio::test]
async fn refresh_sessions_if_needed_skips_db_call_before_deadline_and_preserves_deadline() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let clock: Arc<dyn Clock> = fake_clock;
    let mut session_manager = session_manager_fixture(clock);
    let original_deadline = session_manager.state.refresh_deadline;
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let temp_dir = tempdir().expect("temp dir should be created");
    let projects = empty_project_manager(temp_dir.path().to_path_buf());
    let mut mode = AppMode::List;

    // Act
    session_manager
        .refresh_sessions_if_needed(&mut mode, &projects, &services)
        .await;

    // Assert
    assert_eq!(session_manager.state.refresh_deadline, original_deadline);
    assert_eq!(session_manager.state.row_count, 0);
    assert_eq!(session_manager.state.updated_at_max, 0);
}

#[tokio::test]
async fn refresh_sessions_if_needed_advances_deadline_and_skips_reload_when_metadata_unchanged() {
    // Arrange
    let database = AppRepositories::in_memory().await.expect("db should open");
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let clock: Arc<dyn Clock> = fake_clock.clone();
    let mut session_manager = session_manager_fixture(clock);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let temp_dir = tempdir().expect("temp dir should be created");
    let projects = empty_project_manager(temp_dir.path().to_path_buf());
    let mut mode = AppMode::List;
    let original_deadline = session_manager.state.refresh_deadline;
    fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

    // Act
    session_manager
        .refresh_sessions_if_needed(&mut mode, &projects, &services)
        .await;

    // Assert
    assert!(session_manager.state.refresh_deadline > original_deadline);
    assert_eq!(session_manager.state.row_count, 0);
    assert_eq!(session_manager.state.updated_at_max, 0);
}

#[tokio::test]
async fn refresh_sessions_if_needed_reloads_sessions_when_metadata_row_count_changed() {
    // Arrange
    let session = test_session(PathBuf::from("/tmp/session"), None, Status::Done);
    let database = database_with_session(&session).await;
    let now = Instant::now();
    let fake_clock = Arc::new(FakeClock::new(now, SystemTime::UNIX_EPOCH));
    let clock: Arc<dyn Clock> = fake_clock.clone();
    let mut session_manager = session_manager_fixture(clock);
    let services = test_services(
        &database,
        Arc::new(git::MockGitClient::new()),
        Arc::new(forge::MockReviewRequestClient::new()),
    );
    let temp_dir = tempdir().expect("temp dir should be created");
    let projects = empty_project_manager(temp_dir.path().to_path_buf());
    let mut mode = AppMode::List;
    fake_clock.set_now_instant(now + SESSION_REFRESH_INTERVAL);

    // Act
    session_manager
        .refresh_sessions_if_needed(&mut mode, &projects, &services)
        .await;

    // Assert
    assert_eq!(session_manager.state.row_count, 1);
    assert_eq!(session_manager.state.sessions.len(), 1);
    assert_eq!(session_manager.state.sessions[0].id, "session-id");
}

/// Test clock implementation with mutable `Instant` and `SystemTime`.
struct FakeClock {
    instant: Mutex<Instant>,
    system_time: Mutex<SystemTime>,
}

impl FakeClock {
    /// Creates a fake clock seeded with deterministic wall-clock values.
    fn new(instant: Instant, system_time: SystemTime) -> Self {
        Self {
            instant: Mutex::new(instant),
            system_time: Mutex::new(system_time),
        }
    }

    /// Overrides the fake monotonic instant used by refresh checks.
    fn set_now_instant(&self, instant: Instant) {
        let mut current_instant = self
            .instant
            .lock()
            .expect("fake clock instant lock should not be poisoned");
        *current_instant = instant;
    }
}

impl Clock for FakeClock {
    fn now_instant(&self) -> Instant {
        *self
            .instant
            .lock()
            .expect("fake clock instant lock should not be poisoned")
    }

    fn now_system_time(&self) -> SystemTime {
        *self
            .system_time
            .lock()
            .expect("fake clock system-time lock should not be poisoned")
    }
}
