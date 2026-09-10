use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ag_agent::{AgentAvailabilityProbe, AppServerClient};
use ag_forge as forge;
use ag_forge::ReviewRequestClient;
use ag_git::GitClient;
use app::review::ReviewCacheEntry;
use app::service::AppServices;
use app::sync;
use session::{SyncMainOutcome, TurnAppliedState};
use tempfile::tempdir;

use super::super::{App, AppClients, SyncReviewRequestTaskResult};
use crate::app;
use crate::app::branch_publish::BranchPublishTaskSuccess;
use crate::app::core::event::{AppEvent, ReviewRequestStatusUpdate};
use crate::app::session;
use crate::app::test_support::AppServiceDeps;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
use crate::domain::input::InputState;
use crate::domain::question::QuestionItem;
use crate::domain::session::{
    ForgeKind, ReviewRequestState, ReviewRequestSummary, SESSION_DATA_DIR, SessionDiffState,
    SessionDiffStats, SessionFollowUpTask, SessionId, SessionSize, SessionStats, Status,
};
use crate::domain::transient_message::{
    TransientMessage, TransientMessageAnchor, TransientMessageBody, TransientMessageLifecycle,
    TransientMessageSlot,
};
use crate::infra::db;
use crate::infra::db::AppRepositories;
use crate::infra::personality::PersonalityCatalogClient;
use crate::infra::project_discovery::ProjectDiscoveryClient;
use crate::infra::tmux::{MockTmuxClient, TmuxClient};
use crate::presentation::app_mode::{AppMode, PromptModeSnapshot};
use crate::presentation::prompt::{PromptAttachmentState, PromptHistoryState, PromptSlashState};

/// Builds one reducer-ready turn projection for tests.
pub(super) fn test_turn_applied_state(
    questions: Vec<QuestionItem>,
    follow_up_tasks: Vec<&str>,
    token_usage_delta: SessionStats,
) -> TurnAppliedState {
    TurnAppliedState {
        follow_up_tasks: follow_up_tasks
            .into_iter()
            .enumerate()
            .map(|(position, text)| SessionFollowUpTask {
                id: i64::try_from(position).unwrap_or(i64::MAX),
                launched_session_id: None,
                position,
                text: text.to_string(),
            })
            .collect(),
        questions,
        token_usage_delta,
    }
}

pub(super) fn test_view_app_mode(session_id: &str) -> AppMode {
    AppMode::View {
        session_id: session_id.into(),
        scroll_offset: None,
    }
}

/// Builds one restorable prompt snapshot without attachments.
pub(in crate::app::core) fn test_prompt_mode_snapshot(session_id: SessionId) -> PromptModeSnapshot {
    PromptModeSnapshot {
        at_mention_state: None,
        attachment_state: PromptAttachmentState::default(),
        history_state: PromptHistoryState::new(Vec::new()),
        input: InputState::with_text("saved reply".to_string()),
        scroll_offset: None,
        session_id,
        slash_state: PromptSlashState::default(),
    }
}

pub(super) async fn test_app_viewing_reconcile_session(
    status: Status,
    questions: Vec<QuestionItem>,
    folder_name: &str,
) -> App {
    let mut app = crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(
        Arc::new(MockTmuxClient::new()),
    )
    .await;
    app.sessions.push_session(
        crate::test_support::SessionFixtureBuilder::new()
            .id("session-1")
            .folder(PathBuf::from(format!("/tmp/{folder_name}")))
            .status(status)
            .questions(questions)
            .build(),
    );
    app.mode = test_view_app_mode("session-1");

    app
}

/// Seeds one materialized session row for project-switching tests.
pub(super) async fn seed_materialized_session(
    database: &AppRepositories,
    base_path: &Path,
    project_id: i64,
    session_id: &str,
    status: Status,
) {
    database
        .sessions()
        .insert_session(
            session_id,
            AgentModel::Gpt56Sol.as_str(),
            "main",
            &status.to_string(),
            project_id,
        )
        .await
        .expect("failed to insert materialized session");
    fs::create_dir_all(session::session_folder(base_path, session_id).join(SESSION_DATA_DIR))
        .expect("failed to create materialized session data dir");
}

