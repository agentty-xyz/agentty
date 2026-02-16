//! App-layer composition root and shared state container.
//!
//! This module wires app submodules and exposes [`App`] and [`SessionState`]
//! used by runtime mode handlers.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use ratatui::widgets::TableState;
use tokio::sync::mpsc;

use crate::agent::{AgentKind, AgentModel};
use crate::db::Database;
use crate::model::{AppMode, PermissionMode, Project, Session, SessionHandles, Status, Tab};

mod pr;
mod project;
pub(crate) mod session;
mod task;
mod title;
mod worker;

/// Relative directory name used for session git worktrees under `~/.agentty`.
pub const AGENTTY_WT_DIR: &str = "wt";

/// Returns the agentty home directory (`~/.agentty`).
pub fn agentty_home() -> PathBuf {
    if let Some(home_dir) = dirs::home_dir() {
        return home_dir.join(".agentty");
    }

    PathBuf::from(".agentty")
}

/// Internal app events emitted by background workers and workflows.
///
/// Producers should emit events only; state mutation is centralized in
/// [`App::apply_app_events`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum AppEvent {
    /// Indicates latest ahead/behind information from the git status worker.
    GitStatusUpdated { status: Option<(u32, u32)> },
    /// Indicates a session history reset has been persisted.
    SessionHistoryCleared { session_id: String },
    /// Indicates a session agent/model selection has been persisted.
    SessionAgentModelUpdated {
        session_agent: AgentKind,
        session_id: String,
        session_model: AgentModel,
    },
    /// Indicates a session permission mode selection has been persisted.
    SessionPermissionModeUpdated {
        permission_mode: PermissionMode,
        session_id: String,
    },
    /// Indicates a PR creation task has finished for a session.
    PrCreationCleared { session_id: String },
    /// Indicates a PR polling task has stopped for a session.
    PrPollingStopped { session_id: String },
    /// Requests a full session list refresh.
    RefreshSessions,
    /// Indicates that a session handle snapshot changed in-memory.
    SessionUpdated { session_id: String },
}

#[derive(Default)]
struct AppEventBatch {
    cleared_pr_creation_ids: HashSet<String>,
    cleared_session_history_ids: HashSet<String>,
    git_status_update: Option<(u32, u32)>,
    has_git_status_update: bool,
    session_agent_model_updates: HashMap<String, (AgentKind, AgentModel)>,
    session_ids: HashSet<String>,
    session_permission_mode_updates: HashMap<String, PermissionMode>,
    should_force_reload: bool,
    stopped_pr_poll_ids: HashSet<String>,
}

/// Holds all in-memory state related to session listing and refresh tracking.
pub struct SessionState {
    pub handles: HashMap<String, SessionHandles>,
    pub sessions: Vec<Session>,
    pub table_state: TableState,
    refresh_deadline: std::time::Instant,
    row_count: i64,
    updated_at_max: i64,
}

impl SessionState {
    /// Creates a new [`SessionState`] with initial refresh metadata.
    pub fn new(
        handles: HashMap<String, SessionHandles>,
        sessions: Vec<Session>,
        table_state: TableState,
        row_count: i64,
        updated_at_max: i64,
    ) -> Self {
        Self {
            handles,
            sessions,
            table_state,
            refresh_deadline: std::time::Instant::now() + session::SESSION_REFRESH_INTERVAL,
            row_count,
            updated_at_max,
        }
    }

    /// Copies current values from one runtime handle into its `Session`
    /// snapshot.
    pub fn sync_session_from_handle(&mut self, session_id: &str) {
        let Some(session_handles) = self.handles.get(session_id) else {
            return;
        };
        let Some(session) = self
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        else {
            return;
        };

        Self::sync_session_with_handles(session, session_handles);
    }

    /// Copies current values from runtime handles into plain `Session` fields.
    pub fn sync_from_handles(&mut self) {
        let handles = &self.handles;

        for session in &mut self.sessions {
            let Some(session_handles) = handles.get(&session.id) else {
                continue;
            };

            Self::sync_session_with_handles(session, session_handles);
        }
    }

    fn sync_session_with_handles(session: &mut Session, session_handles: &SessionHandles) {
        if let Ok(output) = session_handles.output.lock()
            && session.output.len() != output.len()
        {
            session.output.clone_from(&*output);
        }

        if let Ok(status) = session_handles.status.lock() {
            session.status = *status;
        }

        if let Ok(count) = session_handles.commit_count.lock() {
            session.commit_count = *count;
        }
    }
}

