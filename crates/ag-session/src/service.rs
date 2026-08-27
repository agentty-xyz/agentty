//! Programmatic session orchestration facade and host backend port.

use std::sync::Arc;

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
    /// Creates a controller session that plans and supervises worker sessions.
    Orchestrator,
    /// Creates one worker owned by a persisted orchestration task.
    OrchestrationChild {
        /// Durable task row used to re-link the child after restart.
        task_id: i64,
    },
    /// Creates one temporary read-only researcher owned by an orchestration
    /// task.
    OrchestrationResearch {
        /// Durable task row used to re-link the child after restart.
        task_id: i64,
    },
    /// Creates a draft stacked on an existing parent session.
    Stacked {
        /// Review-ready parent session whose branch becomes the stack base.
        parent_session_id: SessionId,
    },
}

/// Explicit input for creating one session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateSessionRequest {
    /// Existing session whose launch settings should be copied.
    ///
    /// When absent, the host resolves the owning project's current defaults.
    pub inherit_from_session_id: Option<SessionId>,
    /// Determines whether the session is regular, deferred, or stacked.
    pub mode: CreateSessionMode,
    /// Project that owns the new session.
    pub project_id: i64,
}

/// One structured response to a persisted clarification question.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QuestionAnswer {
    /// User response paired with `question`.
    pub answer: String,
    /// Exact persisted question text being answered.
    pub question: String,
}

/// Structured input for resuming one session from clarification questions.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AnswerQuestionsRequest {
    /// Ordered question and answer pairs for the current question set.
    pub answers: Vec<QuestionAnswer>,
}

/// Durable coordinator-owned turn submitted to one controller session.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CoordinatorMessageRequest {
    /// Agent-facing roll-up or supervision prompt.
    pub message: String,
    /// Stable operation identifier reused when delivery is retried.
    pub operation_id: String,
    /// Whether the machine-authored prompt is shown in the human transcript.
    pub visibility: CoordinatorMessageVisibility,
}

/// Transcript treatment for one coordinator-owned prompt.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum CoordinatorMessageVisibility {
    /// Deliver agent context without duplicating board data in chat.
    #[default]
    Hidden,
    /// Preserve a worker continuation instruction in its inspectable history.
    Visible,
}

/// Host implementation boundary for session persistence and workflows.
///
/// The trait is object-safe so agent loops and future orchestrators can hold a
/// programmatic session capability without depending on a concrete frontend.
#[async_trait]
pub trait SessionBackend: Send + Sync {
    /// Creates one session and returns its stable identifier.
    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, SessionError>;

    /// Loads one complete session aggregate, including settings and messages.
    async fn get_session(&self, session_id: &SessionId) -> Result<Option<Session>, SessionError>;

    /// Sends one text message, starting, resuming, or queueing as appropriate.
    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), SessionError>;

    /// Submits a coordinator-owned turn without entering the lossy live-chat
    /// queue used while an ordinary user turn is active.
    async fn submit_coordinator_message(
        &self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), SessionError>;

    /// Answers the complete current clarification-question set.
    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError>;

    /// Cancels one session through the host lifecycle workflow.
    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Requests merge processing for one review-ready session.
    async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError>;

    /// Queues publication of one session branch and creates or refreshes its
    /// review request.
    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError>;
}

/// Stable programmatic facade for session lifecycle operations.
#[derive(Clone)]
pub struct SessionService {
    backend: Arc<dyn SessionBackend>,
}

impl SessionService {
    /// Creates an owned session capability backed by a shared host handle.
    pub fn new(backend: Arc<dyn SessionBackend>) -> Self {
        Self { backend }
    }

    /// Creates one session and returns its stable identifier.
    ///
    /// # Errors
    /// Returns an error when the host cannot create the requested session.
    pub async fn create_session(
        &self,
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
        &self,
        session_id: &SessionId,
        message: impl Into<String> + Send,
    ) -> Result<(), SessionError> {
        self.backend.send_message(session_id, message.into()).await
    }

    /// Submits one coordinator-owned turn directly to the serialized worker.
    ///
    /// # Errors
    /// Returns an error when the session is busy or cannot accept the turn.
    pub async fn submit_coordinator_message(
        &self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), SessionError> {
        self.backend
            .submit_coordinator_message(session_id, request)
            .await
    }

