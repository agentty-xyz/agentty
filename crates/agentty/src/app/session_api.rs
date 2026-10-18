//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use std::future::Future;
use std::sync::Arc;

use ag_contracts::{ReasoningLevel, ResponseStyle, SpeedMode};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, QuestionAnswer, ReviewRequest, ReviewRequestState, SessionBackend,
    SessionError as ApiSessionError, SessionId, SessionMessage, SessionMessageKind, SessionService,
    SessionSettings, SessionStatus, parse_persisted_session_agent_model,
};
use async_trait::async_trait;

use crate::app::message_admission::MessagePlan;
use crate::app::session::{SessionCreationSettings, migrate_session_off_retired_model};
use crate::app::session_creation::PreparedSessionCreation;
use crate::app::{
    App, AppError, AppEvent, SessionError, SessionRuntimeCommand, SessionRuntimeHandle,
};
use crate::domain::agent::{AgentKind, AgentSelection};
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{SessionMessageRow, SessionReviewRequestRow, SessionRow};

#[async_trait]
impl SessionBackend for SessionRuntimeHandle {
    async fn create_task(
        &self,
        session_id: &SessionId,
        request: ag_session::CreateTaskRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::create_task(self, session_id, request).await
    }

    async fn list_tasks(
        &self,
        session_id: &SessionId,
    ) -> Result<Vec<ag_session::Task>, ApiSessionError> {
        SessionRuntimeHandle::list_tasks(self, session_id).await
    }

    async fn submit_task(
        &self,
        session_id: &SessionId,
        request: ag_session::SubmitTaskRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_task(self, session_id, request).await
    }

    async fn get_task(&self, task_id: &str) -> Result<Option<ag_session::Task>, ApiSessionError> {
        SessionRuntimeHandle::get_task(self, task_id).await
    }

    async fn complete_task(&self, task_id: &str) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::complete_task(self, task_id).await
    }

    async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        SessionRuntimeHandle::create_session(self, request).await
    }

    async fn get_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        SessionRuntimeHandle::get_session(self, session_id).await
    }

    async fn send_message(
        &self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::send_message(self, session_id, message).await
    }

    async fn submit_durable_message(
        &self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_durable_message(self, session_id, request).await
    }

    async fn answer_questions(
        &self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::answer_questions(self, session_id, request).await
    }

    async fn cancel_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::cancel_session(self, session_id).await
    }

    async fn merge_session(&self, session_id: &SessionId) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::merge_session(self, session_id).await
    }

    async fn create_review_request(
        &self,
        session_id: &SessionId,
    ) -> Result<ReviewRequest, ApiSessionError> {
        SessionRuntimeHandle::create_review_request(self, session_id).await
    }
}

impl App {
    /// Returns a cloneable frontend-neutral session capability.
    pub fn session_service(&self) -> SessionService {
        SessionService::new(Arc::new(self.sessions.handle()))
    }

    /// Uses the same mailbox for terminal actions while retaining provenance.
    pub(crate) fn session_view_service(&self) -> SessionService {
        SessionService::new(Arc::new(
            self.sessions
                .handle()
                .with_initiator(crate::app::command_gateway::CommandInitiator::SessionView),
        ))
    }

    /// Drives one local API request while processing the actor commands ahead
    /// of it.
    ///
    /// Background callers rely on the terminal event loop to drive the same
    /// mailbox. Foreground callers use this helper so awaiting their own
    /// response never deadlocks the foreground executor. Pending creation also
    /// pumps reducer events so its background completion can acknowledge the
    /// request; other commands retain their normal snapshot ordering.
    pub(crate) async fn drive_session_request<RequestFuture>(
        &mut self,
        request: RequestFuture,
    ) -> RequestFuture::Output
    where
        RequestFuture: Future,
    {
        let _session_runtime_consumer = self.sessions.foreground_consumer();
        tokio::pin!(request);

        loop {
            tokio::select! {
                biased;
                result = &mut request => return result,
                event = async {
                    if self.pending_session_creations.is_empty() {
                        crate::app::AppRuntimeEvent::Session(self.sessions.next_command().await)
                    } else {
                        self.next_runtime_event().await
                    }
                } => {
                    match event {
                        crate::app::AppRuntimeEvent::App(event) => {
                            Box::pin(self.apply_app_events(*event)).await;
                        }
                        crate::app::AppRuntimeEvent::Session(command) => {
                            self.apply_session_runtime_command(command).await;
                        }
                    }
                }
            }
        }
    }