/// Seeds one review-ready session and its persisted focused review.
pub(super) async fn seed_persisted_review_session(
    database: &AppRepositories,
    base_path: &Path,
    project_id: i64,
    session_id: &str,
    diff_hash: &str,
    review_text: &str,
) {
    seed_materialized_session(database, base_path, project_id, session_id, Status::Review).await;
    database
        .sessions()
        .update_session_focused_review(
            session_id,
            Some(crate::domain::review::FocusedReviewStatus::Ready),
            Some(diff_hash.to_string()),
            Some(review_text.to_string()),
        )
        .await
        .expect("failed to persist focused review");
}

/// Inserts one completed focused review into an app cache for eviction tests.
pub(super) fn insert_test_ready_review(app: &mut App, session_id: &str) {
    app.review_cache.insert(
        session_id.into(),
        ReviewCacheEntry::Ready {
            diff_hash: 1,
            text: "inactive review".to_string(),
        },
    );
}

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

/// Builds a successful branch-publish batch payload for one session.
pub(super) fn test_pushed_branch_result(branch_name: &str) -> BranchPublishTaskSuccess {
    BranchPublishTaskSuccess::Pushed {
        branch_name: branch_name.to_string(),
        review_request_creation: None,
        upstream_reference: format!("origin/{branch_name}"),
    }
}

/// Builds a test app with one selected session, configurable launch
/// configuration, and injected tmux boundary.
pub(super) async fn new_test_app_with_selected_session(
    session_folder: PathBuf,
    launch_configuration: &str,
    tmux_client: Arc<dyn TmuxClient>,
) -> App {
    // Arrange
    let mut app =
        crate::test_support::new_test_app_with_tmux_client_without_retained_base_dir(tmux_client)
            .await;
    if !session_folder.as_os_str().is_empty() {
        std::fs::create_dir_all(&session_folder).expect("failed to create session folder");
    }

    // Act
    app.settings.launch_configuration = launch_configuration.to_string();
    app.sessions
        .push_session(crate::test_support::session_fixture_with_folder(
            session_folder,
        ));
    app.sessions.select_session_index(Some(0));

    // Assert
    app
}

/// Inserts the selected session into the test database for durable transcript
/// assertions.
pub(super) async fn persist_selected_session(app: &App) {
    let project_id = app
        .services
        .db()
        .projects()
        .upsert_project("/tmp/project", Some("main".to_string()))
        .await
        .expect("failed to insert project");
    app.services
        .db()
        .sessions()
        .insert_session("session-1", "gpt-5.6-sol", "main", "Review", project_id)
        .await
        .expect("failed to insert session");
}

/// Persists the selected session with a known-empty diff and aligns its
/// loaded snapshot with that durable state.
pub(super) async fn seed_selected_session_empty_diff_state(app: &mut App) {
    persist_selected_session(app).await;
    app.services
        .db()
        .sessions()
        .update_session_diff_stats(0, 0, false, "session-1", "XS")
        .await
        .expect("failed to seed empty diff state");
    app.sessions.sessions_mut()[0].stats.diff_state = SessionDiffState::Empty;
}

pub(super) fn seed_completed_review_transient_message(app: &mut App) {
    app.sessions.state_mut().sessions_mut()[0]
        .transient_messages
        .upsert(TransientMessage {
            anchor: TransientMessageAnchor::Tail,
            body: TransientMessageBody::Markdown(
                "## Review\n\nReview completed before publishing.".to_string(),
            ),
            lifecycle: TransientMessageLifecycle::ClearOnNewTurn,
            slot: TransientMessageSlot::Review,
            turn_position: None,
        });
}

