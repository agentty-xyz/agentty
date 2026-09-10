//! Hidden support APIs for tests.
//!
//! These helpers intentionally live outside the production-facing module
//! surface so tests can share canonical naming and render-buffer rules without
//! widening app APIs.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Instant, SystemTime};

use ag_agent::{AppServerClient, MockAppServerClient, StaticAgentAvailabilityProbe};
use ag_git as git;
use ratatui::buffer::{Buffer, Cell};
use tracing::field::{Field, Visit};
use tracing::subscriber::{Interest, Subscriber};
use tracing::{Event, Level, Metadata, span};

use crate::app;
use crate::app::{App, SessionManager, SessionState};
use crate::db::Database;
use crate::domain::agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel};
use crate::domain::question::QuestionItem;
use crate::domain::selection::SelectionState;
use crate::domain::session::{
    ReviewRequest, Session, SessionHandles, SessionId, SessionRole, SessionSize, SessionStats,
    Status,
};
use crate::domain::session_message::{SessionMessage, SessionMessageKind, SessionTranscript};
use crate::domain::transient_message::TransientMessageStore;
use crate::infra::project_discovery::MockProjectDiscoveryClient;
/// Subscriber that enables tracing fields while unit tests exercise warning
/// paths under source coverage.
#[derive(Debug)]
pub(crate) struct TestSubscriber;

impl Subscriber for TestSubscriber {
    fn enabled(&self, _metadata: &Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, span: &span::Attributes<'_>) -> span::Id {
        span.record(&mut TestVisitor);

        span::Id::from_u64(1)
    }

    fn record(&self, _span: &span::Id, values: &span::Record<'_>) {
        values.record(&mut TestVisitor);
    }

    fn record_follows_from(&self, _span: &span::Id, _follows: &span::Id) {}

    fn event(&self, event: &Event<'_>) {
        event.record(&mut TestVisitor);
    }

    fn enter(&self, _span: &span::Id) {}

    fn exit(&self, _span: &span::Id) {}

    fn register_callsite(&self, _metadata: &'static Metadata<'static>) -> Interest {
        Interest::sometimes()
    }

    fn max_level_hint(&self) -> Option<tracing::metadata::LevelFilter> {
        Some(Level::TRACE.into())
    }
}

struct TestVisitor;

impl Visit for TestVisitor {
    fn record_debug(&mut self, _field: &Field, value: &dyn std::fmt::Debug) {
        let _rendered = format!("{value:?}");
    }
}

/// Deterministic [`crate::infra::clock::Clock`] implementation for unit-test
/// fixtures.
pub(crate) struct FixedClock {
    instant: Instant,
    system_time: SystemTime,
}

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
pub(crate) struct SessionFixtureBuilder {
    session: Session,
}

