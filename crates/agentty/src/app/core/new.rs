//! App construction and startup helpers for the core module.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_git::GitClient;
use tokio::sync::mpsc;

use super::event::AppEvent;
use super::state::{App, AppClients};
use crate::app::service::{AppServiceDeps, AppServices};
use crate::app::session::{SessionManager, migrate_active_sessions_off_retired_models};
use crate::app::setting::SettingsManager;
use crate::app::startup::{AppStartup, StartupProjectContext, StartupSessionLoadContext};
use crate::app::{AppError, review, sync, task};
use crate::domain::agent::{AgentCliInfo, AgentKind};
use crate::infra::clock::{self, Clock};
use crate::infra::db;
use crate::infra::db::AppRepositories;
use crate::infra::fs::FsClient;
use crate::presentation::setting::SettingsPresentationState;

/// Environment flag that pins the version rendered by feature-test runs.
const E2E_PIN_DISPLAY_VERSION_ENV_VAR: &str = "AGENTTY_E2E_PIN_DISPLAY_VERSION";
/// Stable version label rendered while feature-test capture is enabled.
const E2E_DISPLAY_VERSION: &str = "v<test>";

impl App {
    /// Builds the app state from persisted data and starts background
    /// housekeeping tasks.
    ///
    /// When `auto_update` is `true`, a background `npm i -g agentty@latest`
    /// runs automatically after detecting a newer version at startup or in an
    /// hourly follow-up check.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted,
    /// required startup state cannot be loaded from the database, or restart
    /// recovery cannot complete.
    pub async fn new(
        auto_update: bool,
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        repositories: impl Into<AppRepositories>,
    ) -> Result<Self, AppError> {
        let clients = AppClients::new();
        let current_version_display_text = current_version_display_text(
            std::env::var_os(E2E_PIN_DISPLAY_VERSION_ENV_VAR).as_deref(),
            env!("CARGO_PKG_VERSION"),
        );

        let app = Self::new_with_options(
            auto_update,
            base_path,
            working_dir,
            git_branch,
            current_version_display_text,
            repositories.into(),
            clients,
        )
        .await?;

        Ok(app)
    }

    /// Core constructor with all options explicit.
    ///
    /// # Errors
    /// Returns an error if startup project metadata cannot be persisted,
    /// required startup state cannot be loaded from the database, or restart
    /// recovery cannot complete.
    async fn new_with_options(
        auto_update: bool,
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        current_version_display_text: String,
        repositories: AppRepositories,
        clients: AppClients,
    ) -> Result<Self, AppError> {
        let StartupProjectContext {
            active_project_id,
            active_project_name,
            initial_tab,
            project_items,
            startup_git_branch,
            startup_git_upstream_ref,
            startup_working_dir,
        } = Self::load_startup_project_state(
            &base_path,
            working_dir.as_path(),
            git_branch,
            &repositories,
            &clients,
        )
        .await?;

        let clock: Arc<dyn Clock> = clock::from_environment();
        let (event_tx, event_rx) = mpsc::unbounded_channel();
        let services = Self::build_services(
            base_path.clone(),
            Arc::clone(&clock),
            event_tx.clone(),
            repositories.clone(),
            &clients,
        )
        .await?;
        Self::recover_startup_operations(
            repositories.clone(),
            base_path,
            services.git_client(),
            clock,
        )
        .await?;
        let projects = crate::app::project::ProjectManager::new(
            active_project_id,
            active_project_name,
            startup_git_branch,
            startup_git_upstream_ref,
            project_items,
            startup_working_dir.clone(),
        );
        let settings = Self::load_settings(&repositories, &services, active_project_id).await;
        let mut sessions = Self::load_and_restack_startup_sessions(
            &services,
            active_project_id,
            startup_working_dir.as_path(),
        )
        .await;
        let (review_cache, recoverable_focused_review_session_ids) =
            Self::load_startup_focused_reviews(&repositories, active_project_id, &mut sessions)
                .await?;

        let sync_context = Self::sync_context_for(&projects, &services, &sessions);
        let sync_handle = sync::SyncHandle::spawn(event_tx.clone(), sync_context);
        Self::spawn_startup_background_tasks(auto_update, &event_tx, &services, &clients);
        let sync_main_runner = clients
            .sync_main_runner
            .unwrap_or_else(|| sync_handle.sync_main_runner());
        let mut app = Self {
            mode: crate::presentation::app_mode::AppMode::List,
            needs_redraw: true,
            settings,
            settings_presentation: SettingsPresentationState::default(),
            tabs: crate::app::tab::TabManager::new(initial_tab),
            current_version_display_text,
            prompt_progress: std::collections::HashMap::new(),
            diff_comment_progress: std::collections::HashMap::new(),
            auto_address_review_iterations: std::collections::HashMap::new(),
            deferred_auto_review_session_ids: std::collections::HashSet::new(),
            latest_project_sync_operation_ids: std::collections::HashMap::new(),
            pending_project_sync_completions: std::collections::HashMap::new(),
            pending_project_sync_requests: std::collections::VecDeque::new(),
            pending_focused_review_persistence: std::collections::HashMap::new(),
            pending_session_creations: std::collections::HashMap::new(),
            pending_session_diff_requests: std::collections::HashMap::new(),
            projects,
            services,
            sessions: sessions.into(),
            question_progress: std::collections::HashMap::new(),
            question_reconcile_reload_attempted: None,
            event_rx,
            is_tmux_session: clients.is_tmux_session,
            review_cache,
            review_diff_hashes: std::collections::HashMap::new(),
            latest_available_version: None,
            last_seen_session_update_versions: std::collections::HashMap::new(),
            merge_queue: crate::app::merge_queue::MergeQueue::default(),
            next_sync_operation_id: 1,
            project_sync_status: None,
            project_sync_status_expires_at: None,
            session_progress_messages: std::collections::HashMap::new(),
            update_status: None,
            sync_handle,
            sync_main_runner,
            tmux_client: clients.tmux_client,
        };
        app.recover_startup_focused_reviews(recoverable_focused_review_session_ids);

        Ok(app)
    }