pub(super) fn assert_review_message_reanchored_after_publish(app: &App) {
    let transient_messages = &app.sessions.state().sessions()[0].transient_messages;

    assert!(
        transient_messages
            .get(TransientMessageSlot::BranchPublish)
            .is_none()
    );
    assert_eq!(
        transient_messages
            .get(TransientMessageSlot::Review)
            .expect("completed review should remain visible")
            .anchor,
        TransientMessageAnchor::AfterCompletedTurn
    );
}

pub(super) fn known_session_diff_stats(
    added_lines: u64,
    deleted_lines: u64,
    session_size: SessionSize,
) -> SessionDiffStats {
    SessionDiffStats::Known {
        added_lines,
        deleted_lines,
        has_diff: true,
        session_size,
    }
}

/// Verifies successful synchronous creation still returns a ready fork with
/// the source conversation after an earlier preparation fault is removed.
pub(super) async fn assert_synchronous_fork_is_ready_with_history(app: &mut App, source_id: &str) {
    // Act
    let fork_id = app
        .sessions
        .fork_session(&app.services, source_id)
        .await
        .expect("fork");
    let preparation = app
        .services
        .db()
        .sessions()
        .load_session_preparation(&fork_id)
        .await
        .expect("load")
        .expect("fork preparation");
    let fork_messages = app
        .services
        .db()
        .sessions()
        .load_session_messages(&fork_id)
        .await
        .expect("copied history");

    // Assert
    assert_eq!(preparation.state, db::SessionPreparationState::Ready);
    assert_eq!(
        fork_messages
            .iter()
            .map(|message| message.content.as_str())
            .collect::<Vec<_>>(),
        vec!["Source question", "Source answer"]
    );
}

/// Snapshots checkout directories, session branches, and worktree registrations
/// so failed creation cannot leave resources that are invisible in the
/// database.
pub(super) async fn session_creation_resources(root: &Path) -> Vec<String> {
    let mut snapshot: Vec<String> = fs::read_dir(root)
        .expect("repository entries")
        .map(|entry| {
            entry
                .expect("entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    snapshot.sort();
    for arguments in [
        ["branch", "--list", "wt/*"],
        ["worktree", "list", "--porcelain"],
    ] {
        let output = tokio::process::Command::new("git")
            .args(arguments)
            .current_dir(root)
            .output()
            .await
            .expect("read git state");
        assert!(output.status.success());
        snapshot.push(String::from_utf8(output.stdout).expect("git output"));
    }

    snapshot
}

/// Creates one directory with a `.git` marker for repository discovery
/// tests.
pub(super) fn create_git_repo_marker(repository_path: &Path) {
    fs::create_dir_all(repository_path.join(".git"))
        .expect("failed to create repository .git marker");
}

/// Builds one lightweight project row fixture for project list tests.
pub(super) fn project_list_row_fixture(
    project_id: i64,
    project_path: String,
) -> db::ProjectListRow {
    db::ProjectListRow {
        active_session_count: 0,
        created_at: 0,
        display_name: None,
        git_branch: Some("main".to_string()),
        id: project_id,
        input_tokens: 0,
        is_favorite: false,
        last_opened_at: None,
        last_session_updated_at: None,
        output_tokens: 0,
        path: project_path,
        session_count: 0,
        updated_at: 0,
    }
}

/// Applies queued app events until `condition` observes the expected app
/// state, or fails the test after a short timeout.
pub(super) async fn wait_for_app_condition(app: &mut App, condition: impl Fn(&App) -> bool) {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if condition(app) {
                break;
            }

            let app_event = app
                .next_app_event()
                .await
                .expect("background task should emit an app event");
            app.apply_app_events(app_event).await;
        }
    })
    .await
    .expect("timed out waiting for app condition");
}

