//! Hidden support APIs for tests.
//!
//! These helpers intentionally live outside the production-facing module
//! surface so tests can share canonical naming and render-buffer rules without
//! widening app APIs.

use std::path::{Path, PathBuf};
#[cfg(test)]
use std::process::Command;
#[cfg(test)]
use std::sync::Arc;
#[cfg(test)]
use std::time::{Instant, SystemTime};

#[cfg(test)]
use ag_git as git;
use ratatui::buffer::{Buffer, Cell};
#[cfg(test)]
use ratatui::widgets::TableState;

use crate::app;
#[cfg(test)]
use crate::app::{App, SessionManager, SessionState};
use crate::db::{Database, DbError};
#[cfg(test)]
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel};
#[cfg(test)]
use crate::domain::question::QuestionItem;
#[cfg(test)]
use crate::domain::session::{
    PublishedBranchSyncStatus, ReviewRequest, Session, SessionHandles, SessionId, SessionSize,
    SessionStats, Status,
};
use crate::domain::setting::SettingName;
#[cfg(test)]
use crate::infra::{agent, app_server};
#[cfg(test)]
use crate::ui::state::app_mode::AppMode;

/// Returns the canonical session folder path for integration-test fixtures.
pub fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    app::session::session_folder(base, session_id)
}

/// Persists the active project id for integration-test database setup.
pub async fn persist_active_project_id_for_test(
    database: &Database,
    project_id: i64,
) -> Result<(), DbError> {
    sqlx::query(
        r"
INSERT INTO setting (name, value)
VALUES (?, ?)
ON CONFLICT(name) DO UPDATE
SET value = excluded.value
",
    )
    .bind(SettingName::ActiveProjectId.as_str())
    .bind(project_id.to_string())
    .execute(database.pool())
    .await?;

    Ok(())
}

/// Deterministic [`crate::infra::clock::Clock`] implementation for unit-test
/// fixtures.
#[cfg(test)]
pub(crate) struct FixedClock {
    instant: Instant,
    system_time: SystemTime,
}

#[cfg(test)]
impl FixedClock {
    /// Creates a fixed clock pinned to the given monotonic and system times.
    pub(crate) fn new(instant: Instant, system_time: SystemTime) -> Self {
        Self {
            instant,
            system_time,
        }
    }

    /// Creates a fixed clock whose wall time is Unix epoch and whose instant
    /// starts at construction time.
    pub(crate) fn unix_epoch() -> Self {
        Self::new(Instant::now(), SystemTime::UNIX_EPOCH)
    }
}

#[cfg(test)]
impl crate::infra::clock::Clock for FixedClock {
    fn now_instant(&self) -> Instant {
        self.instant
    }

    fn now_system_time(&self) -> SystemTime {
        self.system_time
    }
}

/// Chainable builder that produces deterministic [`Session`] values for unit
/// tests.
#[cfg(test)]
pub(crate) struct SessionFixtureBuilder {
    session: Session,
}