/// Stores application state and coordinates session/project workflows.
pub struct App {
    pub current_tab: Tab,
    pub mode: AppMode,
    pub projects: Vec<Project>,
    pub session_state: SessionState,
    active_project_id: i64,
    event_rx: mpsc::UnboundedReceiver<AppEvent>,
    event_tx: mpsc::UnboundedSender<AppEvent>,
    base_path: PathBuf,
    db: Database,
    default_session_agent: AgentKind,
    default_session_model: AgentModel,
    default_session_permission_mode: PermissionMode,
    git_branch: Option<String>,
    git_status: Option<(u32, u32)>,
    git_status_cancel: Arc<AtomicBool>,
    pr_creation_in_flight: Arc<Mutex<HashSet<String>>>,
    pr_poll_cancel: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
    session_workers: HashMap<String, mpsc::UnboundedSender<worker::SessionCommand>>,
    working_dir: PathBuf,
}

impl App {
    /// Builds the app state from persisted data and starts background
    /// housekeeping tasks.
    pub async fn new(
        base_path: PathBuf,
        working_dir: PathBuf,
        git_branch: Option<String>,
        db: Database,
    ) -> Self {
        let active_project_id = db
            .upsert_project(&working_dir.to_string_lossy(), git_branch.as_deref())
            .await
            .unwrap_or(0);

        let _ = db.backfill_session_project(active_project_id).await;

        Self::discover_sibling_projects(&working_dir, &db).await;
        Self::fail_unfinished_operations_from_previous_run(&db).await;

        let projects = Self::load_projects_from_db(&db).await;

        let mut table_state = TableState::default();
        let mut handles = HashMap::new();
        let sessions = Self::load_sessions(&base_path, &db, &projects, &mut handles).await;
        let (sessions_row_count, sessions_updated_at_max) =
            db.load_sessions_metadata().await.unwrap_or((0, 0));
        let (default_session_agent, default_session_model, default_session_permission_mode) =
            sessions.first().map_or(
                (
                    AgentKind::Gemini,
                    AgentKind::Gemini.default_model(),
                    PermissionMode::default(),
                ),
                |session| {
                    let (session_agent, session_model) =
                        Self::resolve_session_agent_and_model(session);

                    (session_agent, session_model, session.permission_mode)
                },
            );
        if sessions.is_empty() {
            table_state.select(None);
        } else {
            table_state.select(Some(0));
        }

        let git_status = None;
        let git_status_cancel = Arc::new(AtomicBool::new(false));
        let pr_creation_in_flight = Arc::new(Mutex::new(HashSet::new()));
        let pr_poll_cancel = Arc::new(Mutex::new(HashMap::new()));
        let (event_tx, event_rx) = mpsc::unbounded_channel();

        if git_branch.is_some() {
            Self::spawn_git_status_task(
                &working_dir,
                Arc::clone(&git_status_cancel),
                event_tx.clone(),
            );
        }

        let app = Self {
            current_tab: Tab::Sessions,
            mode: AppMode::List,
            session_state: SessionState::new(
                handles,
                sessions,
                table_state,
                sessions_row_count,
                sessions_updated_at_max,
            ),
            active_project_id,
            event_rx,
            event_tx,
            base_path,
            db,
            default_session_agent,
            default_session_model,
            default_session_permission_mode,
            git_branch,
            git_status,
            git_status_cancel,
            pr_creation_in_flight,
            pr_poll_cancel,
            projects,
            session_workers: HashMap::new(),
            working_dir,
        };

        app.start_pr_polling_for_pull_request_sessions();

        app
    }

    /// Returns the active project identifier.
    pub fn active_project_id(&self) -> i64 {
        self.active_project_id
    }

    /// Returns the working directory for the active project.
    pub fn working_dir(&self) -> &Path {
        self.working_dir.as_path()
    }

    /// Returns the git branch of the active project, when available.
    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Returns the latest ahead/behind snapshot from reducer-applied events.
    pub fn git_status_info(&self) -> Option<(u32, u32)> {
        self.git_status
    }

    /// Returns whether the onboarding screen should be shown.
    pub fn should_show_onboarding(&self) -> bool {
        self.session_state.sessions.is_empty()
    }

    /// Applies one or more queued app events through a single reducer path.
    ///
    /// This method drains currently queued app events, coalesces refresh and
    /// git-status updates, then applies session-handle sync for touched
    /// sessions.
    pub(crate) async fn apply_app_events(&mut self, first_event: AppEvent) {
        let drained_events = self.drain_app_events(first_event);
        let event_batch = Self::reduce_app_events(drained_events);

        self.apply_app_event_batch(event_batch).await;
    }