    /// Loads focused-review cache state and reviews that must be regenerated
    /// during startup recovery.
    async fn load_startup_focused_reviews(
        repositories: &AppRepositories,
        active_project_id: i64,
        sessions: &mut SessionManager,
    ) -> Result<
        (
            std::collections::HashMap<crate::domain::session::SessionId, review::ReviewCacheEntry>,
            Vec<String>,
        ),
        AppError,
    > {
        let review_cache = Self::load_focused_review_cache(repositories, active_project_id).await;
        let recoverable_session_ids =
            Self::load_recoverable_focused_review_session_ids(repositories, active_project_id)
                .await?;
        review::hydrate_review_transients(&review_cache, sessions.state_mut());

        Ok((review_cache, recoverable_session_ids))
    }

    /// Restarts focused review for durable triggers whose persistence was
    /// interrupted before a terminal review result was stored.
    pub(super) fn recover_startup_focused_reviews(&mut self, session_ids: Vec<String>) {
        let session_ids = session_ids
            .into_iter()
            .map(crate::domain::session::SessionId::from)
            .collect();

        self.auto_start_reviews(&session_ids);
    }

    /// Loads durable automatic-review triggers for eligible worker and
    /// managed sessions in one project.
    pub(super) async fn load_recoverable_focused_review_session_ids(
        repositories: &AppRepositories,
        project_id: i64,
    ) -> Result<Vec<String>, AppError> {
        let mut session_ids = repositories
            .sessions()
            .load_pending_focused_review_session_ids(project_id)
            .await?;
        session_ids.extend(
            repositories
                .orchestrations()
                .load_recoverable_focused_review_session_ids(project_id)
                .await?,
        );
        session_ids.sort_unstable();
        session_ids.dedup();

        Ok(session_ids)
    }

    /// Loads active-project settings from the feature-scoped dependencies.
    async fn load_settings(
        repositories: &AppRepositories,
        services: &AppServices,
        project_id: i64,
    ) -> SettingsManager {
        SettingsManager::from_repositories(
            repositories.clone(),
            services.available_agent_kinds(),
            project_id,
        )
        .await
    }

    /// Loads persisted focused reviews for one project into the render cache.
    pub(super) async fn load_focused_review_cache(
        repositories: &AppRepositories,
        active_project_id: i64,
    ) -> std::collections::HashMap<crate::domain::session::SessionId, review::ReviewCacheEntry>
    {
        let focused_review_rows = repositories
            .sessions()
            .load_session_focused_reviews_for_project(active_project_id)
            .await
            .unwrap_or_default();

        review::review_cache_from_rows(focused_review_rows)
    }

    /// Loads startup sessions and requeues durable post-merge stack restacks
    /// that were interrupted after persistence had recorded the stack base.
    async fn load_and_restack_startup_sessions(
        services: &AppServices,
        active_project_id: i64,
        startup_working_dir: &Path,
    ) -> SessionManager {
        let default_session_model = SessionManager::load_default_session_model(
            services,
            Some(active_project_id),
            AgentKind::Antigravity.default_model(),
        )
        .await;
        let mut sessions = AppStartup::load_startup_sessions(
            services,
            StartupSessionLoadContext {
                active_project_id,
                default_session_model,
                startup_working_dir,
            },
        )
        .await;
        let failures = sessions
            .rebase_pending_stack_restacks_for_project(services, active_project_id)
            .await;
        sessions
            .append_stacked_rebase_failure_notices(failures, "Pending stacked child sync failed");

        sessions
    }

    /// Returns the optional agent availability probe used by the background
    /// CLI version task.
    fn startup_agent_cli_version_probe(
        clients: &AppClients,
    ) -> Option<Arc<dyn ag_agent::AgentAvailabilityProbe>> {
        clients
            .agent_cli_version_task_enabled
            .then(|| Arc::clone(&clients.agent_availability_probe))
    }

