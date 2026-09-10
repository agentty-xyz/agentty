use std::collections::VecDeque;
use std::sync::Mutex;

use ag_agent::{AgentKind, AgentModel, AgentSelection, ReasoningLevel, ResponseStyle, SpeedMode};
use ag_forge::{ForgeKind, ReviewRequestState, ReviewRequestSummary};
use async_trait::async_trait;

use crate::error::SessionError;
use crate::message::{SessionMessage, SessionMessageKind};
use crate::model::{
    PermissionMode, ReviewRequest, Session, SessionId, SessionRole, SessionSettings, SessionStatus,
};
use crate::service::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CreateSessionRequest, SessionBackend,
};

#[derive(Default)]
pub(super) struct FakeBackend {
    state: Mutex<FakeBackendState>,
}

impl FakeBackend {
    pub(super) fn from_state(state: FakeBackendState) -> Self {
        Self {
            state: Mutex::new(state),
        }
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.state
            .lock()
            .map(|state| state.calls.clone())
            .unwrap_or_default()
    }
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

    async fn get_session(&self, _session_id: &SessionId) -> Result<Option<Session>, SessionError> {
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

#[derive(Default)]
pub(super) struct FakeBackendState {
    pub(super) calls: Vec<String>,
    pub(super) create_results: VecDeque<Result<SessionId, SessionError>>,
    pub(super) get_result: Option<Result<Option<Session>, SessionError>>,
    pub(super) review_result: Option<Result<ReviewRequest, SessionError>>,
    pub(super) unit_results: VecDeque<Result<(), SessionError>>,
}

pub(super) fn session_fixture() -> Session {
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
            response_style: ResponseStyle::Balanced,
            role: SessionRole::Worker,
            speed_mode: SpeedMode::Normal,
        },
        status: SessionStatus::Review,
        title: Some("Build it".to_string()),
        updated_at: 20,
    }
}

pub(super) fn review_request_fixture() -> ReviewRequest {
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