#[cfg(test)]
impl SessionFixtureBuilder {
    /// Creates a builder seeded with minimal deterministic defaults that match
    /// the common session snapshot used across app, runtime, and UI tests.
    pub(crate) fn new() -> Self {
        Self {
            session: Session {
                agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini3FlashPreview),
                base_branch: "main".to_string(),
                created_at: 0,
                draft_attachments: Vec::new(),
                folder: PathBuf::new(),
                follow_up_tasks: Vec::new(),
                id: SessionId::from("session-id"),
                in_progress_started_at: None,
                in_progress_total_seconds: 0,
                is_draft: false,
                output: String::new(),
                parent_session_id: None,
                project_name: "project".to_string(),
                prompt: String::new(),
                queued_messages: Vec::new(),
                reasoning_level_override: None,
                published_upstream_ref: None,
                published_branch_sync_status: PublishedBranchSyncStatus::Idle,
                questions: Vec::new(),
                review_request: None,
                size: SessionSize::Xs,
                stats: SessionStats::default(),
                status: Status::Review,
                summary: None,
                title: None,
                transcript: None,
                updated_at: 0,
                workflow_notice: None,
            },
        }
    }

    /// Overrides the selected agent.
    pub(crate) fn agent(mut self, agent: AgentSelection) -> Self {
        self.session.agent = agent;

        self
    }

    /// Overrides the draft flag.
    pub(crate) fn draft(mut self, is_draft: bool) -> Self {
        self.session.is_draft = is_draft;

        self
    }

    /// Overrides the worktree folder.
    pub(crate) fn folder(mut self, folder: PathBuf) -> Self {
        self.session.folder = folder;

        self
    }

    /// Overrides the stable session identifier.
    pub(crate) fn id(mut self, id: impl Into<SessionId>) -> Self {
        self.session.id = id.into();

        self
    }

    /// Overrides the agent model while preserving the current agent kind.
    pub(crate) fn model(mut self, model: AgentModel) -> Self {
        self.session.agent = AgentSelection::new(self.session.agent.kind(), model);

        self
    }

    /// Overrides the captured output transcript.
    pub(crate) fn output(mut self, output: impl Into<String>) -> Self {
        self.session.output = output.into();

        self
    }

    /// Overrides the optional stacked-session parent identifier.
    pub(crate) fn parent_session_id(mut self, parent_session_id: Option<SessionId>) -> Self {
        self.session.parent_session_id = parent_session_id;

        self
    }

    /// Overrides the project name.
    pub(crate) fn project_name(mut self, project_name: impl Into<String>) -> Self {
        self.session.project_name = project_name.into();

        self
    }

    /// Overrides the user prompt text.
    pub(crate) fn prompt(mut self, prompt: impl Into<String>) -> Self {
        self.session.prompt = prompt.into();

        self
    }

    /// Overrides the pending clarification questions.
    pub(crate) fn questions(mut self, questions: Vec<QuestionItem>) -> Self {
        self.session.questions = questions;

        self
    }

    /// Overrides the session-scoped reasoning level override.
    pub(crate) fn reasoning_level_override(
        mut self,
        reasoning_level_override: Option<ReasoningLevel>,
    ) -> Self {
        self.session.reasoning_level_override = reasoning_level_override;

        self
    }

    /// Overrides the persisted forge review request.
    pub(crate) fn review_request(mut self, review_request: Option<ReviewRequest>) -> Self {
        self.session.review_request = review_request;

        self
    }

    /// Overrides the lifecycle status.
    pub(crate) fn status(mut self, status: Status) -> Self {
        self.session.status = status;

        self
    }

    /// Overrides the optional persisted session summary text.
    pub(crate) fn summary(mut self, summary: Option<String>) -> Self {
        self.session.summary = summary;

        self
    }

    /// Overrides the optional explicit session title.
    pub(crate) fn title(mut self, title: Option<String>) -> Self {
        self.session.title = title;

        self
    }

    /// Consumes the builder and returns the fully populated fixture.
    pub(crate) fn build(self) -> Session {
        self.session
    }
}

/// Builds a minimal session fixture with the given identifier and status.
#[cfg(test)]
pub(crate) fn session_fixture(session_id: &str, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .status(status)
        .folder(PathBuf::from("/tmp/test"))
        .build()
}

/// Builds a session fixture whose title matches its identifier.
#[cfg(test)]
pub(crate) fn titled_session_fixture(session_id: &str, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .status(status)
        .title(Some(session_id.to_string()))
        .build()
}

/// Builds a review-state session fixture rooted at the given folder.
#[cfg(test)]
pub(crate) fn session_fixture_with_folder(session_folder: PathBuf) -> Session {
    SessionFixtureBuilder::new()
        .id("session-1")
        .folder(session_folder)
        .project_name("test-project")
        .prompt("test prompt")
        .build()
}

/// Returns a mock app-server client wrapped in `Arc` for test injection.
#[cfg(test)]
pub(crate) fn mock_app_server() -> Arc<dyn app_server::AppServerClient> {
    Arc::new(app_server::MockAppServerClient::new())
}

/// Builds one client bundle with a caller-provided agent availability
/// snapshot.
#[cfg(test)]
pub(crate) fn test_app_clients_with_available_agent_kinds(
    available_agent_kinds: Vec<AgentKind>,
) -> app::AppClients {
    app::AppClients::new().with_agent_availability_probe(Arc::new(
        agent::StaticAgentAvailabilityProbe {
            available_agent_kinds,
        },
    ))
}