    /// Routes mutations to shared admission and keeps reads on the query path.
    pub(crate) async fn apply_session_runtime_command(&mut self, command: SessionRuntimeCommand) {
        match command {
            SessionRuntimeCommand::ListTasks {
                response_tx,
                session_id,
            } => {
                let result = self
                    .services
                    .db()
                    .tasks()
                    .list_tasks(&session_id)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()));
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::GetTask {
                response_tx,
                task_id,
            } => {
                let result = self
                    .services
                    .db()
                    .tasks()
                    .load_task(&task_id)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()));
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::Get {
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.get_api_session(&session_id).await);
            }
            SessionRuntimeCommand::Mutation(command) => {
                let _ = self.admit_session_command(command).await;
            }
        }
    }

    /// Validates one creation request and captures its launch settings. Drafts
    /// persist immediately; materialized worktrees return an owned effect plan.
    pub(super) async fn prepare_api_session_creation(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<PreparedSessionCreation, ApiSessionError> {
        self.validate_api_session_request(&request).await?;
        self.ensure_project_checkout_available(request.project_id)
            .map_err(api_error_from_app)?;

        let inherited_settings = self
            .inherited_creation_settings(
                request.inherit_from_session_id.as_ref(),
                request.project_id,
            )
            .await?;
        let (base_branch_override, creation_settings) = inherited_settings
            .map_or((None, None), |inherited| {
                (Some(inherited.base_branch), Some(inherited.settings))
            });
        let runs_on_harness = request.mode == CreateSessionMode::Harness;
        match request.mode {
            CreateSessionMode::Draft => {
                let project = self.api_project_creation_context(base_branch_override)?;
                let session_id = self
                    .sessions
                    .create_draft_session_for_project_with_settings(
                        &self.services,
                        request.project_id,
                        &project.base_branch,
                        creation_settings,
                    )
                    .await
                    .map_err(api_error_from_session)?;

                return Ok(PreparedSessionCreation::Persisted(session_id));
            }
            CreateSessionMode::Stacked { parent_session_id } => {
                let session_id = if let Some(settings) = creation_settings {
                    self.sessions
                        .create_stacked_draft_session_with_settings(
                            &self.services,
                            &parent_session_id,
                            settings,
                        )
                        .await
                } else {
                    self.sessions
                        .create_stacked_draft_session(&self.services, &parent_session_id)
                        .await
                }
                .map_err(api_error_from_session)?;

                return Ok(PreparedSessionCreation::Persisted(session_id));
            }
            CreateSessionMode::Regular | CreateSessionMode::Harness => {}
        }
        let project = self.api_project_creation_context(base_branch_override)?;
        let settings = self
            .sessions
            .resolve_session_creation_settings(
                &self.services,
                request.project_id,
                creation_settings,
            )
            .await
            .map_err(api_error_from_session)?;
        let harness_model = runs_on_harness
            .then(|| self.services.harness_availability().default_model())
            .flatten();
        let settings = if let Some(harness_model) = harness_model {
            SessionCreationSettings {
                agent: AgentSelection::new(AgentKind::Harness, harness_model),
                speed_mode: SpeedMode::Normal,
                ..settings
            }
        } else {
            settings
        };

        Ok(PreparedSessionCreation::Materialized {
            base_branch: project.base_branch,
            project_id: request.project_id,
            settings,
        })
    }

    async fn validate_api_session_request(
        &self,
        request: &CreateSessionRequest,
    ) -> Result<(), ApiSessionError> {
        if request.project_id != self.active_project_id() {
            return Err(ApiSessionError::Operation(format!(
                "Project `{}` is not active",
                request.project_id
            )));
        }
        if request.mode == CreateSessionMode::Harness
            && !self.services.harness_availability().is_available()
        {
            return Err(ApiSessionError::Operation(
                "Harness sessions need `agentty --experimental-harness` and a configured \
                 provider: `MODEL_API_KEY`, `KIMI_API_KEY` with `KIMI_BASE_URL`, or \
                 `DASHSCOPE_API_KEY` with `DASHSCOPE_BASE_URL`."
                    .to_string(),
            ));
        }
        if request.mode != CreateSessionMode::Harness && !self.services.has_agent_cli() {
            return Err(ApiSessionError::Operation(
                "No supported backend CLI found on `PATH`; only Harness sessions are available."
                    .to_string(),
            ));
        }
        if let CreateSessionMode::Stacked { parent_session_id } = &request.mode {
            let parent_project_id = self
                .services
                .db()
                .sessions()
                .load_session_project_id(parent_session_id)
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?
                .ok_or(ApiSessionError::NotFound)?;
            if parent_project_id != request.project_id {
                return Err(ApiSessionError::Operation(format!(
                    "Parent session `{parent_session_id}` belongs to project `{}`, not `{}`",
                    parent_project_id, request.project_id
                )));
            }
        }

        Ok(())
    }

    /// Loads one complete session aggregate from persistence plus live queue
    /// state.
    async fn get_api_session(
        &self,
        session_id: &SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        let Some(row) = self
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let session_status = row
            .status
            .parse::<SessionStatus>()
            .unwrap_or(SessionStatus::Done);
        migrate_session_off_retired_model(
            self.services.db(),
            &row.id,
            &row.agent,
            &row.model,
            session_status,
        )
        .await;
        let message_rows = self
            .services
            .db()
            .sessions()
            .load_session_messages(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let queued_messages = self
            .sessions
            .session_for_id(session_id)
            .map_or_default(|session| {
                session
                    .queued_messages
                    .iter()
                    .map(|message| message.transcript_text().to_string())
                    .collect()
            });

        build_api_session(row, message_rows, queued_messages).map(Some)
    }

    /// Sends one validated API message through the existing session workflow.
    pub(super) async fn send_api_message(
        &mut self,
        session_id: &SessionId,
        message: String,
    ) -> Result<(), ApiSessionError> {
        if message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot send an empty session message".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let status = session.status;
        let plan = MessagePlan::for_api(status, session.is_draft_session());
        let prompt = TurnPrompt::from_text(message);

        if self
            .execute_message_plan(session_id, prompt, plan)
            .await
            .map_err(api_error_from_app)?
        {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept a message in status `{status}`"
        )))
    }

    /// Submits a durable turn only when it can bypass the lossy
    /// in-memory chat queue and enter the serialized session worker directly.
    pub(super) async fn submit_api_durable_message(
        &mut self,
        session_id: &SessionId,
        request: DurableMessageRequest,
    ) -> Result<(), ApiSessionError> {
        if request.message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit an empty durable message".to_string(),
            ));
        }
        if request.operation_id.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit a durable message without an operation id".to_string(),
            ));
        }

        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        let status = session.status;
        if !matches!(
            status,
            SessionStatus::Review | SessionStatus::AgentReview | SessionStatus::Question
        ) {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept a durable message in status `{status}`"
            )));
        }

        if self
            .sessions
            .reply_to_durable_message(
                &self.services,
                session_id,
                request.operation_id,
                request.visibility == DurableMessageVisibility::Visible,
                TurnPrompt::from_agent_data(request.message),
            )
            .await
        {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not enqueue the durable message"
        )))
    }

    /// Admits explicit work after checking durable delivery identity.
    pub(super) async fn submit_api_task(
        &mut self,
        session_id: &SessionId,
        request: ag_session::SubmitTaskRequest,
    ) -> Result<(), ApiSessionError> {
        if self
            .services
            .db()
            .tasks()
            .contains_attempt(session_id, &request)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        {
            return Ok(());
        }
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        if !matches!(
            session.status,
            SessionStatus::Review | SessionStatus::AgentReview | SessionStatus::Question
        ) {
            return Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept a task attempt in status `{}`",
                session.status
            )));
        }
        if self
            .sessions
            .reply_to_task(&self.services, session_id, request)
            .await
        {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not enqueue the task attempt"
        )))
    }

    /// Cancels a session through the ordinary lifecycle workflow.
    pub(super) async fn cancel_api_session(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        self.cancel_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Claims structured question answers against the current persisted
    /// question set before resuming the session.
    pub(super) async fn answer_api_questions(
        &mut self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        let row = self
            .services
            .db()
            .sessions()
            .load_session(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            .ok_or(ApiSessionError::NotFound)?;
        let persisted_questions = row.questions.unwrap_or_default();
        let questions = api_questions_from_json(Some(&persisted_questions), session_id)?;
        validate_question_answers(&questions, &request.answers)?;
        let message = question_answer_message(&request.answers);
        let status = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?
            .status;

        self.services
            .db()
            .sessions()
            .update_session_questions(session_id, "")
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let send_result = if self
            .sessions
            .reply_to_question_answers(&self.services, session_id, message)
            .await
        {
            Ok(())
        } else {
            Err(ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept question answers in status `{status}`"
            )))
        };
        if let Err(send_error) = send_result {
            self.services
                .db()
                .sessions()
                .update_session_questions(session_id, &persisted_questions)
                .await
                .map_err(|restore_error| question_restore_error(&send_error, &restore_error))?;

            return Err(send_error);
        }
        self.clear_diff_comment_progress(session_id);
        self.services.emit_app_event(AppEvent::RefreshSessions);

        Ok(())
    }

    pub(super) async fn merge_api_session(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        self.sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;

        self.merge_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    /// Loads inherited launch settings and verifies that the source belongs
    /// to the requested project.
    async fn inherited_creation_settings(
        &self,
        source_session_id: Option<&SessionId>,
        project_id: i64,
    ) -> Result<Option<InheritedCreationSettings>, ApiSessionError> {
        let Some(source_session_id) = source_session_id else {
            return Ok(None);
        };
        let source_row = self
            .services
            .db()
            .sessions()
            .load_session(source_session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            .ok_or(ApiSessionError::NotFound)?;
        let source = build_api_session(source_row, Vec::new(), Vec::new())?;
        if source.settings.project_id != project_id {
            return Err(ApiSessionError::Operation(format!(
                "Session `{source_session_id}` belongs to project `{}`, not `{project_id}`",
                source.settings.project_id
            )));
        }

        Ok(Some(InheritedCreationSettings {
            base_branch: source.settings.base_branch,
            settings: SessionCreationSettings {
                agent: source.settings.agent,
                permission_mode: source.settings.permission_mode,
                personality_id: source.settings.personality_id,
                reasoning_level: source.settings.reasoning_level,
                response_style: source.settings.response_style,
                speed_mode: source.settings.speed_mode,
            },
        }))
    }

    /// Resolves the active project into worktree creation inputs.
    fn api_project_creation_context(
        &self,
        base_branch_override: Option<String>,
    ) -> Result<ApiProjectCreationContext, ApiSessionError> {
        let base_branch = base_branch_override
            .or_else(|| self.projects.git_branch().map(str::to_string))
            .ok_or_else(|| {
                ApiSessionError::Operation("Git branch is required to create a session".to_string())
            })?;

        Ok(ApiProjectCreationContext { base_branch })
    }

    /// Attempts to register a newly persisted active-project session before
    /// acknowledging creation, scheduling a refresh retry when loading is
    /// temporarily unavailable.
    pub(super) async fn finish_api_session_creation(&mut self, session_id: &str) {
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        let _ = self
            .sessions
            .register_created_session(&self.services, session_id, self.projects.working_dir())
            .await;
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.services.emit_app_event(AppEvent::RefreshSessions);
    }
}

/// Worktree inputs resolved for one API-requested project.
struct ApiProjectCreationContext {
    base_branch: String,
}

/// Launch settings loaded from one existing session.
struct InheritedCreationSettings {
    base_branch: String,
    settings: SessionCreationSettings,
}

/// Combines a rejected question answer with a subsequent persistence failure.
fn question_restore_error(
    send_error: &ApiSessionError,
    restore_error: &impl std::fmt::Display,
) -> ApiSessionError {
    ApiSessionError::Operation(format!(
        "{send_error}; failed to restore session questions: {restore_error}"
    ))
}

/// Validates that a structured answer set exactly matches the current
/// persisted questions.
fn validate_question_answers(
    questions: &[QuestionItem],
    answers: &[QuestionAnswer],
) -> Result<(), ApiSessionError> {
    if questions.is_empty() {
        return Err(ApiSessionError::Operation(
            "Session has no questions to answer".to_string(),
        ));
    }

    if questions.len() != answers.len() {
        return Err(ApiSessionError::Operation(format!(
            "Expected {} question answers, received {}",
            questions.len(),
            answers.len()
        )));
    }

    for (question_index, (question, answer)) in questions.iter().zip(answers).enumerate() {
        if question.text != answer.question {
            return Err(ApiSessionError::Operation(format!(
                "Question answer {} is stale",
                question_index + 1
            )));
        }
        if answer.answer.trim().is_empty() {
            return Err(ApiSessionError::Operation(format!(
                "Question answer {} is empty",
                question_index + 1
            )));
        }
    }

    Ok(())
}

/// Formats validated structured answers into the existing clarification
/// follow-up prompt.
fn question_answer_message(answers: &[QuestionAnswer]) -> String {
    let mut lines = vec!["Clarifications:".to_string()];

    for (question_index, answer) in answers.iter().enumerate() {
        lines.push(format!("{}. Q: {}", question_index + 1, answer.question));
        lines.push(format!("   A: {}", answer.answer));
    }

    lines.join("\n")
}

/// Converts complete persistence rows into the public session aggregate.
fn build_api_session(
    row: SessionRow,
    message_rows: Vec<SessionMessageRow>,
    queued_messages: Vec<String>,
) -> Result<ag_session::Session, ApiSessionError> {
    let project_id = row.project_id.ok_or_else(|| {
        ApiSessionError::InvalidData(format!("session `{}` has no project", row.id))
    })?;
    let status = row
        .status
        .parse::<SessionStatus>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let reasoning_level = row
        .reasoning_level_override
        .as_deref()
        .and_then(|value| value.parse::<ReasoningLevel>().ok())
        .unwrap_or_default();
    let permission_mode = row
        .permission_mode
        .parse::<ag_contracts::PermissionMode>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let speed_mode = row.speed_mode.parse::<SpeedMode>().unwrap_or_default();
    let response_style = row
        .response_style
        .parse::<ResponseStyle>()
        .unwrap_or_default();
    let messages = message_rows
        .into_iter()
        .map(api_message_from_row)
        .collect::<Result<Vec<_>, _>>()?;
    let questions = api_questions_from_json(row.questions.as_deref(), &row.id)?;
    let review_request = row
        .review_request
        .map(api_review_request_from_row)
        .transpose()?;
    let draft_prompt = (row.is_draft && !row.prompt.is_empty()).then_some(row.prompt);

    Ok(ag_session::Session {
        created_at: row.created_at,
        draft_prompt,
        id: SessionId::from(row.id),
        messages,
        published_upstream_ref: row.published_upstream_ref,
        questions,
        queued_messages,
        review_request,
        settings: SessionSettings {
            agent: parse_persisted_session_agent_model(Some(&row.agent), &row.model),
            base_branch: row.base_branch,
            is_draft: row.is_draft,
            parent_session_id: row.parent_session_id.map(SessionId::from),
            permission_mode,
            personality_id: row.personality_id,
            project_id,
            reasoning_level,
            response_style,
            speed_mode,
        },
        status,
        title: row.title,
        updated_at: row.updated_at,
    })
}

/// Converts one persisted transcript row into its shared typed model.
fn api_message_from_row(row: SessionMessageRow) -> Result<SessionMessage, ApiSessionError> {
    let kind = row.kind.parse::<SessionMessageKind>().map_err(|error| {
        ApiSessionError::InvalidData(format!(
            "session message at position {}: {error}",
            row.position
        ))
    })?;

    Ok(SessionMessage::new(row.position, kind, row.content))
}

/// Parses current and legacy persisted clarification-question payloads.
fn api_questions_from_json(
    raw_json: Option<&str>,
    session_id: &str,
) -> Result<Vec<QuestionItem>, ApiSessionError> {
    let Some(raw_json) = raw_json.filter(|value| !value.is_empty()) else {
        return Ok(Vec::new());
    };

    if let Ok(questions) = serde_json::from_str::<Vec<QuestionItem>>(raw_json) {
        return Ok(questions);
    }

    serde_json::from_str::<Vec<String>>(raw_json)
        .map(|questions| questions.into_iter().map(QuestionItem::new).collect())
        .map_err(|error| {
            ApiSessionError::InvalidData(format!(
                "session `{session_id}` has invalid questions: {error}"
            ))
        })
}

/// Converts persisted joined forge metadata into its shared typed model.
fn api_review_request_from_row(
    row: SessionReviewRequestRow,
) -> Result<ReviewRequest, ApiSessionError> {
    let forge_kind = row
        .forge_kind
        .parse()
        .map_err(ApiSessionError::InvalidData)?;
    let state = row
        .state
        .parse::<ReviewRequestState>()
        .map_err(ApiSessionError::InvalidData)?;

    Ok(ReviewRequest {
        last_refreshed_at: row.last_refreshed_at,
        summary: ag_session::ReviewRequestSummary {
            display_id: row.display_id,
            forge_kind,
            source_branch: row.source_branch,
            state,
            status_summary: row.status_summary,
            target_branch: row.target_branch,
            title: row.title,
            web_url: row.web_url,
        },
    })
}

/// Preserves stable not-found semantics while translating host app errors.
pub(super) fn api_error_from_app(error: AppError) -> ApiSessionError {
    match error {
        AppError::Session(error) => api_error_from_session(error),
        other => ApiSessionError::Operation(other.to_string()),
    }
}

/// Preserves stable not-found semantics while translating session errors.
fn api_error_from_session(error: SessionError) -> ApiSessionError {
    match error {
        SessionError::NotFound => ApiSessionError::NotFound,
        other => ApiSessionError::Operation(other.to_string()),
    }
}

#[cfg(test)]
#[path = "session_api_test.rs"]
mod tests;
