//! Programmatic session orchestration facade and host backend port.

use async_trait::async_trait;

use crate::{ReviewRequest, Session, SessionError, SessionId};

/// Creation strategy for a new session.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub enum CreateSessionMode {
    /// Creates a regular root session with an eagerly materialized worktree.
    #[default]
    Regular,
    /// Creates a root draft whose worktree is materialized on first send.
    Draft,
    /// Creates a one-level draft stacked on an existing parent session.
    Stacked {
        /// Review-ready parent session whose branch becomes the stack base.
        parent_session_id: SessionId,
    },
}

/// Explicit input for creating one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSessionRequest {
    /// Determines whether the session is regular, deferred, or stacked.
    pub mode: CreateSessionMode,
    /// Project that owns the new session.
    pub project_id: i64,
}

/// Host implementation boundary for session persistence and workflows.
///
/// The trait is object-safe so agent loops and future orchestrators can hold a
/// programmatic session capability without depending on a concrete frontend.
#[async_trait]
pub trait SessionBackend: Send {
    /// Creates one session and returns its stable identifier.
    async fn create_session(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError>;

    /// Loads one complete session aggregate, including settings and messages.
    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError>;

    /// Sends one text message, starting, resuming, or queueing as appropriate.
    async fn send_message(
        &mut self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError>;

    /// Requests merge processing for one review-ready session.
    async fn merge_session(&mut self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Publishes one session branch and creates or refreshes its review
    /// request.
    async fn create_review_request(
        &mut self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError>;
}

/// Stable programmatic facade for session lifecycle operations.
pub struct SessionService<'backend> {
    backend: &'backend mut dyn SessionBackend,
}

impl<'backend> SessionService<'backend> {
    /// Binds the stable session API to one host backend.
    pub fn new(backend: &'backend mut dyn SessionBackend) -> Self {
        Self { backend }
    }

    /// Creates one session and returns its stable identifier.
    ///
    /// # Errors
    /// Returns an error when the host cannot create the requested session.
    pub async fn create_session(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError> {
        self.backend.create_session(request).await
    }

    /// Loads one complete session aggregate by identifier.
    ///
    /// # Errors
    /// Returns an error when persisted data cannot be loaded or decoded.
    pub async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<Session>, SessionError> {
        self.backend.get_session(session_id).await
    }

    /// Sends a text message to one session.
    ///
    /// # Errors
    /// Returns an error when the session cannot accept or enqueue the message.
    pub async fn send_message(
        &mut self,
        session_id: &SessionId,
        message: impl Into<String> + Send,
    ) -> Result<(), SessionError> {
        self.backend.send_message(session_id, message.into()).await
    }

    /// Requests merge processing for one session.
    ///
    /// # Errors
    /// Returns an error when the session is not mergeable or queueing fails.
    pub async fn merge_session(&mut self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.merge_session(session_id).await
    }

    /// Publishes one session and creates or refreshes its review request.
    ///
    /// # Errors
    /// Returns an error when branch publication, forge access, or persistence
    /// fails.
    pub async fn create_review_request(
        &mut self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.backend.create_review_request(session_id).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use ag_agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel};
    use ag_forge::{ForgeKind, ReviewRequestState, ReviewRequestSummary};

    use super::*;
    use crate::{SessionMessage, SessionMessageKind, SessionSettings, SessionStatus};

    #[derive(Default)]
    struct FakeBackend {
        calls: Vec<String>,
        create_results: VecDeque<Result<SessionId, SessionError>>,
        get_result: Option<Result<Option<Session>, SessionError>>,
        review_result: Option<Result<ReviewRequest, SessionError>>,
        unit_results: VecDeque<Result<(), SessionError>>,
    }

    #[async_trait]
    impl SessionBackend for FakeBackend {
        async fn create_session(
            &mut self,
            request: CreateSessionRequest,
        ) -> Result<SessionId, SessionError> {
            self.calls.push(format!("create:{:?}", request.mode));

            self.create_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn get_session(
            &self,
            _session_id: &SessionId,
        ) -> Result<Option<Session>, SessionError> {
            self.get_result
                .clone()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn send_message(
            &mut self,
            session_id: &SessionId,
            message: String,
        ) -> Result<(), SessionError> {
            self.calls.push(format!("send:{session_id}:{message}"));

            self.unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn merge_session(&mut self, session_id: &SessionId) -> Result<(), SessionError> {
            self.calls.push(format!("merge:{session_id}"));

            self.unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn create_review_request(
            &mut self,
            session_id: &SessionId,
        ) -> Result<ReviewRequest, SessionError> {
            self.calls.push(format!("review:{session_id}"));

            self.review_result
                .clone()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }
    }

    fn session_fixture() -> Session {
        Session {
            created_at: 10,
            draft_prompt: None,
            id: SessionId::from("session-1"),
            messages: vec![SessionMessage::new(
                0,
                SessionMessageKind::UserPrompt,
                "build it",
            )],
            published_upstream_ref: None,
            questions: Vec::new(),
            queued_messages: Vec::new(),
            review_request: None,
            settings: SessionSettings {
                agent: AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
                base_branch: "main".to_string(),
                is_draft: false,
                parent_session_id: None,
                personality_id: Some("reviewer".to_string()),
                project_id: 7,
                reasoning_level: ReasoningLevel::High,
            },
            status: SessionStatus::Review,
            summary: Some("Implemented it".to_string()),
            title: Some("Build it".to_string()),
            updated_at: 20,
        }
    }

    fn review_request_fixture() -> ReviewRequest {
        ReviewRequest {
            last_refreshed_at: 30,
            summary: ReviewRequestSummary {
                display_id: "#42".to_string(),
                forge_kind: ForgeKind::GitHub,
                source_branch: "wt/session-1".to_string(),
                state: ReviewRequestState::Open,
                status_summary: None,
                target_branch: "main".to_string(),
                title: "Build it".to_string(),
                web_url: "https://example.test/pull/42".to_string(),
            },
        }
    }

    #[tokio::test]
    async fn service_delegates_create_and_get() {
        // Arrange
        let expected_session = session_fixture();
        let mut backend = FakeBackend {
            create_results: VecDeque::from([Ok(SessionId::from("session-1"))]),
            get_result: Some(Ok(Some(expected_session.clone()))),
            ..FakeBackend::default()
        };
        // Act
        let loaded_session = {
            let mut service = SessionService::new(&mut backend);
            let session_id = service
                .create_session(CreateSessionRequest {
                    mode: CreateSessionMode::Regular,
                    project_id: 7,
                })
                .await
                .expect("session should be created");

            service
                .get_session(&session_id)
                .await
                .expect("session should load")
        };

        // Assert
        assert_eq!(loaded_session, Some(expected_session));
        assert_eq!(backend.calls, ["create:Regular"]);
    }

    #[tokio::test]
    async fn service_delegates_send_merge_and_review() {
        // Arrange
        let expected_review_request = review_request_fixture();
        let mut backend = FakeBackend {
            review_result: Some(Ok(expected_review_request.clone())),
            unit_results: VecDeque::from([Ok(()), Ok(())]),
            ..FakeBackend::default()
        };
        let session_id = SessionId::from("session-1");
        // Act
        let review_request = {
            let mut service = SessionService::new(&mut backend);
            service
                .send_message(&session_id, "continue")
                .await
                .expect("message should be sent");
            service
                .merge_session(&session_id)
                .await
                .expect("merge should be requested");

            service
                .create_review_request(&session_id)
                .await
                .expect("review request should be created")
        };

        // Assert
        assert_eq!(review_request, expected_review_request);
        assert_eq!(
            backend.calls,
            [
                "send:session-1:continue",
                "merge:session-1",
                "review:session-1"
            ]
        );
    }

    #[tokio::test]
    async fn service_preserves_backend_errors() {
        // Arrange
        let expected_error = SessionError::Operation("cannot create".to_string());
        let mut backend = FakeBackend {
            create_results: VecDeque::from([Err(expected_error.clone())]),
            ..FakeBackend::default()
        };
        let mut service = SessionService::new(&mut backend);

        // Act
        let error = service
            .create_session(CreateSessionRequest {
                mode: CreateSessionMode::Draft,
                project_id: 7,
            })
            .await
            .expect_err("backend error should be preserved");

        // Assert
        assert_eq!(error, expected_error);
    }

    #[tokio::test]
    async fn fake_backend_requires_explicit_results() {
        // Arrange
        let mut backend = FakeBackend::default();
        let session_id = SessionId::from("session-1");

        // Act
        let errors = {
            let mut service = SessionService::new(&mut backend);
            let create_error = service
                .create_session(CreateSessionRequest {
                    mode: CreateSessionMode::Regular,
                    project_id: 7,
                })
                .await
                .expect_err("create should require a result");
            let get_error = service
                .get_session(&session_id)
                .await
                .expect_err("get should require a result");
            let send_error = service
                .send_message(&session_id, "continue")
                .await
                .expect_err("send should require a result");
            let merge_error = service
                .merge_session(&session_id)
                .await
                .expect_err("merge should require a result");
            let review_error = service
                .create_review_request(&session_id)
                .await
                .expect_err("review should require a result");

            [
                create_error,
                get_error,
                send_error,
                merge_error,
                review_error,
            ]
        };

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| error == SessionError::Operation("missing result".to_string()))
        );
    }
}