impl SessionFixtureBuilder {
    /// Creates a builder seeded with minimal deterministic defaults that match
    /// the common session snapshot used across app, runtime, and UI tests.
    pub(crate) fn new() -> Self {
        Self {
            session: Session {
                agent: AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini38Flash),
                base_branch: "main".to_string(),
                created_at: 0,
                draft_attachments: Vec::new(),
                folder: PathBuf::new(),
                follow_up_tasks: Vec::new(),
                id: SessionId::from("session-id"),
                in_progress_started_at: None,
                in_progress_total_seconds: 0,
                is_draft: false,
                controller_session_id: None,
                orchestration_progress: None,
                role: SessionRole::default(),
                parent_session_id: None,
                permission_mode: crate::domain::permission::PermissionMode::default(),
                personality_id: None,
                project_name: "project".to_string(),
                prompt: String::new(),
                queued_messages: Vec::new(),
                reasoning_level_override: None,
                response_style: crate::domain::agent::ResponseStyle::default(),
                published_upstream_ref: None,
                questions: Vec::new(),
                review_request: None,
                size: SessionSize::Xs,
                speed_mode: crate::domain::agent::SpeedMode::default(),
                stats: SessionStats::default(),
                status: Status::Review,
                title: None,
                transcript: None,
                updated_at: 0,
                transient_messages: TransientMessageStore::default(),
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

    /// Overrides the captured transcript using already formatted text.
    pub(crate) fn transcript(mut self, transcript: impl Into<String>) -> Self {
        self.session.transcript = Some(assistant_transcript(transcript.into()));

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

    /// Overrides the session's orchestration role.
    pub(crate) fn role(mut self, role: SessionRole) -> Self {
        self.session.role = role;

        self
    }

    /// Overrides the lifecycle status.
    pub(crate) fn status(mut self, status: Status) -> Self {
        self.session.status = status;

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

/// Builds a typed transcript containing one assistant answer.
pub(crate) fn assistant_transcript(content: impl AsRef<str>) -> SessionTranscript {
    SessionTranscript::new(vec![SessionMessage::conversation(
        0,
        SessionMessageKind::AssistantAnswer,
        content.as_ref(),
    )])
}

/// Builds a minimal session fixture with the given identifier and status.
pub(crate) fn session_fixture(session_id: &str, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .status(status)
        .folder(PathBuf::from("/tmp/test"))
        .build()
}

/// Builds a session fixture whose title matches its identifier.
pub(crate) fn titled_session_fixture(session_id: &str, status: Status) -> Session {
    SessionFixtureBuilder::new()
        .id(session_id)
        .status(status)
        .title(Some(session_id.to_string()))
        .build()
}

/// Builds a review-state session fixture rooted at the given folder.
pub(crate) fn session_fixture_with_folder(session_folder: PathBuf) -> Session {
    SessionFixtureBuilder::new()
        .id("session-1")
        .folder(session_folder)
        .project_name("test-project")
        .prompt("test prompt")
        .build()
}

/// Returns a mock app-server client wrapped in `Arc` for test injection.
pub(crate) fn mock_app_server() -> Arc<dyn AppServerClient> {
    Arc::new(MockAppServerClient::new())
}

/// Builds one client bundle with a caller-provided agent availability
/// snapshot.
pub(crate) fn test_app_clients_with_available_agent_kinds(
    available_agent_kinds: Vec<AgentKind>,
) -> app::test_support::AppClients {
    let mut project_discovery_client = MockProjectDiscoveryClient::new();
    project_discovery_client
        .expect_discover_home_project_paths()
        .times(0..)
        .returning(|_, _| Box::pin(async { Ok(Vec::new()) }));

    app::test_support::AppClients::new()
        .with_background_tasks_disabled()
        .with_agent_availability_probe(Arc::new(StaticAgentAvailabilityProbe {
            available_agent_kinds,
        }))
        .with_project_discovery_client(Arc::new(project_discovery_client))
}

/// Builds one client bundle with deterministic agent availability for test
/// app startup.
pub(crate) fn test_app_clients() -> app::test_support::AppClients {
    test_app_clients_with_available_agent_kinds(AgentKind::ALL.to_vec())
}

/// Builds one client bundle with deterministic agent availability and a mock
/// app-server override.
pub(crate) fn test_app_clients_with_mock_app_server() -> app::test_support::AppClients {
    test_app_clients().with_app_server_client_override(mock_app_server())
}

/// Builds one app rooted at a retained temporary directory using the given
/// clients.
pub(crate) async fn new_test_app_with_clients(
    clients: app::test_support::AppClients,
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
pub(crate) async fn new_test_app() -> (App, tempfile::TempDir) {
    new_test_app_with_clients(test_app_clients()).await
}

/// Builds one app rooted at a retained temporary directory with a mocked tmux
/// boundary and app-server override.
pub(crate) async fn new_test_app_with_mock_tmux_client() -> (App, tempfile::TempDir) {
    new_test_app_with_tmux_client(Arc::new(crate::infra::tmux::MockTmuxClient::new())).await
}

/// Builds one app rooted at a retained temporary directory with an injected
/// tmux boundary and app-server override.
pub(crate) async fn new_test_app_with_tmux_client(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> (App, tempfile::TempDir) {
    let clients = test_app_clients_with_mock_app_server().with_tmux_client(tmux_client);

    new_test_app_with_clients(clients).await
}

/// Builds one app with an injected tmux boundary, then intentionally drops
/// the temporary directory guard before returning.
pub(crate) async fn new_test_app_with_tmux_client_without_retained_base_dir(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> App {
    let (app, _base_dir) = new_test_app_with_tmux_client(tmux_client).await;

    app
}

/// Builds one app and intentionally drops the temporary directory guard before
/// returning, matching tests that only need in-memory state.
pub(crate) async fn new_test_app_without_retained_base_dir() -> App {
    let (app, _base_dir) = new_test_app().await;

    app
}

/// Initializes a minimal git repository for retained-tempdir app fixtures.
///
/// Every git invocation is checked for success because a silently failing
/// setup command leaves a commit-less repository behind. Host git settings
/// such as `commit.gpgsign`, `core.hooksPath`, or `init.templateDir` can break
/// the initial commit, and an unchecked failure only resurfaces much later as
/// an opaque session-creation panic inside an unrelated test.
pub(crate) fn setup_test_git_repo(path: &Path) {
    run_fixture_git_command(path, &["init"]);
    run_fixture_git_command(path, &["config", "user.name", "Test"]);
    run_fixture_git_command(path, &["config", "user.email", "test@test.com"]);

    std::fs::write(path.join("README.md"), "test").expect("write failed");

    run_fixture_git_command(path, &["add", "."]);
    run_fixture_git_command(path, &["commit", "-m", "Initial commit"]);
    run_fixture_git_command(path, &["branch", "-M", "main"]);
}

/// Runs one git command inside a fixture repository and panics with the
/// captured stderr when the command cannot spawn or exits non-zero.
///
/// Both panic messages are formatted before they are needed so the success
/// path executes every line in this helper.
fn run_fixture_git_command(path: &Path, args: &[&str]) {
    let command_label = format!("git {}", args.join(" "));
    let spawn_failure = format!("failed to run `{command_label}`");
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect(&spawn_failure);
    let exit_failure = format!(
        "`{command_label}` failed with {}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );

    assert!(output.status.success(), "{exit_failure}");
}

/// Builds one git-backed app rooted at a retained temporary directory using
/// the given clients.
pub(crate) async fn new_git_test_app_with_clients(
    clients: app::test_support::AppClients,
) -> (App, tempfile::TempDir) {
    let (app, base_dir, _pool) = new_git_test_app_with_clients_and_pool(clients).await;

    (app, base_dir)
}

/// Builds one git-backed app and exposes its shared database pool for tests
/// that need to inject a persistence failure after app construction.
pub(crate) async fn new_git_test_app_with_pool() -> (App, tempfile::TempDir, sqlx::SqlitePool) {
    new_git_test_app_with_clients_and_pool(test_app_clients()).await
}

/// Builds one git-backed app plus its shared database pool using the given
/// clients.
async fn new_git_test_app_with_clients_and_pool(
    clients: app::test_support::AppClients,
) -> (App, tempfile::TempDir, sqlx::SqlitePool) {
    let base_dir = tempfile::tempdir().expect("failed to create temp dir");
    let base_path = base_dir.path().to_path_buf();
    setup_test_git_repo(base_dir.path());
    let database = Database::open_in_memory()
        .await
        .expect("failed to open in-memory db");
    let pool = database.pool().clone();
    let app = App::new_with_clients(
        base_path.clone(),
        base_path,
        Some("main".to_string()),
        database,
        clients,
    )
    .await
    .expect("failed to build app");

    (app, base_dir, pool)
}

/// Drives foreground reducers until all tracked workspace setup has completed.
pub(crate) async fn finish_session_creation_tasks(app: &mut App) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        while !app.pending_session_creations.is_empty() {
            let event = app
                .next_app_event()
                .await
                .expect("workspace completion event");
            app.apply_app_events(event).await;
        }
    })
    .await
    .expect("workspace setup should complete");
}

/// Builds one git-backed app rooted at a retained temporary directory.
pub(crate) async fn new_git_test_app() -> (App, tempfile::TempDir) {
    new_git_test_app_with_clients(test_app_clients()).await
}

/// Builds one git-backed app rooted at a retained temporary directory with a
/// mocked tmux boundary and app-server override.
pub(crate) async fn new_git_test_app_with_mock_tmux_client() -> (App, tempfile::TempDir) {
    new_git_test_app_with_tmux_client(Arc::new(crate::infra::tmux::MockTmuxClient::new())).await
}

/// Builds one git-backed app rooted at a retained temporary directory with an
/// injected tmux boundary and app-server override.
pub(crate) async fn new_git_test_app_with_tmux_client(
    tmux_client: Arc<dyn crate::infra::tmux::TmuxClient>,
) -> (App, tempfile::TempDir) {
    let clients = test_app_clients_with_mock_app_server().with_tmux_client(tmux_client);

    new_git_test_app_with_clients(clients).await
}

/// Builds a session manager fixture with the provided sessions and handles.
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
            SelectionState::default(),
            Arc::new(FixedClock::unix_epoch()),
            0,
            0,
        ),
        Vec::new(),
    )
}

/// Builds a session manager fixture with the provided sessions and no runtime
/// handles.
pub(crate) fn session_manager_with_sessions(sessions: Vec<Session>) -> SessionManager {
    session_manager_with_handles(sessions, std::collections::HashMap::new())
}

/// Sets a session status in both the session snapshot and its live handles,
/// when either exists.
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

/// Returns the first rendered cell for a contiguous text match in a test
/// buffer.
pub(crate) fn rendered_text_start_cell<'a>(buffer: &'a Buffer, needle: &str) -> Option<&'a Cell> {
    rendered_text_start_cells(buffer, needle).into_iter().next()
}

/// Returns rendered start cells for every contiguous text match in a test
/// buffer.
pub(crate) fn rendered_text_start_cells<'a>(buffer: &'a Buffer, needle: &str) -> Vec<&'a Cell> {
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
#[path = "test_support_assertion_test.rs"]
mod tests;
