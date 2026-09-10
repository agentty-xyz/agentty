//! App startup and project-catalog helper workflows.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ag_agent::AgentAvailabilityProbe;
use ag_git::GitClient;
use tokio::sync::mpsc;

use super::core::{AGENTTY_WT_DIR, AppEvent};
use super::task;
use crate::app::service::AppServices;
use crate::app::session::{SessionLoadInput, SessionManager};
use crate::app::session_state::SessionState;
use crate::app::tab::Tab;
use crate::app::{AppError, session};
use crate::domain::agent::{AgentKind, AgentModel};
use crate::domain::project::{Project, ProjectListItem, project_name_from_path};
use crate::domain::selection::SelectionState;
use crate::domain::session_order;
use crate::domain::setting::SettingName;
use crate::infra::db::AppRepositories;
use crate::infra::fs::FsClient;
use crate::infra::project_discovery::ProjectDiscoveryClient;

/// Startup project context resolved before the first render.
pub(crate) struct StartupProjectContext {
    /// Persisted active project id used to initialize managers and settings.
    pub(crate) active_project_id: i64,
    /// Display label for the active project shown on first render.
    pub(crate) active_project_name: String,
    /// Initial top-level tab selected for the first render.
    pub(crate) initial_tab: Tab,
    /// Initial project list shown in the projects tab.
    pub(crate) project_items: Vec<ProjectListItem>,
    /// Startup git branch for the active project when detected.
    pub(crate) startup_git_branch: Option<String>,
    /// Startup upstream reference for the active project when detected.
    pub(crate) startup_git_upstream_ref: Option<String>,
    /// Working directory used for the active project session list.
    pub(crate) startup_working_dir: PathBuf,
}

/// Startup-only inputs needed to hydrate the initial `SessionManager`.
pub(crate) struct StartupSessionLoadContext<'a> {
    /// Identifier for the project whose sessions should be loaded.
    pub(crate) active_project_id: i64,
    /// Default model applied when persisted session rows omit one.
    pub(crate) default_session_model: AgentModel,
    /// Working directory used to resolve session metadata at startup.
    pub(crate) startup_working_dir: &'a Path,
}

/// Shared startup coordinator for app construction and project catalog work.
pub(crate) struct AppStartup;

impl AppStartup {
    /// Returns a startup error when no supported backend CLI is installed.
    pub(crate) fn validate_startup_agent_availability(
        available_agent_kinds: &[AgentKind],
    ) -> Result<(), AppError> {
        if available_agent_kinds.is_empty() {
            return Err(AppError::Workflow(
                "No supported backend CLI found on `PATH`. Install `codex`, `claude`, `gemini`, \
                 or Antigravity CLI 1.1.7 or newer. For an older `agy`, run `agy update`, then \
                 restart `agentty`."
                    .to_string(),
            ));
        }

        Ok(())
    }

