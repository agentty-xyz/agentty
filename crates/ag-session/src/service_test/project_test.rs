//! Session service tests for the project-addressed API and its defaults for
//! hosts that serve only their active project.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::support::FakeBackend;
use crate::error::SessionError;
use crate::model::{
    ReviewRequest, Session, SessionId, SessionListEntry, SessionStatus, SessionSummary,
};
use crate::service::{
    AnswerQuestionsRequest, CreateSessionRequest, DurableMessageRequest, HostContext,
    HostProjectStatus, ProjectScope, ProjectSessions, SessionBackend, SessionFileEntry,
    SessionOperation, SessionService,
};
use crate::test_support as model_fixture;

/// Refusal a host that serves only its active project gives another scope.
const ACTIVE_ONLY: &str = "Host serves only its active project";

#[test]
fn project_scope_defaults_to_the_active_project() {
    // Act
    let scope = ProjectScope::default();

    // Assert
    assert_eq!(scope, ProjectScope::Active);
}

#[tokio::test]
async fn existing_hosts_report_unsupported_project_listing() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let supported = service.supports(SessionOperation::Projects);
    let projects = service.list_projects().await;

    // Assert
    assert!(!supported);
    assert_eq!(
        projects,
        Err(SessionError::Operation(
            "Host does not list its projects".to_string()
        ))
    );
}

#[tokio::test]
async fn active_only_hosts_refuse_other_projects_before_reaching_them() {
    // Arrange
    let backend = Arc::new(ActiveProjectBackend::serving_status());
    let service = SessionService::new(backend.clone());
    let scope = ProjectScope::Project(3);

    // Act
    let host = service.host_context_for(scope).await;
    let sessions = service.project_sessions(scope).await;
    let files = service.list_project_files_for(scope).await;
    let synced = service.sync_project_for(scope).await;

    // Assert
    let refusal = SessionError::Operation(ACTIVE_ONLY.to_string());
    assert_eq!(host, Err(refusal.clone()));
    assert_eq!(sessions, Err(refusal.clone()));
    assert_eq!(files, Err(refusal.clone()));
    assert_eq!(synced, Err(refusal));
    assert_eq!(backend.calls(), Vec::<&str>::new());
}

#[tokio::test]
async fn active_only_hosts_answer_the_active_scope_with_their_project_methods() {
    // Arrange
    let backend = Arc::new(ActiveProjectBackend::serving_status());
    let service = SessionService::new(backend.clone());

    // Act
    let host = service.host_context_for(ProjectScope::Active).await;
    let files = service.list_project_files_for(ProjectScope::Active).await;
    let synced = service.sync_project_for(ProjectScope::Active).await;

    // Assert
    assert_eq!(host, Ok(host_context_fixture()));
    assert_eq!(files, Ok(project_files_fixture()));
    assert_eq!(synced, Ok(()));
    assert_eq!(
        backend.calls(),
        ["host_context", "list_project_files", "sync_project"]
    );
}

#[tokio::test]
async fn active_project_sessions_carry_the_status_a_host_reports() {
    // Arrange
    let backend = Arc::new(ActiveProjectBackend::serving_status());
    let service = SessionService::new(backend.clone());

    // Act
    let sessions = service.project_sessions(ProjectScope::Active).await;

    // Assert
    assert_eq!(
        sessions,
        Ok(ProjectSessions {
            entries: vec![SessionListEntry::from(session_summary("session-1"))],
            project_id: None,
            status: Some(project_status_fixture()),
        })
    );
    assert_eq!(backend.calls(), ["list_sessions", "project_status"]);
}

#[tokio::test]
async fn active_project_sessions_skip_the_status_a_host_does_not_report() {
    // Arrange
    let backend = Arc::new(ActiveProjectBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let sessions = service.project_sessions(ProjectScope::Active).await;

    // Assert
    assert_eq!(
        sessions,
        Ok(ProjectSessions {
            entries: vec![SessionListEntry::from(session_summary("session-1"))],
            project_id: None,
            status: None,
        })
    );
    assert_eq!(backend.calls(), ["list_sessions"]);
}

#[tokio::test]
async fn active_project_sessions_report_the_listing_error() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let sessions = service.project_sessions(ProjectScope::Active).await;

    // Assert
    assert_eq!(
        sessions,
        Err(SessionError::Operation(
            "Host does not support session listing".to_string()
        ))
    );
}