/// Builds one client bundle with deterministic agent availability for test
/// app startup.
#[cfg(test)]
pub(crate) fn test_app_clients() -> app::AppClients {
    test_app_clients_with_available_agent_kinds(AgentKind::ALL.to_vec())
}

/// Builds one client bundle with deterministic agent availability and a mock
/// app-server override.
#[cfg(test)]
pub(crate) fn test_app_clients_with_mock_app_server() -> app::AppClients {
    test_app_clients().with_app_server_client_override(mock_app_server())
}

/// Builds one app rooted at a retained temporary directory using the given
/// clients.
#[cfg(test)]
pub(crate) async fn new_test_app_with_clients(
    clients: app::AppClients,
) -> (App, tempfile::TempDir) {
    let base_dir = tempfile::tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let app = App::new_with_clients(base_path.clone(), base_path, None, database, clients)
        .await
        .expect("failed to build app");

    (app, base_dir)
}

/// Builds one app rooted at a retained temporary directory.
#[cfg(test)]
pub(crate) async fn new_test_app() -> (App, tempfile::TempDir) {
    new_test_app_with_clients(test_app_clients()).await
}

/// Builds one app rooted at a retained temporary directory with a mocked tmux
/// boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_test_app_with_mock_tmux_client() -> (App, tempfile::TempDir) {
    new_test_app_with_tmux_client(Arc::new(crate::infra::tmux::MockTmuxClient::new())).await
}

/// Builds one app rooted at a retained temporary directory with an injected
/// tmux boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_test_app_with_tmux_client(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> (App, tempfile::TempDir) {
    let clients = test_app_clients_with_mock_app_server().with_tmux_client(tmux_client);

    new_test_app_with_clients(clients).await
}

/// Builds one app with an injected tmux boundary, then intentionally drops
/// the temporary directory guard before returning.
#[cfg(test)]
pub(crate) async fn new_test_app_with_tmux_client_without_retained_base_dir(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> App {
    let (app, _base_dir) = new_test_app_with_tmux_client(tmux_client).await;

    app
}

/// Builds one app and intentionally drops the temporary directory guard before
/// returning, matching tests that only need in-memory state.
#[cfg(test)]
pub(crate) async fn new_test_app_without_retained_base_dir() -> App {
    let (app, _base_dir) = new_test_app().await;

    app
}

/// Initializes a minimal git repository for retained-tempdir app fixtures.
#[cfg(test)]
pub(crate) fn setup_test_git_repo(path: &Path) {
    Command::new("git")
        .args(["init"])
        .current_dir(path)
        .output()
        .expect("git init failed");
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .expect("git config failed");
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .expect("git config failed");
    std::fs::write(path.join("README.md"), "test").expect("write failed");
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .expect("git add failed");
    Command::new("git")
        .args(["commit", "-m", "Initial commit"])
        .current_dir(path)
        .output()
        .expect("git commit failed");
    Command::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(path)
        .output()
        .expect("git branch failed");
}

/// Builds one git-backed app rooted at a retained temporary directory using
/// the given clients.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_clients(
    clients: app::AppClients,
) -> (App, tempfile::TempDir) {
    let base_dir = tempfile::tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    setup_test_git_repo(base_dir.path());
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build app");

    (app, base_dir)
}

/// Builds one git-backed app rooted at a retained temporary directory.
#[cfg(test)]
pub(crate) async fn new_git_test_app() -> (App, tempfile::TempDir) {
    new_git_test_app_with_clients(test_app_clients()).await
}

/// Builds one git-backed app rooted at a retained temporary directory with a
/// mocked tmux boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_mock_tmux_client() -> (App, tempfile::TempDir) {
    new_git_test_app_with_tmux_client(Arc::new(crate::infra::tmux::MockTmuxClient::new())).await
}