/// Replaces the app-level git dependencies with one caller-provided mock.
pub(in crate::app::core) fn install_mock_git_client(
    app: &mut App,
    mock_git_client: ag_git::MockGitClient,
) {
    let mock_git_client: Arc<dyn ag_git::GitClient> = Arc::new(mock_git_client);
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
        AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client: Arc::clone(&mock_git_client),
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Replaces the app-level review-request dependency with one
/// caller-provided mock.
pub(super) fn install_mock_review_request_client(
    app: &mut App,
    mock_review_request_client: forge::MockReviewRequestClient,
) {
    let review_request_client: Arc<dyn ReviewRequestClient> = Arc::new(mock_review_request_client);
    let base_path = app.services.base_path().to_path_buf();
    let db = app.services.db().clone();
    let event_sender = app.services.event_sender();
    let app_server_client_override = app.services.app_server_client_override();
    let available_agent_kinds = app.services.available_agent_kinds();
    let available_agent_clis =
        crate::domain::agent::AgentCliInfo::from_kinds(&available_agent_kinds);
    let fs_client = app.services.fs_client();
    let git_client = app.services.git_client();

    app.services = AppServices::new_with_agent_clis(
        base_path,
        app.services.clock(),
        event_sender,
        AppServiceDeps {
            app_server_client_override,
            available_agent_kinds,
            clipboard_image_client_override: None,
            fs_client,
            git_client,
            one_shot_client_override: None,
            personality_catalog_client_override: None,
            repositories: db,
            review_request_client,
        },
        available_agent_clis,
    );
}

/// Builds one GitHub remote fixture for review-comment state tests.
pub(super) fn forge_remote() -> forge::ForgeRemote {
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

/// Builds one review-comment snapshot fixture for app-state detail tests.
pub(super) fn review_comment_snapshot() -> forge::ReviewCommentSnapshot {
    forge::ReviewCommentSnapshot {
        pr_level_comments: vec![forge::ReviewComment {
            author: "alice".to_string(),
            authored_by_current_user: false,
            body: "Looks good.".to_string(),
        }],
        threads: Vec::new(),
    }
}

/// Applies queued app events until the pending session-diff request settles.
pub(super) async fn apply_next_session_diff(app: &mut App) {
    let expected_request_id = *app
        .pending_session_diff_requests
        .keys()
        .next()
        .expect("session diff request should be pending");
    tokio::time::timeout(Duration::from_secs(5), async {
        while app
            .pending_session_diff_requests
            .contains_key(&expected_request_id)
        {
            let event = app
                .next_app_event()
                .await
                .expect("app event channel should remain open");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("session diff request should settle");
}

/// Builds one test review request summary for background sync tests.
pub(in crate::app::core) fn test_review_request_summary(
    display_id: &str,
    state: ReviewRequestState,
) -> ReviewRequestSummary {
    ReviewRequestSummary {
        display_id: display_id.to_string(),
        forge_kind: ForgeKind::GitHub,
        source_branch: "wt/session-id".to_string(),
        state,
        status_summary: None,
        target_branch: "main".to_string(),
        title: "feat".to_string(),
        web_url: String::new(),
    }
}

pub(in crate::app::core) async fn insert_review_session_with_data_dir(app: &App, session_id: &str) {
    app.services
        .db()
        .sessions()
        .insert_session(
            session_id,
            "gemini-3.8-flash",
            "main",
            &Status::Review.to_string(),
            app.active_project_id(),
        )
        .await
        .expect("failed to insert session");
    let session_folder_name = session_id.chars().take(8).collect::<String>();
    fs::create_dir_all(
        app.services
            .base_path()
            .join(session_folder_name)
            .join(SESSION_DATA_DIR),
    )
    .expect("failed to create session data dir");
}

pub(in crate::app::core) async fn new_test_app_with_database_pool()
-> (App, sqlx::SqlitePool, tempfile::TempDir) {
    let base_dir = tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let (database, pool) = AppRepositories::in_memory_with_pool()
        .await
        .expect("db should open");
    let clients = crate::test_support::test_app_clients_with_mock_app_server()
        .with_tmux_client(Arc::new(MockTmuxClient::new()));
    let app = App::new_with_clients(base_path.clone(), base_path, None, database, clients)
        .await
        .expect("failed to build app");

    (app, pool, base_dir)
}

pub(in crate::app::core) fn merged_review_request_status_update(
    session_id: &str,
    display_id: &str,
    session_head_hash: &str,
    target_branch: &str,
) -> ReviewRequestStatusUpdate {
    let mut summary = test_review_request_summary(display_id, ReviewRequestState::Merged);
    summary.target_branch = target_branch.to_string();

    ReviewRequestStatusUpdate {
        generation: 0,
        result: Ok(SyncReviewRequestTaskResult {
            outcome: session::SyncReviewRequestOutcome::Merged {
                display_id: display_id.to_string(),
                session_head_hash: Some(session_head_hash.to_string()),
            },
            summary: Some(summary),
        }),
        session_id: session_id.into(),
    }
}

pub(super) fn successful_manual_sync(
    project_id: i64,
    default_branch: &str,
    pulled_commits: u32,
) -> AppEvent {
    AppEvent::SyncMainCompleted {
        completion: successful_manual_sync_completion(project_id, default_branch, pulled_commits),
    }
}

pub(super) fn successful_manual_sync_completion(
    project_id: i64,
    default_branch: &str,
    pulled_commits: u32,
) -> sync::SyncMainCompletion {
    sync::SyncMainCompletion {
        operation: sync::ProjectSyncContext {
            default_branch: default_branch.to_string(),
            operation_id: 1,
            project_id,
            project_name: "agentty".to_string(),
        },
        result: Ok(SyncMainOutcome {
            default_branch: default_branch.to_string(),
            deferred_merged_session_ids: Vec::new(),
            pulled_commit_titles: Vec::new(),
            pulled_commits: Some(pulled_commits),
            pushed_commit_titles: Vec::new(),
            pushed_commits: Some(0),
            resolved_conflict_files: Vec::new(),
        }),
        review_request_updates: Vec::new(),
    }
}

impl AppClients {
    /// Replaces the startup agent-availability boundary while preserving the
    /// remaining clients.
    #[must_use]
    pub(crate) fn with_agent_availability_probe(
        mut self,
        agent_availability_probe: Arc<dyn AgentAvailabilityProbe>,
    ) -> Self {
        self.agent_availability_probe = agent_availability_probe;

        self
    }

    /// Replaces the default provider-owned app-server clients with one shared
    /// override.
    #[must_use]
    pub(crate) fn with_app_server_client_override(
        mut self,
        app_server_client_override: Arc<dyn AppServerClient>,
    ) -> Self {
        self.app_server_client_override = Some(app_server_client_override);

        self
    }

    /// Replaces the git boundary for deterministic app tests.
    #[must_use]
    pub(crate) fn with_git_client(mut self, git_client: Arc<dyn GitClient>) -> Self {
        self.git_client = git_client;

        self
    }

    /// Replaces the personality catalog boundary for deterministic app tests.
    #[must_use]
    pub(crate) fn with_personality_catalog_client(
        mut self,
        personality_catalog_client: Arc<dyn PersonalityCatalogClient>,
    ) -> Self {
        self.personality_catalog_client = personality_catalog_client;

        self
    }

    /// Replaces the startup project-discovery boundary while preserving the
    /// remaining clients.
    #[must_use]
    pub(crate) fn with_project_discovery_client(
        mut self,
        project_discovery_client: Arc<dyn ProjectDiscoveryClient>,
    ) -> Self {
        self.project_discovery_client = project_discovery_client;

        self
    }

    /// Replaces the tmux boundary while preserving the remaining clients.
    #[must_use]
    pub(crate) fn with_tmux_client(mut self, tmux_client: Arc<dyn TmuxClient>) -> Self {
        self.is_tmux_session = true;
        self.tmux_client = tmux_client;

        self
    }

    /// Overrides whether the test app is treated as running inside `tmux`.
    #[must_use]
    pub(crate) fn with_tmux_session(mut self, is_tmux_session: bool) -> Self {
        self.is_tmux_session = is_tmux_session;

        self
    }
}

impl AppClients {
    pub(crate) fn with_background_tasks_disabled(mut self) -> Self {
        self.agent_cli_version_task_enabled = false;
        self.version_task_runner = crate::app::task::tests::mock_version_task_runner();

        self
    }
}