#[tokio::test]
async fn sessions_are_listed_only_while_the_session_list_contains_them() {
    // Arrange
    let backend = Arc::new(ActiveProjectBackend::default());
    let service = SessionService::new(backend.clone());

    // Act
    let listed = service
        .is_session_listed(&SessionId::from("session-1"))
        .await;
    let unlisted = service
        .is_session_listed(&SessionId::from("session-2"))
        .await;

    // Assert
    assert_eq!(listed, Ok(true));
    assert_eq!(unlisted, Ok(false));
    assert_eq!(backend.calls(), ["list_sessions", "list_sessions"]);
}

#[tokio::test]
async fn session_listing_checks_report_the_listing_error() {
    // Arrange
    let service = SessionService::new(Arc::new(FakeBackend::default()));

    // Act
    let listed = service
        .is_session_listed(&SessionId::from("session-1"))
        .await;

    // Assert
    assert_eq!(
        listed,
        Err(SessionError::Operation(
            "Host does not support session listing".to_string()
        ))
    );
}

/// Host that serves only its active project, lists `session-1`, and records
/// each project method it is asked for.
#[derive(Default)]
struct ActiveProjectBackend {
    /// Project methods asked for, in order.
    calls: Mutex<Vec<&'static str>>,
    inner: FakeBackend,
    /// Whether the host reports its project status.
    is_status_served: bool,
}

impl ActiveProjectBackend {
    /// Builds a host that also reports its project status.
    fn serving_status() -> Self {
        Self {
            is_status_served: true,
            ..Self::default()
        }
    }

    /// Returns the project methods asked for, in order.
    fn calls(&self) -> Vec<&'static str> {
        self.calls.lock().expect("calls").clone()
    }

    /// Records one project method call.
    fn record(&self, call: &'static str) {
        self.calls.lock().expect("calls").push(call);
    }
}

#[async_trait]
impl SessionBackend for ActiveProjectBackend {
    fn supports(&self, operation: SessionOperation) -> bool {
        self.is_status_served && operation == SessionOperation::ProjectStatus
    }

    async fn host_context(&self) -> Result<HostContext, SessionError> {
        self.record("host_context");

        Ok(host_context_fixture())
    }

    async fn project_status(&self) -> Result<HostProjectStatus, SessionError> {
        self.record("project_status");

        Ok(project_status_fixture())
    }

    async fn sync_project(&self) -> Result<(), SessionError> {
        self.record("sync_project");

        Ok(())
    }

    async fn list_project_files(&self) -> Result<Vec<SessionFileEntry>, SessionError> {
        self.record("list_project_files");

        Ok(project_files_fixture())
    }

    async fn list_sessions(&self) -> Result<Vec<SessionSummary>, SessionError> {
        self.record("list_sessions");

        Ok(vec![session_summary("session-1")])
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.inner.create_session(request).await
    }

    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError> {
        self.inner.get_session(session_id).await
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError> {
        self.inner.send_message(session_id, message).await
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), SessionError> {
        self.inner.submit_durable_message(session_id, request).await
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        self.inner.answer_questions(session_id, request).await
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.inner.cancel_session(session_id).await
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.inner.merge_session(session_id).await
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.inner.create_review_request(session_id).await
    }
}

/// Describes project `7` on `main`, which serves no creation modes.
fn host_context_fixture() -> HostContext {
    HostContext {
        base_branch: "main".to_string(),
        default_model: model_fixture::CODEX_MODEL,
        models: vec![model_fixture::codex_selection()],
        project_id: 7,
        project_name: "agentty".to_string(),
        session_modes: Vec::new(),
    }
}

/// One directory and one file in the active project's checkout.
fn project_files_fixture() -> Vec<SessionFileEntry> {
    vec![
        SessionFileEntry {
            is_dir: true,
            path: "src".to_string(),
        },
        SessionFileEntry {
            is_dir: false,
            path: "README.md".to_string(),
        },
    ]
}

/// The active project on `main`, level with its upstream and not syncing.
fn project_status_fixture() -> HostProjectStatus {
    HostProjectStatus {
        ahead_behind: Some((0, 0)),
        branch: Some("main".to_string()),
        project_name: "agentty".to_string(),
        sync: None,
    }
}

/// A session in review, titled after its identifier.
fn session_summary(session_id: &str) -> SessionSummary {
    SessionSummary {
        id: SessionId::from(session_id),
        status: SessionStatus::Review,
        title: Some(session_id.to_string()),
        updated_at: 20,
    }
}