/// Builds one git-backed app rooted at a retained temporary directory with an
/// injected tmux boundary and app-server override.
#[cfg(test)]
pub(crate) async fn new_git_test_app_with_tmux_client(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> (App, tempfile::TempDir) {
    let clients = test_app_clients_with_mock_app_server().with_tmux_client(tmux_client);

    new_git_test_app_with_clients(clients).await
}

/// Builds a session manager fixture with the provided sessions and handles.
#[cfg(test)]
pub(crate) fn session_manager_with_handles(
    sessions: Vec<Session>,
    handles: std::collections::HashMap<SessionId, SessionHandles>,
) -> SessionManager {
    SessionManager::new(
        app::session::SessionDefaults {
            model: AgentKind::Antigravity.default_model(),
        },
        Arc::new(git::MockGitClient::new()),
        SessionState::new(
            handles,
            sessions,
            TableState::default(),
            Arc::new(FixedClock::unix_epoch()),
            0,
            0,
        ),
        Vec::new(),
    )
}

/// Builds a session manager fixture with the provided sessions and no runtime
/// handles.
#[cfg(test)]
pub(crate) fn session_manager_with_sessions(sessions: Vec<Session>) -> SessionManager {
    session_manager_with_handles(sessions, std::collections::HashMap::new())
}

/// Sets a session status in both the session snapshot and its live handles,
/// when either exists.
#[cfg(test)]
pub(crate) fn set_session_status_for_test(app: &mut App, session_id: &str, status: Status) {
    if let Some(session) = app
        .sessions
        .sessions_mut()
        .iter_mut()
        .find(|session| session.id == session_id)
    {
        session.status = status;
    }

    if let Some(handles) = app.sessions.session_handles().get(session_id)
        && let Ok(mut current_status) = handles.status.lock()
    {
        *current_status = status;
    }
}

/// Switches a test app into review-detail mode with the provided review.
#[cfg(test)]
pub(crate) fn set_review_detail_mode(app: &mut App, review: ag_forge::RequestedReview) {
    app.mode = AppMode::ReviewDetail {
        comment_error: None,
        is_loading_comments: false,
        review,
        scroll_offset: 0,
    };
}

/// Returns the first rendered cell for a contiguous text match in a test
/// buffer.
pub fn rendered_text_start_cell<'a>(buffer: &'a Buffer, needle: &str) -> Option<&'a Cell> {
    rendered_text_start_cells(buffer, needle).into_iter().next()
}

/// Returns rendered start cells for every contiguous text match in a test
/// buffer.
pub fn rendered_text_start_cells<'a>(buffer: &'a Buffer, needle: &str) -> Vec<&'a Cell> {
    let width = usize::from(buffer.area.width.max(1));
    let needle_symbols = needle.chars().map(|character| character.to_string());
    let needle_symbols = needle_symbols.collect::<Vec<_>>();
    let content = buffer.content();
    let mut cells = Vec::new();

    for row_start in (0..content.len()).step_by(width) {
        let row_end = row_start + width.min(content.len().saturating_sub(row_start));
        let row = &content[row_start..row_end];

        for (index, window) in row.windows(needle_symbols.len()).enumerate() {
            let window_matches = window
                .iter()
                .zip(&needle_symbols)
                .all(|(cell, symbol)| cell.symbol() == symbol);

            if window_matches {
                cells.push(&row[index]);
            }
        }
    }

    cells
}

#[cfg(test)]
mod tests {
    use ratatui::style::{Color, Style};

    use super::*;

    #[test]
    fn rendered_text_start_cell_returns_first_match() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
        buffer.set_string(1, 0, "one", Style::default().fg(Color::Green));
        buffer.set_string(1, 1, "one", Style::default().fg(Color::Yellow));

        // Act
        let cell = rendered_text_start_cell(&buffer, "one").expect("text should render");

        // Assert
        assert_eq!(cell.fg, Color::Green);
    }

    #[test]
    fn rendered_text_start_cells_returns_all_matches() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 2));
        buffer.set_string(1, 0, "same", Style::default().fg(Color::Green));
        buffer.set_string(1, 1, "same", Style::default().fg(Color::Yellow));

        // Act
        let cells = rendered_text_start_cells(&buffer, "same");
        let colors = cells.iter().map(|cell| cell.fg).collect::<Vec<_>>();

        // Assert
        assert_eq!(colors, vec![Color::Green, Color::Yellow]);
    }

    #[test]
    fn rendered_text_start_cell_returns_none_for_missing_text() {
        // Arrange
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 12, 1));
        buffer.set_string(1, 0, "present", Style::default());

        // Act
        let cell = rendered_text_start_cell(&buffer, "missing");

        // Assert
        assert!(cell.is_none());
    }
}
