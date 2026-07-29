//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use ag_agent::{ReasoningLevel, parse_persisted_session_agent_model};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, QuestionAnswer, ReviewRequest,
    ReviewRequestState, SessionBackend, SessionError as ApiSessionError, SessionId, SessionMessage,
    SessionMessageKind, SessionService, SessionSettings, SessionStatus,
};
use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};

use crate::app::branch_publish::{
    BranchPublishTaskFailure, BranchPublishTaskResult, BranchPublishTaskSuccess,
    branch_publish_loading_label, run_branch_publish_action,
};
use crate::app::session::{SessionCreationSettings, migrate_session_off_retired_model};
use crate::app::{
    App, AppError, AppEvent, SessionError, SessionRuntimeCommand, SessionRuntimeHandle,
};
use crate::domain::session::PublishBranchAction;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::{SessionMessageRow, SessionReviewRequestRow, SessionRow};

#[async_trait]
impl SessionBackend for SessionRuntimeHandle {
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

    /// Drives one local API request while processing the actor commands ahead
    /// of it.
    ///
    /// Background callers rely on the terminal event loop to drive the same
    /// mailbox. Foreground callers use this helper so awaiting their own
    /// response never deadlocks the foreground executor.
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
                command = self.sessions.next_command() => {
                    self.apply_session_runtime_command(command).await;
                }
            }
        }
    }

    /// Executes one accepted session command and answers its response channel.
    pub(crate) async fn apply_session_runtime_command(&mut self, command: SessionRuntimeCommand) {
        match command {
            SessionRuntimeCommand::Create {
                request,
                response_tx,
            } => {
                let _ = response_tx.send(self.create_api_session(request).await);
            }
            SessionRuntimeCommand::Get {
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.get_api_session(&session_id).await);
            }
            SessionRuntimeCommand::SendMessage {
                message,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.send_api_message(&session_id, message).await);
            }
            SessionRuntimeCommand::AnswerQuestions {
                request,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.answer_api_questions(&session_id, request).await);
            }
            SessionRuntimeCommand::Cancel {
                response_tx,
                session_id,
            } => {
                let result = self
                    .cancel_session(&session_id)
                    .await
                    .map_err(api_error_from_app);
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::Merge {
                response_tx,
                session_id,
            } => {
                let result = self
                    .merge_session(&session_id)
                    .await
                    .map_err(api_error_from_app);
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::CreateReviewRequest {
                response_tx,
                session_id,
            } => {
                self.start_api_review_request_publish(session_id, response_tx);
            }
        }
    }

    /// Starts review-request publishing on the detached branch-publish
    /// workflow and leaves the foreground command loop available.
    fn start_api_review_request_publish(
        &mut self,
        session_id: SessionId,
        response_tx: oneshot::Sender<Result<ReviewRequest, ApiSessionError>>,
    ) {
        let Some(branch_publish_context) = self.branch_publish_task_context(&session_id) else {
            let _ = response_tx.send(Err(ApiSessionError::NotFound));

            return;
        };

        let clock = self.services.clock();
        let db = self.services.db().clone();
        let event_sender = self.services.event_sender();
        let git_client = self.services.git_client();
        let review_request_client = self.services.review_request_client();
        let publish_future = run_branch_publish_action(
            PublishBranchAction::PublishPullRequest,
            branch_publish_context,
            db,
            clock,
            git_client,
            review_request_client,
            None,
        );

        self.sessions.start_branch_publish(
            &session_id,
            branch_publish_loading_label(PublishBranchAction::PublishPullRequest),
        );
        spawn_api_review_request_publish(publish_future, event_sender, response_tx, session_id);
    }

    /// Creates one API-requested session, validating project relationships and
    /// resolving optional inherited settings before creation mutates state.
    async fn create_api_session(
        &mut self,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        if request.project_id != self.active_project_id() {
            return Err(ApiSessionError::Operation(format!(
                "Project `{}` is not active",
                request.project_id
            )));
        }
        if let CreateSessionMode::Stacked { parent_session_id } = &request.mode {
            let parent = self
                .get_api_session(parent_session_id)
                .await?
                .ok_or(ApiSessionError::NotFound)?;
            if parent.settings.project_id != request.project_id {
                return Err(ApiSessionError::Operation(format!(
                    "Parent session `{parent_session_id}` belongs to project `{}`, not `{}`",
                    parent.settings.project_id, request.project_id
                )));
            }
        }

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
        let session_id = match request.mode {
            CreateSessionMode::Regular => {
                if creation_settings.is_none() {
                    App::create_session(self).await
                } else {
                    let project = self.api_project_creation_context(base_branch_override)?;
                    self.sessions
                        .create_session_for_project(
                            &self.services,
                            request.project_id,
                            &project.base_branch,
                            project.working_dir,
                            creation_settings,
                        )
                        .await
                        .map_err(AppError::from)
                }
            }
            CreateSessionMode::Draft => {
                if creation_settings.is_none() {
                    self.create_draft_session().await
                } else {
                    let project = self.api_project_creation_context(base_branch_override)?;
                    self.sessions
                        .create_draft_session_for_project_with_settings(
                            &self.services,
                            request.project_id,
                            &project.base_branch,
                            creation_settings,
                        )
                        .await
                        .map_err(AppError::from)
                }
            }
            CreateSessionMode::Stacked { parent_session_id } => {
                if let Some(settings) = creation_settings {
                    self.sessions
                        .create_stacked_draft_session_with_settings(
                            &self.services,
                            &parent_session_id,
                            settings,
                        )
                        .await
                        .map_err(AppError::from)
                } else {
                    self.create_stacked_draft_session(&parent_session_id).await
                }
            }
        }
        .map_err(api_error_from_app)?;

        self.finish_api_session_creation(&session_id).await;

        Ok(SessionId::from(session_id))
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
            .map(|session| session.queued_messages.clone())
            .unwrap_or_default();

        build_api_session(row, message_rows, queued_messages).map(Some)
    }

    /// Sends one validated API message through the existing session workflow.
    async fn send_api_message(
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
        let is_draft = session.is_draft_session();
        let status = session.status;
        let prompt = TurnPrompt::from_text(message);

        if status == SessionStatus::Draft {
            if is_draft {
                self.stage_draft_message(session_id, prompt)
                    .await
                    .map_err(api_error_from_app)?;
                self.start_staged_session(session_id)
                    .await
                    .map_err(api_error_from_app)?;
            } else {
                self.start_session(session_id, prompt)
                    .await
                    .map_err(api_error_from_app)?;
            }

            return Ok(());
        }

        if matches!(status, SessionStatus::InProgress | SessionStatus::Rebasing) {
            return self
                .enqueue_message(session_id, prompt)
                .map_err(api_error_from_session);
        }

        if App::reply(self, session_id, prompt).await {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` cannot accept a message in status `{status}`"
        )))
    }

    /// Claims structured question answers against the current persisted
    /// question set before resuming the session.
    async fn answer_api_questions(
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

        Ok(())
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
        let source = self
            .get_api_session(source_session_id)
            .await?
            .ok_or(ApiSessionError::NotFound)?;
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
                personality_id: source.settings.personality_id,
                reasoning_level: source.settings.reasoning_level,
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

        Ok(ApiProjectCreationContext {
            base_branch,
            working_dir: self.projects.working_dir().to_path_buf(),
        })
    }

    /// Attempts to register a newly persisted active-project session before
    /// acknowledging creation, scheduling a refresh retry when loading is
    /// temporarily unavailable.
    async fn finish_api_session_creation(&mut self, session_id: &str) {
        if self
            .sessions
            .sessions()
            .iter()
            .any(|session| session.id == session_id)
        {
            return;
        }

        self.refresh_sessions_now().await;
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
    working_dir: PathBuf,
}

/// Launch settings loaded from one existing session.
struct InheritedCreationSettings {
    base_branch: String,
    settings: SessionCreationSettings,
}

/// Spawns one review-request publish future and routes its result to both the
/// app reducer and the original API caller.
fn spawn_api_review_request_publish(
    publish_future: impl Future<Output = BranchPublishTaskResult> + Send + 'static,
    event_sender: mpsc::UnboundedSender<AppEvent>,
    response_tx: oneshot::Sender<Result<ReviewRequest, ApiSessionError>>,
    session_id: SessionId,
) {
    tokio::spawn(async move {
        let result = publish_future.await;
        let response_result = api_review_request_result(&result);
        let _ = event_sender.send(AppEvent::BranchPublishActionCompleted {
            result: Box::new(result),
            session_id,
        });
        let _ = response_tx.send(response_result);
    });
}

/// Converts one branch-publish task result into the public API result.
fn api_review_request_result(
    result: &BranchPublishTaskResult,
) -> Result<ReviewRequest, ApiSessionError> {
    match result {
        Ok(BranchPublishTaskSuccess::PullRequestPublished { review_request, .. }) => {
            Ok(review_request.clone())
        }
        Ok(BranchPublishTaskSuccess::Pushed { .. }) => Err(ApiSessionError::Operation(
            "Review-request publishing completed without a review request".to_string(),
        )),
        Err(BranchPublishTaskFailure { message, .. }) => {
            Err(ApiSessionError::Operation(message.clone()))
        }
    }
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
            personality_id: row.personality_id,
            project_id,
            reasoning_level,
        },
        status,
        summary: row.summary,
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
fn api_error_from_app(error: AppError) -> ApiSessionError {
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
mod tests {
    use ag_agent::{
        AgentKind, AgentModel, AgentRequestKind, AgentSelection, AppServerTurnResponse,
        MockAppServerClient,
    };
    use ag_forge::ForgeKind;

    use super::*;

    async fn request_session_creation(
        app: &mut App,
        request: CreateSessionRequest,
    ) -> Result<SessionId, ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.create_session(request).await })
            .await
    }

    async fn request_session(
        app: &mut App,
        session_id: SessionId,
    ) -> Result<Option<ag_session::Session>, ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.get_session(&session_id).await })
            .await
    }

    async fn request_message(
        app: &mut App,
        session_id: SessionId,
        message: &str,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();
        let message = message.to_string();

        app.drive_session_request(async move { service.send_message(&session_id, message).await })
            .await
    }

    async fn request_question_answers(
        app: &mut App,
        session_id: SessionId,
        request: AnswerQuestionsRequest,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(
            async move { service.answer_questions(&session_id, request).await },
        )
        .await
    }

    async fn request_cancellation(
        app: &mut App,
        session_id: SessionId,
    ) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.cancel_session(&session_id).await })
            .await
    }

    async fn request_merge(app: &mut App, session_id: SessionId) -> Result<(), ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.merge_session(&session_id).await })
            .await
    }

    async fn request_review_request(
        app: &mut App,
        session_id: SessionId,
    ) -> Result<ReviewRequest, ApiSessionError> {
        let service = app.session_service();

        app.drive_session_request(async move { service.create_review_request(&session_id).await })
            .await
    }

    fn question_transition_app_server(
        first_turn_release: Arc<tokio::sync::Notify>,
        turn_started_tx: tokio::sync::mpsc::UnboundedSender<AgentRequestKind>,
    ) -> MockAppServerClient {
        let turn_index = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut app_server = MockAppServerClient::new();
        app_server.expect_run_turn().times(3..).returning({
            move |request, _| {
                let first_turn_release = Arc::clone(&first_turn_release);
                let request_kind = request.request_kind;
                if request_kind == AgentRequestKind::UtilityPrompt {
                    return Box::pin(async {
                        Ok(app_server_response(
                            r#"{"answer":"Initial prompt","questions":[],"summary":null}"#,
                            None,
                        ))
                    });
                }

                let turn_index =
                    turn_index.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let _ = turn_started_tx.send(request_kind);

                Box::pin(async move {
                    if turn_index == 0 {
                        first_turn_release.notified().await;

                        return Ok(app_server_response(
                            r#"{"answer":"Need detail","questions":[{"text":"Current question?","options":[]}],"summary":null}"#,
                            Some("conversation-1"),
                        ));
                    }

                    Ok(app_server_response(
                        r#"{"answer":"ready","questions":[],"summary":null}"#,
                        Some("conversation-1"),
                    ))
                })
            }
        });
        app_server
            .expect_shutdown_session()
            .times(0..)
            .returning(|_| Box::pin(async {}));

        app_server
    }

    fn app_server_response(
        assistant_message: &str,
        provider_conversation_id: Option<&str>,
    ) -> AppServerTurnResponse {
        AppServerTurnResponse {
            assistant_message: assistant_message.to_string(),
            context_reset: false,
            input_tokens: 0,
            output_tokens: 0,
            pid: None,
            provider_conversation_id: provider_conversation_id.map(str::to_string),
        }
    }

    fn current_question_answer(answer: &str) -> AnswerQuestionsRequest {
        AnswerQuestionsRequest {
            answers: vec![QuestionAnswer {
                answer: answer.to_string(),
                question: "Current question?".to_string(),
            }],
        }
    }

    fn clarification_answer_count(session: &ag_session::Session) -> usize {
        session
            .messages
            .iter()
            .filter(|message| {
                message.kind == SessionMessageKind::UserPrompt
                    && message.content.starts_with("Clarifications:")
            })
            .count()
    }

    fn session_row() -> SessionRow {
        SessionRow {
            added_lines: 12,
            agent: "codex".to_string(),
            base_branch: "main".to_string(),
            created_at: 10,
            deleted_lines: 4,
            has_diff: Some(true),
            id: "session-1".to_string(),
            in_progress_started_at: None,
            in_progress_total_seconds: 40,
            input_tokens: 50,
            is_draft: true,
            model: "gpt-5.6-sol".to_string(),
            output_tokens: 60,
            parent_session_id: Some("parent-1".to_string()),
            personality_id: Some("reviewer".to_string()),
            project_id: Some(7),
            prompt: "staged prompt".to_string(),
            published_upstream_ref: Some("origin/wt/session-1".to_string()),
            questions: Some(
                r#"[{"text":"Which target?","options":["main","develop"]}]"#.to_string(),
            ),
            reasoning_level_override: Some("xhigh".to_string()),
            review_request: Some(SessionReviewRequestRow {
                display_id: "#42".to_string(),
                forge_kind: "GitHub".to_string(),
                last_refreshed_at: 15,
                source_branch: "wt/session-1".to_string(),
                state: "Open".to_string(),
                status_summary: Some("checks passing".to_string()),
                target_branch: "main".to_string(),
                title: "Build feature".to_string(),
                web_url: "https://example.test/pull/42".to_string(),
            }),
            size: "S".to_string(),
            status: "Draft".to_string(),
            summary: Some("Summary".to_string()),
            title: Some("Build feature".to_string()),
            updated_at: 20,
        }
    }

    #[test]
    fn api_review_request_result_requires_a_published_review_request() {
        // Arrange
        let review_request = api_review_request_from_row(
            session_row()
                .review_request
                .expect("review-request fixture should exist"),
        )
        .expect("review-request fixture should parse");
        let published_result = Ok(BranchPublishTaskSuccess::PullRequestPublished {
            branch_name: "wt/session-1".to_string(),
            review_request: review_request.clone(),
            upstream_reference: "origin/wt/session-1".to_string(),
        });
        let pushed_result = Ok(BranchPublishTaskSuccess::Pushed {
            branch_name: "wt/session-1".to_string(),
            review_request_creation: None,
            upstream_reference: "origin/wt/session-1".to_string(),
        });

        // Act
        let published_review_request = api_review_request_result(&published_result);
        let pushed_error = api_review_request_result(&pushed_result)
            .expect_err("a plain branch-push result should not satisfy the API request");

        // Assert
        assert_eq!(published_review_request, Ok(review_request));
        assert_eq!(
            pushed_error,
            ApiSessionError::Operation(
                "Review-request publishing completed without a review request".to_string()
            )
        );
    }

    #[tokio::test]
    async fn api_review_request_publish_runs_detached_from_its_caller() {
        // Arrange
        let publish_started = Arc::new(tokio::sync::Notify::new());
        let publish_release = Arc::new(tokio::sync::Notify::new());
        let publish_future = {
            let publish_started = Arc::clone(&publish_started);
            let publish_release = Arc::clone(&publish_release);

            async move {
                publish_started.notify_one();
                publish_release.notified().await;

                Err(BranchPublishTaskFailure::failed(
                    PublishBranchAction::PublishPullRequest,
                    "simulated publish failure".to_string(),
                ))
            }
        };
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let (response_tx, mut response_rx) = oneshot::channel();

        // Act
        spawn_api_review_request_publish(
            publish_future,
            event_sender,
            response_tx,
            SessionId::from("session-1"),
        );
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            publish_started.notified(),
        )
        .await
        .expect("publish task should start");
        let pending_response = response_rx.try_recv();
        publish_release.notify_one();
        let response = tokio::time::timeout(std::time::Duration::from_secs(1), response_rx)
            .await
            .expect("publish response should arrive")
            .expect("publish response sender should stay available");
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), event_receiver.recv())
            .await
            .expect("publish event should arrive")
            .expect("publish event sender should stay available");

        // Assert
        assert!(matches!(
            pending_response,
            Err(oneshot::error::TryRecvError::Empty)
        ));
        assert_eq!(
            response,
            Err(ApiSessionError::Operation(
                "simulated publish failure".to_string()
            ))
        );
        assert!(matches!(
            event,
            AppEvent::BranchPublishActionCompleted {
                result,
                session_id,
            } if result.is_err() && session_id == "session-1"
        ));
    }

    #[test]
    fn build_api_session_returns_complete_settings_and_messages() {
        // Arrange
        let row = session_row();
        let message_rows = vec![
            SessionMessageRow {
                content: "first".to_string(),
                kind: "user_prompt".to_string(),
                position: 0,
            },
            SessionMessageRow {
                content: "done".to_string(),
                kind: "assistant_answer".to_string(),
                position: 1,
            },
        ];

        // Act
        let session = build_api_session(row, message_rows, vec!["queued message".to_string()])
            .expect("row should convert");

        // Assert
        assert_eq!(session.id, "session-1");
        assert_eq!(session.draft_prompt.as_deref(), Some("staged prompt"));
        assert_eq!(session.messages.len(), 2);
        assert_eq!(session.questions[0].text, "Which target?");
        assert_eq!(session.queued_messages, ["queued message"]);
        assert_eq!(session.settings.project_id, 7);
        assert_eq!(session.settings.parent_session_id, Some("parent-1".into()));
        assert_eq!(session.settings.personality_id.as_deref(), Some("reviewer"));
        assert_eq!(
            session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol)
        );
        assert_eq!(session.settings.reasoning_level, ReasoningLevel::XHigh);
        assert_eq!(
            session
                .review_request
                .expect("review request should convert")
                .summary
                .forge_kind,
            ForgeKind::GitHub
        );
    }

    #[test]
    fn build_api_session_rejects_invalid_persisted_data() {
        // Arrange
        let mut missing_project = session_row();
        missing_project.project_id = None;
        let mut invalid_status = session_row();
        invalid_status.status = "Unknown".to_string();
        let invalid_message = SessionMessageRow {
            content: "content".to_string(),
            kind: "unknown".to_string(),
            position: 4,
        };
        let mut invalid_review = session_row();
        invalid_review
            .review_request
            .as_mut()
            .expect("fixture should have review metadata")
            .state = "Unknown".to_string();
        let mut invalid_questions = session_row();
        invalid_questions.questions = Some("{invalid".to_string());

        // Act
        let missing_project_error = build_api_session(missing_project, Vec::new(), Vec::new())
            .expect_err("project is required");
        let invalid_status_error = build_api_session(invalid_status, Vec::new(), Vec::new())
            .expect_err("status should be validated");
        let invalid_message_error =
            build_api_session(session_row(), vec![invalid_message], Vec::new())
                .expect_err("message kind should be validated");
        let invalid_review_error = build_api_session(invalid_review, Vec::new(), Vec::new())
            .expect_err("review should be validated");
        let invalid_questions_error = build_api_session(invalid_questions, Vec::new(), Vec::new())
            .expect_err("questions should be validated");
        let legacy_questions = api_questions_from_json(Some(r#"["Legacy question"]"#), "session-1")
            .expect("legacy questions should convert");
        let empty_questions =
            api_questions_from_json(Some(""), "session-1").expect("empty questions should convert");

        // Assert
        assert!(matches!(
            missing_project_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_status_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_message_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_review_error,
            ApiSessionError::InvalidData(_)
        ));
        assert!(matches!(
            invalid_questions_error,
            ApiSessionError::InvalidData(_)
        ));
        assert_eq!(legacy_questions[0].text, "Legacy question");
        assert_eq!(empty_questions, Vec::new());
    }

    #[test]
    fn api_error_translation_preserves_not_found() {
        // Arrange / Act
        let app_error = api_error_from_app(AppError::Session(SessionError::NotFound));
        let session_error = api_error_from_session(SessionError::NotFound);
        let workflow_error = api_error_from_app(AppError::Workflow("workflow failed".to_string()));

        // Assert
        assert_eq!(app_error, ApiSessionError::NotFound);
        assert_eq!(session_error, ApiSessionError::NotFound);
        assert_eq!(
            workflow_error,
            ApiSessionError::Operation("workflow failed".to_string())
        );
    }

    #[tokio::test]
    async fn runtime_backend_creates_and_loads_complete_sessions() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();

        // Act
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("regular session should be created");
        app.services
            .db()
            .sessions()
            .append_session_message(&session_id, SessionMessageKind::UserPrompt, "build it")
            .await
            .expect("message should persist");
        let loaded_session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");
        let missing_session = request_session(&mut app, SessionId::from("missing"))
            .await
            .expect("missing lookup should succeed");
        let stacked_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Stacked {
                    parent_session_id: session_id.clone(),
                },
                project_id,
            },
        )
        .await
        .expect("stacked session should be created");
        let stacked_session = request_session(&mut app, stacked_session_id)
            .await
            .expect("stacked session should load")
            .expect("stacked session should exist");
        let inherited_stacked_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(session_id.clone()),
                mode: CreateSessionMode::Stacked {
                    parent_session_id: session_id.clone(),
                },
                project_id,
            },
        )
        .await
        .expect("inherited stacked session should be created");
        let inherited_stacked_session = request_session(&mut app, inherited_stacked_session_id)
            .await
            .expect("inherited stacked session should load")
            .expect("inherited stacked session should exist");

        // Assert
        assert_eq!(loaded_session.id, session_id);
        assert_eq!(loaded_session.status, SessionStatus::Draft);
        assert_eq!(loaded_session.messages.len(), 1);
        assert_eq!(loaded_session.messages[0].content, "build it");
        assert_eq!(
            loaded_session.settings.project_id,
            app.projects.active_project_id()
        );
        assert_eq!(missing_session, None);
        assert_eq!(stacked_session.settings.parent_session_id, Some(session_id));
        assert_eq!(
            inherited_stacked_session.settings.parent_session_id,
            Some(loaded_session.id)
        );
    }

    #[tokio::test]
    async fn runtime_backend_migrates_retired_model_for_inactive_project_lookup() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("main".to_string()))
            .await
            .expect("inactive project should persist");
        let session_id = SessionId::from("inactive-retired-session");
        app.services
            .db()
            .sessions()
            .insert_session(
                &session_id,
                "gemini-3.5-flash",
                "main",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("retired-model session should persist");

        // Act
        let loaded_session = request_session(&mut app, session_id.clone())
            .await
            .expect("session lookup should succeed")
            .expect("session should exist");
        let persisted_row = app
            .services
            .db()
            .sessions()
            .load_session(&session_id)
            .await
            .expect("migrated session should load")
            .expect("migrated session should exist");

        // Assert
        assert_eq!(loaded_session.settings.project_id, inactive_project_id);
        assert_eq!(
            loaded_session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Antigravity, AgentModel::Gemini35FlashLite)
        );
        assert_eq!(persisted_row.agent, "antigravity");
        assert_eq!(persisted_row.model, "gemini-3.5-flash-lite");
    }

    #[tokio::test]
    async fn runtime_backend_rejects_creation_for_inactive_projects() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("develop".to_string()))
            .await
            .expect("inactive project should persist");

        // Act
        let creation_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id: inactive_project_id,
            },
        )
        .await
        .expect_err("inactive project creation should fail");

        // Assert
        assert_eq!(
            creation_error,
            ApiSessionError::Operation(format!("Project `{inactive_project_id}` is not active"))
        );
    }

    #[tokio::test]
    async fn runtime_backend_inherits_launch_settings_for_regular_and_draft_sessions() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let default_session_model = app.sessions.default_session_model();
        let source_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("source session should be created");
        persist_inherited_launch_settings(&app, &source_session_id).await;

        // Act
        let inherited_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id.clone()),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("inherited session should be created");
        let inherited_session = request_session(&mut app, inherited_session_id)
            .await
            .expect("inherited session should load")
            .expect("inherited session should exist");
        let inherited_regular_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id.clone()),
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("inherited regular session should be created");
        let inherited_regular_session = request_session(&mut app, inherited_regular_session_id)
            .await
            .expect("inherited regular session should load")
            .expect("inherited regular session should exist");
        let ordinary_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("ordinary session should be created");
        let ordinary_session = request_session(&mut app, ordinary_session_id)
            .await
            .expect("ordinary session should load")
            .expect("ordinary session should exist");

        // Assert
        assert_eq!(
            inherited_session.settings.agent,
            ag_agent::AgentSelection::new(AgentKind::Claude, AgentModel::ClaudeSonnet5)
        );
        assert_eq!(
            inherited_session.settings.reasoning_level,
            ReasoningLevel::High
        );
        assert_eq!(
            inherited_session.settings.personality_id.as_deref(),
            Some("inherited-personality")
        );
        assert_eq!(
            inherited_regular_session.settings.agent,
            inherited_session.settings.agent
        );
        assert_eq!(
            inherited_regular_session.settings.reasoning_level,
            inherited_session.settings.reasoning_level
        );
        assert_eq!(
            inherited_regular_session.settings.personality_id.as_deref(),
            Some("inherited-personality")
        );
        assert_eq!(
            ordinary_session.settings.agent.model(),
            default_session_model
        );
        assert_eq!(app.sessions.default_session_model(), default_session_model);
    }

    #[tokio::test]
    async fn runtime_backend_rejects_cross_project_inheritance() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("develop".to_string()))
            .await
            .expect("inactive project should persist");
        let source_session_id = SessionId::from("inactive-source");
        app.services
            .db()
            .sessions()
            .insert_session(
                &source_session_id,
                "gpt-5.6-sol",
                "develop",
                "Draft",
                inactive_project_id,
            )
            .await
            .expect("inactive source session should persist");

        // Act
        let project_mismatch_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect_err("cross-project inheritance should fail");

        // Assert
        assert_eq!(
            project_mismatch_error,
            ApiSessionError::Operation(format!(
                "Session `inactive-source` belongs to project `{inactive_project_id}`, not \
                 `{project_id}`"
            ))
        );
    }

    #[tokio::test]
    async fn runtime_backend_rejects_stacked_parent_from_another_project() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let inactive_project_id = app
            .services
            .db()
            .projects()
            .upsert_project("/inactive-project", Some("develop".to_string()))
            .await
            .expect("inactive project should persist");
        let parent_session_id = SessionId::from("inactive-parent");
        app.services
            .db()
            .sessions()
            .insert_session(
                &parent_session_id,
                "gpt-5.6-sol",
                "develop",
                "Review",
                inactive_project_id,
            )
            .await
            .expect("inactive parent session should persist");

        // Act
        let project_mismatch_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Stacked {
                    parent_session_id: parent_session_id.clone(),
                },
                project_id,
            },
        )
        .await
        .expect_err("cross-project stacked creation should fail");

        // Assert
        assert_eq!(
            project_mismatch_error,
            ApiSessionError::Operation(format!(
                "Parent session `{parent_session_id}` belongs to project `{inactive_project_id}`, \
                 not `{project_id}`"
            ))
        );
    }

    async fn persist_inherited_launch_settings(app: &App, session_id: &SessionId) {
        app.services
            .db()
            .sessions()
            .update_session_agent_model(session_id, "claude", "claude-sonnet-5")
            .await
            .expect("source agent should update");
        app.services
            .db()
            .sessions()
            .update_session_reasoning_level(session_id, ReasoningLevel::High)
            .await
            .expect("source reasoning should update");
        app.services
            .db()
            .sessions()
            .update_session_personality_id(session_id, Some("inherited-personality".to_string()))
            .await
            .expect("source personality should update");
    }

    #[tokio::test]
    async fn runtime_backend_preserves_workflow_validation_errors() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();

        // Act
        let project_error = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id: project_id.saturating_add(1),
            },
        )
        .await
        .expect_err("missing project should fail");
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");

        let empty_message_error = request_message(&mut app, session_id.clone(), "  ")
            .await
            .expect_err("empty message should fail");
        let missing_message_error =
            request_message(&mut app, SessionId::from("missing"), "continue")
                .await
                .expect_err("missing session should fail");
        let stale_answers_error = request_question_answers(
            &mut app,
            session_id.clone(),
            AnswerQuestionsRequest {
                answers: vec![QuestionAnswer {
                    answer: "main".to_string(),
                    question: "Which target?".to_string(),
                }],
            },
        )
        .await
        .expect_err("unexpected answer set should fail");
        let cancel_error = request_cancellation(&mut app, SessionId::from("missing"))
            .await
            .expect_err("missing cancellation should fail");
        app.sessions
            .session_at_mut(0)
            .expect("draft session should be loaded")
            .status = SessionStatus::InProgress;
        if let Some(handles) = app.sessions.session_handles().get(&session_id)
            && let Ok(mut status) = handles.status.lock()
        {
            *status = SessionStatus::InProgress;
        }
        request_message(&mut app, session_id.clone(), "queued follow-up")
            .await
            .expect("running session should queue the message");
        let queued_session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Done,
        );
        let terminal_message_error = request_message(&mut app, session_id.clone(), "too late")
            .await
            .expect_err("terminal session should reject messages");
        let merge_error = request_merge(&mut app, session_id.clone())
            .await
            .expect_err("draft session should not merge");
        let missing_review_error =
            request_review_request(&mut app, SessionId::from("missing-review"))
                .await
                .expect_err("missing session should not publish");
        let review_error = request_review_request(&mut app, session_id)
            .await
            .expect_err("draft session should not publish");

        // Assert
        assert!(matches!(project_error, ApiSessionError::Operation(_)));
        assert!(matches!(empty_message_error, ApiSessionError::Operation(_)));
        assert_eq!(missing_message_error, ApiSessionError::NotFound);
        assert!(matches!(stale_answers_error, ApiSessionError::Operation(_)));
        assert_eq!(cancel_error, ApiSessionError::NotFound);
        assert_eq!(queued_session.queued_messages, ["queued follow-up"]);
        assert!(matches!(
            terminal_message_error,
            ApiSessionError::Operation(_)
        ));
        assert!(matches!(merge_error, ApiSessionError::Operation(_)));
        assert_eq!(missing_review_error, ApiSessionError::NotFound);
        assert!(matches!(review_error, ApiSessionError::Operation(_)));
    }

    #[tokio::test]
    async fn runtime_backend_starts_regular_and_staged_draft_messages() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app_server = MockAppServerClient::new();
        app_server
            .expect_run_turn()
            .times(2)
            .returning(move |_, _| {
                let turn_started_tx = turn_started_tx.clone();

                Box::pin(async move {
                    let _ = turn_started_tx.send(());

                    Ok(AppServerTurnResponse {
                        assistant_message: r#"{"answer":"ready","questions":[],"summary":null}"#
                            .to_string(),
                        context_reset: false,
                        input_tokens: 0,
                        output_tokens: 0,
                        pid: None,
                        provider_conversation_id: None,
                    })
                })
            });
        app_server
            .expect_shutdown_session()
            .times(0..)
            .returning(|_| Box::pin(async {}));
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let project_id = app.active_project_id();
        let regular_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id,
            },
        )
        .await
        .expect("regular session should be created");
        let draft_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        for session_id in [&regular_session_id, &draft_session_id] {
            app.set_session_model(
                session_id,
                AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
            )
            .await
            .expect("session model should update");
        }

        // Act
        request_message(&mut app, regular_session_id.clone(), "regular prompt")
            .await
            .expect("regular session should start");
        request_message(&mut app, draft_session_id.clone(), "draft prompt")
            .await
            .expect("staged draft should start");
        for _ in 0..2 {
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("agent turn should start")
                .expect("agent turn signal should be available");
        }

        // Assert
        assert_eq!(
            app.sessions
                .session_for_id(&regular_session_id)
                .map(|session| session.prompt.as_str()),
            Some("regular prompt")
        );
        assert_eq!(
            app.sessions
                .session_for_id(&draft_session_id)
                .map(|session| session.prompt.as_str()),
            Some("draft prompt")
        );
    }

    #[tokio::test]
    async fn runtime_backend_loads_inherited_creation_before_acknowledging_event_backlog() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app_server = MockAppServerClient::new();
        app_server.expect_run_turn().once().returning(move |_, _| {
            let turn_started_tx = turn_started_tx.clone();

            Box::pin(async move {
                let _ = turn_started_tx.send(());

                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ready","questions":[],"summary":null}"#
                        .to_string(),
                    context_reset: false,
                    input_tokens: 0,
                    output_tokens: 0,
                    pid: None,
                    provider_conversation_id: None,
                })
            })
        });
        app_server
            .expect_shutdown_session()
            .times(0..)
            .returning(|_| Box::pin(async {}));
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let project_id = app.active_project_id();
        let source_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("source session should be created");
        for _ in 0..crate::app::reducer::APP_EVENT_DRAIN_BUDGET {
            app.services
                .emit_app_event(crate::app::AppEvent::RefreshProjects);
        }

        // Act
        let inherited_session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: Some(source_session_id),
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("inherited session should be created");
        let send_result =
            request_message(&mut app, inherited_session_id.clone(), "inherited prompt").await;
        tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
            .await
            .expect("agent turn should start")
            .expect("agent turn signal should be available");

        // Assert
        assert_eq!(send_result, Ok(()));
        assert_eq!(
            app.sessions
                .session_for_id(&inherited_session_id)
                .map(|session| session.prompt.as_str()),
            Some("inherited prompt")
        );
    }

    #[tokio::test]
    async fn runtime_backend_queues_one_question_resume_behind_turn_entering_question_state() {
        // Arrange
        let (turn_started_tx, mut turn_started_rx) = tokio::sync::mpsc::unbounded_channel();
        let first_turn_release = Arc::new(tokio::sync::Notify::new());
        let app_server =
            question_transition_app_server(Arc::clone(&first_turn_release), turn_started_tx);
        let clients = crate::test_support::test_app_clients()
            .with_app_server_client_override(Arc::new(app_server));
        let (mut app, _temp_dir) =
            crate::test_support::new_git_test_app_with_clients(clients).await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        app.set_session_model(
            &session_id,
            AgentSelection::new(AgentKind::Codex, AgentModel::Gpt56Sol),
        )
        .await
        .expect("session model should update");
        request_message(&mut app, session_id.clone(), "initial prompt")
            .await
            .expect("initial turn should start");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("initial turn should start")
                .expect("initial turn kind should be available"),
            AgentRequestKind::SessionStart
        );
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("current questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::InProgress,
        );
        let cached_session = app
            .sessions
            .sessions_mut()
            .iter_mut()
            .find(|session| session.id == session_id)
            .expect("session should be loaded");
        cached_session.questions = vec![QuestionItem::new("Stale question?")];
        assert_eq!(cached_session.status, SessionStatus::InProgress);

        // Act
        let answer_result = request_question_answers(
            &mut app,
            session_id.clone(),
            current_question_answer("Current answer"),
        )
        .await;
        let duplicate_answer_error = request_question_answers(
            &mut app,
            session_id.clone(),
            current_question_answer("Duplicate answer"),
        )
        .await
        .expect_err("the persisted question set should be consumed once");
        let queued_messages_before_transition = app
            .sessions
            .session_for_id(&session_id)
            .expect("session should stay loaded")
            .queued_messages
            .clone();
        first_turn_release.notify_one();
        let resumed_turn_kind =
            tokio::time::timeout(std::time::Duration::from_secs(1), turn_started_rx.recv())
                .await
                .expect("question answer should resume")
                .expect("resumed turn kind should be available");
        let session = request_session(&mut app, session_id)
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        assert_eq!(answer_result, Ok(()));
        assert_eq!(
            duplicate_answer_error,
            ApiSessionError::Operation("Session has no questions to answer".to_string())
        );
        assert_eq!(resumed_turn_kind, AgentRequestKind::SessionResume);
        assert!(queued_messages_before_transition.is_empty());
        assert!(session.questions.is_empty());
        assert!(session.queued_messages.is_empty());
        assert_eq!(clarification_answer_count(&session), 1);
    }

    #[tokio::test]
    async fn runtime_backend_does_not_persist_question_answer_when_worker_enqueue_fails() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("current questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::InProgress,
        );

        // Act
        let answer_error = request_question_answers(
            &mut app,
            session_id.clone(),
            current_question_answer("Current answer"),
        )
        .await
        .expect_err("missing active worker should reject question answers");
        let session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        assert_eq!(
            answer_error,
            ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept question answers in status `InProgress`"
            ))
        );
        assert_eq!(session.questions, [QuestionItem::new("Current question?")]);
        assert!(session.messages.is_empty());
    }

    #[tokio::test]
    async fn runtime_backend_restores_claimed_questions_when_resume_is_rejected() {
        // Arrange
        let (mut app, _temp_dir) = crate::test_support::new_git_test_app().await;
        let project_id = app.active_project_id();
        let session_id = request_session_creation(
            &mut app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Draft,
                project_id,
            },
        )
        .await
        .expect("draft session should be created");
        app.services
            .db()
            .sessions()
            .update_session_questions(
                &session_id,
                r#"[{"text":"Current question?","options":[]}]"#,
            )
            .await
            .expect("current questions should persist");
        crate::test_support::set_session_status_for_test(
            &mut app,
            &session_id,
            SessionStatus::Merged,
        );

        // Act
        let answer_error = request_question_answers(
            &mut app,
            session_id.clone(),
            AnswerQuestionsRequest {
                answers: vec![QuestionAnswer {
                    answer: "Current answer".to_string(),
                    question: "Current question?".to_string(),
                }],
            },
        )
        .await
        .expect_err("a read-only session should reject question answers");
        let session = request_session(&mut app, session_id.clone())
            .await
            .expect("session should load")
            .expect("session should exist");

        // Assert
        assert_eq!(
            answer_error,
            ApiSessionError::Operation(format!(
                "Session `{session_id}` cannot accept question answers in status `Merged`"
            ))
        );
        assert_eq!(session.questions, [QuestionItem::new("Current question?")]);
    }

    #[tokio::test]
    async fn project_creation_context_requires_a_base_branch() {
        // Arrange
        let (app, _temp_dir) = crate::test_support::new_test_app().await;

        // Act
        let active_error = app.api_project_creation_context(None).err();

        // Assert
        let expected =
            ApiSessionError::Operation("Git branch is required to create a session".to_string());
        assert_eq!(active_error.as_ref(), Some(&expected));
    }

    #[tokio::test]
    async fn finishing_api_creation_schedules_registration_retry_after_load_failure() {
        // Arrange
        let (mut app, _temp_dir, pool) = crate::test_support::new_git_test_app_with_pool().await;
        let project_id = app.active_project_id();
        app.services
            .db()
            .sessions()
            .insert_session(
                "persisted-session",
                "gpt-5.6-sol",
                "main",
                "Draft",
                project_id,
            )
            .await
            .expect("session should persist before registration");
        sqlx::query("DROP TABLE session")
            .execute(&pool)
            .await
            .expect("session reads should fail");

        // Act
        app.finish_api_session_creation("persisted-session").await;
        let retry_event = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                let event = app
                    .next_app_event()
                    .await
                    .expect("app event channel should remain open");
                if event == AppEvent::RefreshSessions {
                    break event;
                }
            }
        })
        .await
        .expect("registration retry should be scheduled");

        // Assert
        assert_eq!(retry_event, AppEvent::RefreshSessions);
    }

    #[tokio::test]
    async fn runtime_backend_reports_session_read_failures() {
        // Arrange
        let (mut session_query_app, _session_temp_dir, session_pool) =
            crate::test_support::new_git_test_app_with_pool().await;
        let (mut message_query_app, _message_temp_dir, message_pool) =
            crate::test_support::new_git_test_app_with_pool().await;
        let message_project_id = message_query_app.active_project_id();
        let message_session_id = request_session_creation(
            &mut message_query_app,
            CreateSessionRequest {
                inherit_from_session_id: None,
                mode: CreateSessionMode::Regular,
                project_id: message_project_id,
            },
        )
        .await
        .expect("session should be created");
        sqlx::query("DROP TABLE session")
            .execute(&session_pool)
            .await
            .expect("session table should be dropped");
        sqlx::query("DROP TABLE session_message")
            .execute(&message_pool)
            .await
            .expect("message table should be dropped");

        // Act
        let session_query_error =
            request_session(&mut session_query_app, SessionId::from("missing"))
                .await
                .expect_err("session query should fail");
        let message_query_error = request_session(&mut message_query_app, message_session_id)
            .await
            .expect_err("message query should fail");

        // Assert
        assert!(matches!(session_query_error, ApiSessionError::Operation(_)));
        assert!(matches!(message_query_error, ApiSessionError::Operation(_)));
    }

    #[test]
    fn structured_question_answers_require_current_non_empty_pairs() {
        // Arrange
        let questions = vec![
            QuestionItem::new("Which target?"),
            QuestionItem::new("Run tests?"),
        ];
        let valid_answers = vec![
            QuestionAnswer {
                answer: "main".to_string(),
                question: "Which target?".to_string(),
            },
            QuestionAnswer {
                answer: "yes".to_string(),
                question: "Run tests?".to_string(),
            },
        ];
        let mut stale_answers = valid_answers.clone();
        stale_answers[1].question = "Different question".to_string();
        let mut empty_answers = valid_answers.clone();
        empty_answers[0].answer = " ".to_string();

        // Act
        let valid_result = validate_question_answers(&questions, &valid_answers);
        let no_questions_error =
            validate_question_answers(&[], &[]).expect_err("empty question set should fail");
        let missing_error = validate_question_answers(&questions, &valid_answers[..1])
            .expect_err("missing answer should fail");
        let stale_error = validate_question_answers(&questions, &stale_answers)
            .expect_err("stale answer should fail");
        let empty_error = validate_question_answers(&questions, &empty_answers)
            .expect_err("empty answer should fail");
        let message = question_answer_message(&valid_answers);

        // Assert
        assert_eq!(valid_result, Ok(()));
        assert_eq!(
            no_questions_error,
            ApiSessionError::Operation("Session has no questions to answer".to_string())
        );
        assert_eq!(
            missing_error,
            ApiSessionError::Operation("Expected 2 question answers, received 1".to_string())
        );
        assert_eq!(
            stale_error,
            ApiSessionError::Operation("Question answer 2 is stale".to_string())
        );
        assert_eq!(
            empty_error,
            ApiSessionError::Operation("Question answer 1 is empty".to_string())
        );
        assert_eq!(
            message,
            "Clarifications:\n1. Q: Which target?\n   A: main\n2. Q: Run tests?\n   A: yes"
        );
    }

    #[test]
    fn question_restore_error_preserves_both_failures() {
        // Arrange
        let send_error =
            ApiSessionError::Operation("Session cannot accept question answers".to_string());

        // Act
        let error = question_restore_error(&send_error, &"database unavailable");

        // Assert
        assert_eq!(
            error,
            ApiSessionError::Operation(
                "Session cannot accept question answers; failed to restore session questions: \
                 database unavailable"
                    .to_string()
            )
        );
    }
}