    /// Answers the complete current clarification-question set.
    ///
    /// # Errors
    /// Returns an error when the answers are stale, incomplete, or cannot be
    /// enqueued as a follow-up turn.
    pub async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), SessionError> {
        self.backend.answer_questions(session_id, request).await
    }

    /// Cancels one session through its host lifecycle workflow.
    ///
    /// # Errors
    /// Returns an error when the session does not exist or cannot be canceled
    /// in its current state.
    pub async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.cancel_session(session_id).await
    }

    /// Requests merge processing for one session.
    ///
    /// # Errors
    /// Returns an error when the session is not mergeable or queueing fails.
    pub async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
        self.backend.merge_session(session_id).await
    }

    /// Queues publication of one session and creates or refreshes its review
    /// request.
    ///
    /// # Errors
    /// Returns an error when queueing, branch publication, forge access, or
    /// persistence fails.
    pub async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, SessionError> {
        self.backend.create_review_request(session_id).await
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Mutex;

    use ag_agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, SpeedMode};
    use ag_forge::{ForgeKind, ReviewRequestState, ReviewRequestSummary};

    use super::*;
    use crate::{
        PermissionMode, SessionMessage, SessionMessageKind, SessionRole, SessionSettings,
        SessionStatus,
    };

    #[derive(Default)]
    struct FakeBackend {
        state: Mutex<FakeBackendState>,
    }

    impl FakeBackend {
        fn from_state(state: FakeBackendState) -> Self {
            Self {
                state: Mutex::new(state),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.state
                .lock()
                .map(|state| state.calls.clone())
                .unwrap_or_default()
        }
    }

    #[derive(Default)]
    struct FakeBackendState {
        calls: Vec<String>,
        create_results: VecDeque<Result<SessionId, SessionError>>,
        get_result: Option<Result<Option<Session>, SessionError>>,
        review_result: Option<Result<ReviewRequest, SessionError>>,
        unit_results: VecDeque<Result<(), SessionError>>,
    }

    #[async_trait]
    impl SessionBackend for FakeBackend {
        async fn create_session(
            &self,
            request: CreateSessionRequest,
        ) -> Result<SessionId, SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state.calls.push(format!("create:{:?}", request.mode));

            state
                .create_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn get_session(
            &self,
            _session_id: &SessionId,
        ) -> Result<Option<Session>, SessionError> {
            self.state
                .lock()
                .expect("fake backend state should remain available")
                .get_result
                .clone()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn send_message(
            &self,
            session_id: &SessionId,
            message: String,
        ) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state.calls.push(format!("send:{session_id}:{message}"));

            state
                .unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn submit_coordinator_message(
            &self,
            session_id: &SessionId,
            request: CoordinatorMessageRequest,
        ) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state.calls.push(format!(
                "submit-coordinator:{session_id}:{}:{}",
                request.operation_id, request.message
            ));

            state
                .unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn answer_questions(
            &self,
            session_id: &SessionId,
            request: AnswerQuestionsRequest,
        ) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state
                .calls
                .push(format!("answer:{session_id}:{}", request.answers.len()));

            state
                .unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn cancel_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state.calls.push(format!("cancel:{session_id}"));

            state
                .unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn merge_session(&self, session_id: &SessionId) -> Result<(), SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state.calls.push(format!("merge:{session_id}"));

            state
                .unit_results
                .pop_front()
                .unwrap_or_else(|| Err(SessionError::Operation("missing result".to_string())))
        }

        async fn create_review_request(
            &self,
            session_id: &SessionId,
        ) -> Result<ReviewRequest, SessionError> {
            let mut state = self
                .state
                .lock()
                .expect("fake backend state should remain available");
            state.calls.push(format!("review:{session_id}"));

            state
                .review_result
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
                permission_mode: PermissionMode::AutoEdit,
                personality_id: Some("reviewer".to_string()),
                project_id: 7,
                reasoning_level: ReasoningLevel::High,
                role: SessionRole::Worker,
                speed_mode: SpeedMode::Normal,
            },
            status: SessionStatus::Review,
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
        let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
            create_results: VecDeque::from([Ok(SessionId::from("session-1"))]),
            get_result: Some(Ok(Some(expected_session.clone()))),
            ..FakeBackendState::default()
        }));
        let service = SessionService::new(backend.clone());

        // Act
        let session_id = service
            .create_session(CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id: 7,
            })
            .await
            .expect("session should be created");
        let loaded_session = service
            .get_session(&session_id)
            .await
            .expect("session should load");

        // Assert
        assert_eq!(loaded_session, Some(expected_session));
        assert_eq!(backend.calls(), ["create:Regular"]);
    }

    #[tokio::test]
    async fn service_delegates_mutating_operations_through_clones() {
        // Arrange
        let expected_review_request = review_request_fixture();
        let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
            review_result: Some(Ok(expected_review_request.clone())),
            unit_results: VecDeque::from([Ok(()), Ok(()), Ok(()), Ok(()), Ok(())]),
            ..FakeBackendState::default()
        }));
        let session_id = SessionId::from("session-1");
        let service = SessionService::new(backend.clone());
        let cloned_service = service.clone();
        let answers = AnswerQuestionsRequest {
            answers: vec![QuestionAnswer {
                answer: "main".to_string(),
                question: "Which branch?".to_string(),
            }],
        };

        // Act
        service
            .send_message(&session_id, "continue")
            .await
            .expect("message should be sent");
        service
            .submit_coordinator_message(
                &session_id,
                CoordinatorMessageRequest {
                    message: "roll up".to_string(),
                    operation_id: "rollup-7".to_string(),
                    visibility: CoordinatorMessageVisibility::Hidden,
                },
            )
            .await
            .expect("coordinator message should be submitted");
        cloned_service
            .answer_questions(&session_id, answers)
            .await
            .expect("questions should be answered");
        service
            .cancel_session(&session_id)
            .await
            .expect("cancel should be requested");
        cloned_service
            .merge_session(&session_id)
            .await
            .expect("merge should be requested");
        let review_request = service
            .create_review_request(&session_id)
            .await
            .expect("review request should be created");

        // Assert
        assert_eq!(review_request, expected_review_request);
        assert_eq!(
            backend.calls(),
            [
                "send:session-1:continue",
                "submit-coordinator:session-1:rollup-7:roll up",
                "answer:session-1:1",
                "cancel:session-1",
                "merge:session-1",
                "review:session-1"
            ]
        );
    }

    #[tokio::test]
    async fn service_preserves_backend_errors() {
        // Arrange
        let expected_error = SessionError::Operation("cannot create".to_string());
        let backend = Arc::new(FakeBackend::from_state(FakeBackendState {
            create_results: VecDeque::from([Err(expected_error.clone())]),
            ..FakeBackendState::default()
        }));
        let service = SessionService::new(backend);

        // Act
        let error = service
            .create_session(CreateSessionRequest {
                inherit_from_session_id: None,
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
        let backend = Arc::new(FakeBackend::default());
        let session_id = SessionId::from("session-1");
        let service = SessionService::new(backend);

        // Act
        let create_error = service
            .create_session(CreateSessionRequest {
                inherit_from_session_id: None,
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
        let coordinator_error = service
            .submit_coordinator_message(
                &session_id,
                CoordinatorMessageRequest {
                    message: "roll up".to_string(),
                    operation_id: "rollup-1".to_string(),
                    visibility: CoordinatorMessageVisibility::Hidden,
                },
            )
            .await
            .expect_err("coordinator submission should require a result");
        let answer_error = service
            .answer_questions(
                &session_id,
                AnswerQuestionsRequest {
                    answers: Vec::new(),
                },
            )
            .await
            .expect_err("answers should require a result");
        let cancel_error = service
            .cancel_session(&session_id)
            .await
            .expect_err("cancel should require a result");
        let merge_error = service
            .merge_session(&session_id)
            .await
            .expect_err("merge should require a result");
        let review_error = service
            .create_review_request(&session_id)
            .await
            .expect_err("review should require a result");
        let errors = [
            create_error,
            get_error,
            send_error,
            coordinator_error,
            answer_error,
            cancel_error,
            merge_error,
            review_error,
        ];

        // Assert
        assert!(
            errors
                .into_iter()
                .all(|error| error == SessionError::Operation("missing result".to_string()))
        );
    }
}