    /// Persists the startup project row and backfills legacy session rows.
    pub(crate) async fn persist_startup_project(
        db: &AppRepositories,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Result<i64, AppError> {
        let current_project_id = db
            .projects()
            .upsert_project(
                &working_dir.to_string_lossy(),
                git_branch.map(str::to_string),
            )
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to persist startup project `{}`: {error}",
                    working_dir.display()
                ))
            })?;

        db.sessions()
            .backfill_session_project(current_project_id)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to backfill startup sessions for project `{}`: {error}",
                    working_dir.display()
                ))
            })?;

        Ok(current_project_id)
    }

    /// Resolves startup project state and persists active-project metadata.
    pub(crate) async fn load_startup_project_context(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        git_client: &Arc<dyn GitClient>,
        project_discovery_client: &dyn ProjectDiscoveryClient,
        working_dir: &Path,
        git_branch: Option<String>,
        current_project_id: i64,
    ) -> Result<StartupProjectContext, AppError> {
        let had_active_project_setting = db
            .settings()
            .load_active_project_id()
            .await
            .ok()
            .flatten()
            .is_some();
        let startup_active_project_id =
            Self::resolve_startup_active_project_id(db, fs_client, current_project_id).await;
        let startup_active_project = Self::load_project(
            db,
            startup_active_project_id,
            working_dir,
            git_branch.as_deref(),
        )
        .await;
        let startup_working_dir = startup_active_project.path.clone();
        let startup_git_branch = if startup_working_dir.as_path() == working_dir {
            git_branch
        } else {
            git_client
                .detect_git_info(startup_working_dir.clone())
                .await
        };
        let startup_git_upstream_ref = Self::load_git_upstream_ref(
            git_client.as_ref(),
            startup_working_dir.as_path(),
            startup_git_branch.as_deref(),
        )
        .await;
        let active_project_id = db
            .projects()
            .upsert_project(
                &startup_working_dir.to_string_lossy(),
                startup_git_branch.clone(),
            )
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to persist active startup project `{}`: {error}",
                    startup_working_dir.display()
                ))
            })?;
        db.settings()
            .set_active_project_id(active_project_id)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to store active startup project `{}`: {error}",
                    startup_working_dir.display()
                ))
            })?;
        db.projects()
            .touch_project_last_opened(active_project_id)
            .await
            .map_err(|error| {
                AppError::Workflow(format!(
                    "Failed to update startup project activity for `{}`: {error}",
                    startup_working_dir.display()
                ))
            })?;
        Self::refresh_project_catalog_on_startup(db, git_client.as_ref(), project_discovery_client)
            .await;

        let project_items = Self::load_project_items(db, fs_client).await;
        let active_project_name =
            Self::project_title_for_id(&project_items, active_project_id, &startup_working_dir);
        let initial_tab = Self::load_startup_tab(db, had_active_project_setting).await;

        Ok(StartupProjectContext {
            active_project_id,
            active_project_name,
            initial_tab,
            project_items,
            startup_git_branch,
            startup_git_upstream_ref,
            startup_working_dir,
        })
    }

    /// Loads startup session rows, metadata, and runtime handles.
    pub(crate) async fn load_startup_sessions(
        services: &AppServices,
        context: StartupSessionLoadContext<'_>,
    ) -> SessionManager {
        let StartupSessionLoadContext {
            active_project_id,
            default_session_model,
            startup_working_dir,
        } = context;
        let mut table_state = SelectionState::default();
        let mut handles = std::collections::HashMap::new();
        let clock = services.clock();
        let fs_client = services.fs_client();
        let (sessions, stats_activity, session_worktree_availability) =
            SessionManager::load_sessions_with_fs_client(
                SessionLoadInput {
                    active_project_id,
                    active_session_id: None,
                    base: services.base_path(),
                    clock: clock.as_ref(),
                    db: services.db(),
                    fs_client: fs_client.as_ref(),
                    working_dir: startup_working_dir,
                },
                &mut handles,
            )
            .await;
        let (sessions_row_count, sessions_updated_at_max) = services
            .db()
            .sessions()
            .load_sessions_metadata()
            .await
            .unwrap_or((0, 0));
        table_state.select(session_order::preferred_initial_session_index(&sessions));

        let mut session_manager = SessionManager::new(
            session::SessionDefaults {
                model: default_session_model,
            },
            services.git_client(),
            SessionState::new(
                handles,
                sessions,
                table_state,
                clock,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            stats_activity,
        );
        if let Some(selected_session_id) = session_manager
            .selected_session()
            .map(|session| session.id.clone())
        {
            session_manager
                .load_session_detail_into_state(services.db(), selected_session_id.as_str())
                .await;
        }
        session_manager.replace_session_worktree_availability(session_worktree_availability);
        session_manager.refresh_session_branch_names().await;

        session_manager
    }

    /// Loads the initial list tab from settings, with a project-aware fallback.
    pub(crate) async fn load_startup_tab(
        db: &AppRepositories,
        had_active_project_setting: bool,
    ) -> Tab {
        let persisted_tab = db
            .settings()
            .get_setting(SettingName::ActiveTab)
            .await
            .ok()
            .flatten();
        if let Some(tab) = persisted_tab.as_deref().and_then(Tab::from_str) {
            return tab;
        }

        if had_active_project_setting {
            Tab::Sessions
        } else {
            Tab::Projects
        }
    }

    /// Spawns app-wide background tasks that are not owned by the sync
    /// orchestrator.
    pub(crate) fn spawn_background_tasks(
        auto_update: bool,
        event_tx: &mpsc::UnboundedSender<AppEvent>,
        agent_availability_probe: Option<Arc<dyn AgentAvailabilityProbe>>,
        fallback_agent_kinds: Vec<AgentKind>,
        version_task_runner: Arc<dyn task::VersionTaskRunner>,
    ) {
        if let Some(agent_availability_probe) = agent_availability_probe {
            task::TaskService::spawn_agent_cli_version_task(
                event_tx,
                agent_availability_probe,
                fallback_agent_kinds,
            );
        }
        task::TaskService::spawn_version_check_task(event_tx, auto_update, version_task_runner);
    }

    /// Loads project list entries for the projects tab.
    pub(crate) async fn load_project_items(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
    ) -> Vec<ProjectListItem> {
        let session_worktree_root = super::core::agentty_home().join(AGENTTY_WT_DIR);

        Self::load_project_items_with_session_worktree_root(
            db,
            fs_client,
            session_worktree_root.as_path(),
        )
        .await
    }

    /// Loads project list entries with one caller-provided worktree root.
    pub(crate) async fn load_project_items_with_session_worktree_root(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<ProjectListItem> {
        Self::visible_project_rows(
            db.projects()
                .load_projects_with_stats()
                .await
                .unwrap_or_default(),
            fs_client,
            session_worktree_root,
        )
        .into_iter()
        .map(Self::project_list_item_from_row)
        .collect()
    }

    /// Refreshes the persisted project catalog from the user home directory
    /// through the injected project-discovery boundary.
    pub(crate) async fn refresh_project_catalog_on_startup(
        db: &AppRepositories,
        git_client: &dyn GitClient,
        project_discovery_client: &dyn ProjectDiscoveryClient,
    ) {
        let session_worktree_root = super::core::agentty_home().join(AGENTTY_WT_DIR);
        let home_directory = env::home_dir();

        Self::load_projects_from_home_directory(
            db,
            git_client,
            project_discovery_client,
            session_worktree_root.as_path(),
            home_directory.as_deref(),
        )
        .await;
    }

    /// Discovers git repositories under the user home directory through the
    /// injected project-discovery boundary and persists them.
    pub(crate) async fn load_projects_from_home_directory(
        db: &AppRepositories,
        git_client: &dyn GitClient,
        project_discovery_client: &dyn ProjectDiscoveryClient,
        session_worktree_root: &Path,
        home_directory: Option<&Path>,
    ) {
        let Some(home_directory) = home_directory.map(Path::to_path_buf) else {
            return;
        };

        let Ok(discovered_project_paths) = project_discovery_client
            .discover_home_project_paths(home_directory, session_worktree_root.to_path_buf())
            .await
        else {
            return;
        };

        for project_path in discovered_project_paths {
            let git_branch = git_client.detect_git_info(project_path.clone()).await;
            let project_path = project_path.to_string_lossy().to_string();
            // Best-effort: project metadata persistence is non-critical.
            let _ = db
                .projects()
                .upsert_project(project_path.as_str(), git_branch)
                .await;
        }
    }

    /// Returns whether a persisted project path points to an agentty worktree.
    pub(crate) fn is_session_worktree_project_path(
        project_path: &str,
        session_worktree_root: &Path,
    ) -> bool {
        Path::new(project_path).starts_with(session_worktree_root)
    }

    /// Filters persisted project rows down to visible project list entries.
    pub(crate) fn visible_project_rows(
        project_rows: Vec<crate::infra::db::ProjectListRow>,
        fs_client: &dyn FsClient,
        session_worktree_root: &Path,
    ) -> Vec<crate::infra::db::ProjectListRow> {
        project_rows
            .into_iter()
            .filter(|project_row| {
                !Self::is_session_worktree_project_path(
                    project_row.path.as_str(),
                    session_worktree_root,
                ) && Self::is_existing_project_path(fs_client, project_row.path.as_str())
                    && Self::is_git_repository_project_path(fs_client, project_row.path.as_str())
            })
            .collect()
    }

    /// Returns whether one persisted project path still resolves to a
    /// directory.
    pub(crate) fn is_existing_project_path(fs_client: &dyn FsClient, project_path: &str) -> bool {
        fs_client.is_dir(PathBuf::from(project_path))
    }

    /// Returns whether one persisted project path has a direct git metadata
    /// marker, either as a `.git` directory or linked-worktree `.git` file.
    pub(crate) fn is_git_repository_project_path(
        fs_client: &dyn FsClient,
        project_path: &str,
    ) -> bool {
        fs_client.exists(Path::new(project_path).join(".git"))
    }

    /// Converts a project row into the domain project model.
    pub(crate) fn project_from_row(project_row: crate::infra::db::ProjectRow) -> Project {
        Project {
            created_at: project_row.created_at,
            display_name: project_row.display_name,
            git_branch: project_row.git_branch,
            id: project_row.id,
            is_favorite: project_row.is_favorite,
            last_opened_at: project_row.last_opened_at,
            path: PathBuf::from(project_row.path),
            updated_at: project_row.updated_at,
        }
    }

    /// Converts an aggregated project row into list-friendly project metadata.
    pub(crate) fn project_list_item_from_row(
        project_row: crate::infra::db::ProjectListRow,
    ) -> ProjectListItem {
        let project = Project {
            created_at: project_row.created_at,
            display_name: project_row.display_name,
            git_branch: project_row.git_branch,
            id: project_row.id,
            is_favorite: project_row.is_favorite,
            last_opened_at: project_row.last_opened_at,
            path: PathBuf::from(project_row.path),
            updated_at: project_row.updated_at,
        };

        ProjectListItem {
            active_session_count: u32::try_from(project_row.active_session_count)
                .unwrap_or(u32::MAX),
            input_tokens: u64::try_from(project_row.input_tokens).unwrap_or(0),
            last_session_updated_at: project_row.last_session_updated_at,
            output_tokens: u64::try_from(project_row.output_tokens).unwrap_or(0),
            project,
            session_count: u32::try_from(project_row.session_count).unwrap_or(u32::MAX),
        }
    }

    /// Resolves the active project title used for startup rendering.
    pub(crate) fn project_title_for_id(
        project_items: &[ProjectListItem],
        project_id: i64,
        fallback_path: &Path,
    ) -> String {
        if let Some(project_item) = project_items
            .iter()
            .find(|project_item| project_item.project.id == project_id)
        {
            return project_item.project.display_label();
        }

        project_name_from_path(fallback_path)
    }

    /// Resolves the configured upstream reference for one project branch.
    pub(crate) async fn load_git_upstream_ref(
        git_client: &dyn GitClient,
        working_dir: &Path,
        git_branch: Option<&str>,
    ) -> Option<String> {
        git_branch?;

        git_client
            .current_upstream_reference(working_dir.to_path_buf())
            .await
            .ok()
    }

    /// Resolves startup active project id from settings.
    pub(crate) async fn resolve_startup_active_project_id(
        db: &AppRepositories,
        fs_client: &dyn FsClient,
        current_project_id: i64,
    ) -> i64 {
        let Some(stored_project_id) = db.settings().load_active_project_id().await.ok().flatten()
        else {
            return current_project_id;
        };
        let Some(project_row) = db
            .projects()
            .get_project(stored_project_id)
            .await
            .ok()
            .flatten()
        else {
            return current_project_id;
        };
        if !Self::is_existing_project_path(fs_client, project_row.path.as_str()) {
            return current_project_id;
        }

        stored_project_id
    }

    /// Loads one project and falls back to the current working directory.
    pub(crate) async fn load_project(
        db: &AppRepositories,
        project_id: i64,
        fallback_working_dir: &Path,
        fallback_git_branch: Option<&str>,
    ) -> Project {
        if let Some(project_row) = db.projects().get_project(project_id).await.ok().flatten() {
            return Self::project_from_row(project_row);
        }

        Project {
            created_at: 0,
            display_name: None,
            git_branch: fallback_git_branch.map(str::to_string),
            id: project_id,
            is_favorite: false,
            last_opened_at: None,
            path: fallback_working_dir.to_path_buf(),
            updated_at: 0,
        }
    }
}

#[cfg(test)]
#[path = "startup_test.rs"]
mod tests;