    /// Enqueues an app event onto the internal app event bus.
    pub(crate) fn emit_app_event(&self, event: AppEvent) {
        let _ = self.event_tx.send(event);
    }

    /// Processes currently queued app events without waiting.
    pub(crate) async fn process_pending_app_events(&mut self) {
        let Ok(first_event) = self.event_rx.try_recv() else {
            return;
        };

        self.apply_app_events(first_event).await;
    }

    /// Emits one app event and immediately processes the pending app queue.
    pub(crate) async fn dispatch_app_event(&mut self, event: AppEvent) {
        self.emit_app_event(event);
        self.process_pending_app_events().await;
    }

    /// Returns a clone of the internal app event sender.
    pub(crate) fn app_event_sender(&self) -> mpsc::UnboundedSender<AppEvent> {
        self.event_tx.clone()
    }

    /// Waits for the next internal app event.
    pub(crate) async fn next_app_event(&mut self) -> Option<AppEvent> {
        self.event_rx.recv().await
    }

    fn drain_app_events(&mut self, first_event: AppEvent) -> Vec<AppEvent> {
        let mut events = vec![first_event];
        while let Ok(event) = self.event_rx.try_recv() {
            events.push(event);
        }

        events
    }

    fn reduce_app_events(events: Vec<AppEvent>) -> AppEventBatch {
        let mut event_batch = AppEventBatch::default();
        for event in events {
            event_batch.collect_event(event);
        }

        event_batch
    }

    async fn apply_app_event_batch(&mut self, event_batch: AppEventBatch) {
        if event_batch.should_force_reload {
            self.refresh_sessions_now().await;
        }

        if event_batch.has_git_status_update {
            self.git_status = event_batch.git_status_update;
        }

        if let Ok(mut in_flight) = self.pr_creation_in_flight.lock() {
            for session_id in &event_batch.cleared_pr_creation_ids {
                in_flight.remove(session_id);
            }
        }

        if let Ok(mut polling) = self.pr_poll_cancel.lock() {
            for session_id in &event_batch.stopped_pr_poll_ids {
                polling.remove(session_id);
            }
        }

        for session_id in &event_batch.cleared_session_history_ids {
            self.apply_session_history_cleared(session_id);
        }

        for (session_id, (session_agent, session_model)) in event_batch.session_agent_model_updates
        {
            self.apply_session_agent_model_updated(&session_id, session_agent, session_model);
        }

        for (session_id, permission_mode) in event_batch.session_permission_mode_updates {
            self.apply_session_permission_mode_updated(&session_id, permission_mode);
        }

        for session_id in &event_batch.session_ids {
            self.session_state.sync_session_from_handle(session_id);
        }
    }

    fn apply_session_history_cleared(&mut self, session_id: &str) {
        if let Some(handles) = self.session_state.handles.get(session_id) {
            if let Ok(mut output_buffer) = handles.output.lock() {
                output_buffer.clear();
            }
            if let Ok(mut status_value) = handles.status.lock() {
                *status_value = Status::New;
            }
        }

        if let Some(session) = self
            .session_state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.output.clear();
            session.prompt = String::new();
            session.title = None;
            session.status = Status::New;
        }
    }

    fn apply_session_agent_model_updated(
        &mut self,
        session_id: &str,
        session_agent: AgentKind,
        session_model: AgentModel,
    ) {
        if let Some(session) = self
            .session_state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.agent = session_agent.to_string();
            session.model = session_model.as_str().to_string();
        }
        self.default_session_agent = session_agent;
        self.default_session_model = session_model;
    }

    fn apply_session_permission_mode_updated(
        &mut self,
        session_id: &str,
        permission_mode: PermissionMode,
    ) {
        if let Some(session) = self
            .session_state
            .sessions
            .iter_mut()
            .find(|session| session.id == session_id)
        {
            session.permission_mode = permission_mode;
        }
        self.default_session_permission_mode = permission_mode;
    }
}