    /// Spawns startup background tasks with the initial availability snapshot
    /// needed for CLI version fallback rows.
    fn spawn_startup_background_tasks(
        auto_update: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
        services: &AppServices,
        clients: &AppClients,
    ) {
        let agent_cli_version_probe = Self::startup_agent_cli_version_probe(clients);
        AppStartup::spawn_background_tasks(
            auto_update,
            event_tx,
            agent_cli_version_probe,
            services.available_agent_kinds(),
            Arc::clone(&clients.version_task_runner),
        );
    }

    /// Reclaims stale agent artifacts and migrates project-independent session
    /// state before loading the startup project context.
    ///
    /// # Errors
    /// Returns an error if artifact recovery fails or startup project metadata
    /// cannot be persisted or loaded from storage.
    async fn load_startup_project_state(
        base_path: &Path,
        working_dir: &Path,
        git_branch: Option<String>,
        repositories: &AppRepositories,
        clients: &AppClients,
    ) -> Result<StartupProjectContext, AppError> {
        clients
            .fs_client
            .cleanup_agent_artifacts(base_path.to_owned())
            .await
            .map_err(Self::startup_recovery_error)?;
        migrate_active_sessions_off_retired_models(repositories).await;
        let current_project_id =
            AppStartup::persist_startup_project(repositories, working_dir, git_branch.as_deref())
                .await?;
        let startup_project_context = AppStartup::load_startup_project_context(
            repositories,
            clients.fs_client.as_ref(),
            &clients.git_client,
            clients.project_discovery_client.as_ref(),
            working_dir,
            git_branch,
            current_project_id,
        )
        .await?;

        Ok(startup_project_context)
    }

    /// Builds the shared app services after validating startup agent
    /// availability.
    ///
    /// # Errors
    /// Returns an error when no supported agent backend is available.
    async fn build_services(
        base_path: PathBuf,
        clock: Arc<dyn Clock>,
        event_tx: mpsc::UnboundedSender<AppEvent>,
        repositories: AppRepositories,
        clients: &AppClients,
    ) -> Result<AppServices, AppError> {
        let available_agent_kinds = task::TaskService::load_agent_availability(Arc::clone(
            &clients.agent_availability_probe,
        ))
        .await;
        AppStartup::validate_startup_agent_availability(&available_agent_kinds)?;
        let available_agent_clis = AgentCliInfo::loading_from_kinds(&available_agent_kinds);

        Ok(AppServices::new_with_agent_clis(
            base_path,
            clock,
            event_tx,
            AppServiceDeps {
                app_server_client_override: clients
                    .app_server_client_override
                    .as_ref()
                    .map(Arc::clone),
                available_agent_kinds,
                clipboard_image_client_override: None,
                fs_client: Arc::clone(&clients.fs_client),
                git_client: Arc::clone(&clients.git_client),
                one_shot_client_override: None,
                personality_catalog_client_override: Some(Arc::clone(
                    &clients.personality_catalog_client,
                )),
                repositories,
                review_request_client: Arc::clone(&clients.review_request_client),
            },
            available_agent_clis,
        ))
    }

    /// Completes durable operation recovery before startup admits sessions.
    ///
    /// # Errors
    /// Returns an actionable error when recovery cannot complete.
    async fn recover_startup_operations(
        repositories: AppRepositories,
        base_path: PathBuf,
        git_client: Arc<dyn GitClient>,
        clock: Arc<dyn Clock>,
    ) -> Result<(), AppError> {
        repositories
            .sessions()
            .recover_session_preparations()
            .await
            .map_err(Self::startup_recovery_error)?;
        SessionManager::fail_unfinished_operations_from_previous_run(
            repositories,
            base_path,
            git_client,
            clock,
        )
        .await
        .map_err(Self::startup_recovery_error)
    }

    /// Converts a failed startup recovery into an actionable application error.
    fn startup_recovery_error(error: impl std::fmt::Display) -> AppError {
        AppError::Workflow(format!(
            "Startup recovery did not complete. Resolve the underlying storage or Git error and \
             restart Agentty: {error}"
        ))
    }

    /// Resolves the configured upstream reference for one project branch.
    pub(super) async fn load_git_upstream_ref(
        git_client: &dyn GitClient,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Option<String> {
        AppStartup::load_git_upstream_ref(git_client, working_dir, git_branch).await
    }

    /// Loads project list entries for the projects tab.
    ///
    /// Agentty-managed session worktrees, missing project directories, and
    /// non-git folders are excluded so the list keeps only user-facing
    /// repository roots that still exist on disk.
    pub(super) async fn load_project_items(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
    ) -> Vec<crate::domain::project::ProjectListItem> {
        AppStartup::load_project_items(db, fs_client).await
    }

    /// Converts a project row into domain project model.
    pub(super) fn project_from_row(project_row: db::ProjectRow) -> crate::domain::project::Project {
        AppStartup::project_from_row(project_row)
    }
}

/// Resolves the real or deterministic feature-test version label.
fn current_version_display_text(
    pin_feature_version: Option<&std::ffi::OsStr>,
    package_version: &str,
) -> String {
    if pin_feature_version == Some(std::ffi::OsStr::new("1")) {
        return E2E_DISPLAY_VERSION.to_string();
    }

    format!("v{package_version}")
}

#[cfg(test)]
#[path = "new_test.rs"]
mod tests;