impl AppEventBatch {
    fn collect_event(&mut self, event: AppEvent) {
        match event {
            AppEvent::GitStatusUpdated { status } => {
                self.has_git_status_update = true;
                self.git_status_update = status;
            }
            AppEvent::SessionHistoryCleared { session_id } => {
                self.cleared_session_history_ids.insert(session_id);
            }
            AppEvent::SessionAgentModelUpdated {
                session_agent,
                session_id,
                session_model,
            } => {
                self.session_agent_model_updates
                    .insert(session_id, (session_agent, session_model));
            }
            AppEvent::SessionPermissionModeUpdated {
                permission_mode,
                session_id,
            } => {
                self.session_permission_mode_updates
                    .insert(session_id, permission_mode);
            }
            AppEvent::PrCreationCleared { session_id } => {
                self.cleared_pr_creation_ids.insert(session_id);
            }
            AppEvent::PrPollingStopped { session_id } => {
                self.stopped_pr_poll_ids.insert(session_id);
            }
            AppEvent::RefreshSessions => {
                self.should_force_reload = true;
            }
            AppEvent::SessionUpdated { session_id } => {
                self.session_ids.insert(session_id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    #[tokio::test]
    async fn test_should_show_onboarding_when_database_is_empty() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");

        // Act
        let app = App::new(base_path, working_dir, None, database).await;

        // Assert
        assert!(app.should_show_onboarding());
    }

    #[tokio::test]
    async fn test_should_show_onboarding_when_database_has_sessions() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let project_id = database
            .upsert_project(&working_dir.to_string_lossy(), None)
            .await
            .expect("failed to upsert project");
        database
            .insert_session(
                "seed0000",
                "gemini",
                "gemini-2.5-flash",
                "main",
                "Done",
                project_id,
            )
            .await
            .expect("failed to insert session");

        // Act
        let app = App::new(base_path, working_dir, None, database).await;

        // Assert
        assert!(!app.should_show_onboarding());
    }

    #[test]
    fn test_app_event_batch_collects_git_status_and_refresh() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::GitStatusUpdated {
            status: Some((2, 1)),
        });
        event_batch.collect_event(AppEvent::RefreshSessions);

        // Assert
        assert!(event_batch.has_git_status_update);
        assert_eq!(event_batch.git_status_update, Some((2, 1)));
        assert!(event_batch.should_force_reload);
    }

    #[test]
    fn test_app_event_batch_collects_session_updates() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::SessionUpdated {
            session_id: "session-1".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionHistoryCleared {
            session_id: "session-2".to_string(),
        });
        event_batch.collect_event(AppEvent::SessionAgentModelUpdated {
            session_agent: AgentKind::Claude,
            session_id: "session-3".to_string(),
            session_model: AgentModel::Claude(crate::agent::ClaudeModel::ClaudeOpus46),
        });
        event_batch.collect_event(AppEvent::SessionPermissionModeUpdated {
            permission_mode: PermissionMode::Autonomous,
            session_id: "session-4".to_string(),
        });

        // Assert
        assert!(event_batch.session_ids.contains("session-1"));
        assert!(
            event_batch
                .cleared_session_history_ids
                .contains("session-2")
        );
        assert_eq!(
            event_batch.session_agent_model_updates.get("session-3"),
            Some(&(
                AgentKind::Claude,
                AgentModel::Claude(crate::agent::ClaudeModel::ClaudeOpus46)
            ))
        );
        assert_eq!(
            event_batch.session_permission_mode_updates.get("session-4"),
            Some(&PermissionMode::Autonomous)
        );
    }

    #[test]
    fn test_app_event_batch_collects_pr_updates() {
        // Arrange
        let mut event_batch = AppEventBatch::default();

        // Act
        event_batch.collect_event(AppEvent::PrCreationCleared {
            session_id: "session-1".to_string(),
        });
        event_batch.collect_event(AppEvent::PrPollingStopped {
            session_id: "session-2".to_string(),
        });

        // Assert
        assert!(event_batch.cleared_pr_creation_ids.contains("session-1"));
        assert!(event_batch.stopped_pr_poll_ids.contains("session-2"));
    }

    #[tokio::test]
    async fn test_apply_app_events_updates_git_status() {
        // Arrange
        let temp_dir = tempdir().expect("failed to create temp dir");
        let base_path = temp_dir.path().join("wt");
        let working_dir = temp_dir.path().to_path_buf();
        let database = Database::open_in_memory()
            .await
            .expect("failed to open in-memory db");
        let mut app = App::new(base_path, working_dir, None, database).await;

        // Act
        app.apply_app_events(AppEvent::GitStatusUpdated {
            status: Some((5, 3)),
        })
        .await;

        // Assert
        assert_eq!(app.git_status_info(), Some((5, 3)));
    }
}
