//! Agentty adapter for the frontend-neutral `ag-session` programmatic API.

use std::future::Future;
use std::sync::Arc;

use ag_agent::{ReasoningLevel, ResponseStyle, SpeedMode, parse_persisted_session_agent_model};
use ag_orchestration::{OrchestrationApprovalOutcome, child_session_is_stopped};
use ag_protocol::QuestionItem;
use ag_session::{
    AnswerQuestionsRequest, CoordinatorMessageRequest, CoordinatorMessageVisibility,
    CreateSessionMode, CreateSessionRequest, QuestionAnswer, ReviewRequest, ReviewRequestState,
    SessionBackend, SessionError as ApiSessionError, SessionId, SessionMessage, SessionMessageKind,
    SessionRole, SessionService, SessionSettings, SessionStatus,
};
use async_trait::async_trait;
use tokio::sync::oneshot;

use crate::app::branch_publish::{branch_publish_loading_label, review_request_queued_label};
use crate::app::session::{
    SessionCreationKind, SessionCreationSettings, migrate_session_off_retired_model,
};
use crate::app::session_creation::PreparedSessionCreation;
use crate::app::{
    App, AppError, AppEvent, SessionError, SessionRuntimeAccess, SessionRuntimeCommand,
    SessionRuntimeHandle,
};
use crate::domain::orchestration::{
    IntegrationApproach, OrchestrationStatus, OrchestrationTaskStatus,
};
use crate::domain::session::{PublishBranchAction, Session};
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

    async fn submit_coordinator_message(
        &self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), ApiSessionError> {
        SessionRuntimeHandle::submit_coordinator_message(self, session_id, request).await
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

    /// Returns the capability reserved for orchestration coordinators.
    pub(crate) fn coordinator_session_service(&self) -> SessionService {
        SessionService::new(Arc::new(self.sessions.coordinator_handle()))
    }

    /// Approves the current plan or advances integration with a selected
    /// destination.
    pub(crate) async fn approve_orchestration(
        &self,
        controller_session_id: &str,
        integration_approach: Option<IntegrationApproach>,
    ) -> OrchestrationApprovalOutcome {
        let outcome = ag_orchestration::approve_orchestration(
            self.services.db().orchestrations(),
            controller_session_id,
            integration_approach,
        )
        .await
        .unwrap_or(OrchestrationApprovalOutcome::Unavailable);
        if outcome == OrchestrationApprovalOutcome::Approved {
            self.services.emit_app_event(AppEvent::RefreshSessions);
        }

        outcome
    }

    /// Detaches one managed child and schedules a session-list refresh.
    pub(crate) async fn detach_managed_child(&self, child_session_id: &str) -> bool {
        let detached = ag_orchestration::detach_managed_child(self.services.db(), child_session_id)
            .await
            .unwrap_or(false);
        if detached {
            self.services.emit_app_event(AppEvent::RefreshSessions);
        }

        detached
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

    /// Executes one accepted session command and answers its response channel.
    pub(crate) async fn apply_session_runtime_command(&mut self, command: SessionRuntimeCommand) {
        match command {
            SessionRuntimeCommand::Create {
                request,
                response_tx,
            } => {
                self.start_session_creation(request, Some(response_tx))
                    .await;
            }
            SessionRuntimeCommand::Get {
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.get_api_session(&session_id).await);
            }
            SessionRuntimeCommand::SendMessage {
                access,
                message,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.send_api_message(&session_id, message, access).await);
            }
            SessionRuntimeCommand::SubmitCoordinatorMessage {
                request,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(
                    self.submit_api_coordinator_message(&session_id, request)
                        .await,
                );
            }
            SessionRuntimeCommand::AnswerQuestions {
                access,
                request,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(
                    self.answer_api_questions(&session_id, request, access)
                        .await,
                );
            }
            SessionRuntimeCommand::Cancel {
                access,
                response_tx,
                session_id,
            } => {
                let result = self.cancel_api_session(&session_id, access).await;
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::Merge {
                access,
                response_tx,
                session_id,
            } => {
                let result = self.merge_api_session(&session_id, access).await;
                let _ = response_tx.send(result);
            }
            SessionRuntimeCommand::CreateReviewRequest {
                access,
                response_tx,
                session_id,
            } => {
                self.start_api_review_request_publish(session_id, access, response_tx)
                    .await;
            }
        }
    }

    /// Queues review-request publishing on the session worker and leaves the
    /// foreground command loop available while the API caller awaits the
    /// worker result.
    async fn start_api_review_request_publish(
        &mut self,
        session_id: SessionId,
        access: SessionRuntimeAccess,
        response_tx: oneshot::Sender<Result<ReviewRequest, ApiSessionError>>,
    ) {
        let Some(session) = self.sessions.session_for_id(&session_id) else {
            let _ = response_tx.send(Err(ApiSessionError::NotFound));

            return;
        };
        if session.is_managed() && access != SessionRuntimeAccess::Coordinator {
            let _ = response_tx.send(Err(managed_session_error(&session_id, "publish")));

            return;
        }
        if !session.owns_branch_changes() {
            let _ = response_tx.send(Err(ApiSessionError::Operation(
                "Orchestrator sessions cannot publish review requests".to_string(),
            )));

            return;
        }
        let Some(branch_publish_context) = self.branch_publish_task_context(&session_id) else {
            let _ = response_tx.send(Err(ApiSessionError::NotFound));

            return;
        };
        let branch_operation_lock = Arc::clone(&branch_publish_context.branch_operation_lock);
        // Reserve an idle branch before persistence. An existing owner
        // already serializes worker execution, so the runtime actor never waits
        // here.
        let _branch_operation_guard = branch_operation_lock.try_lock_owned().ok();
        let enqueue_result = self
            .sessions
            .enqueue_review_request_creation(
                &self.services,
                branch_publish_context.session,
                None,
                Some(response_tx),
            )
            .await;
        match enqueue_result {
            Err(error) => {
                let _ = self.sessions.finish_branch_publish(
                    &session_id,
                    crate::domain::transient_message::TransientMessageBody::Markdown(format!(
                        "**Review request publish failed**\n\n{error}"
                    )),
                );
            }
            Ok(Some(queued_order)) => self.sessions.queue_branch_publish(
                &session_id,
                queued_order,
                review_request_queued_label(),
            ),
            Ok(None) => self.sessions.start_branch_publish(
                &session_id,
                branch_publish_loading_label(PublishBranchAction::PublishPullRequest),
            ),
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
        let creation_kind = match request.mode {
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
            CreateSessionMode::Regular => SessionCreationKind::Worker,
            CreateSessionMode::Orchestrator => SessionCreationKind::Orchestrator,
            CreateSessionMode::OrchestrationChild { task_id } => {
                SessionCreationKind::OrchestrationChild { task_id }
            }
            CreateSessionMode::OrchestrationResearch { task_id } => {
                SessionCreationKind::OrchestrationResearch { task_id }
            }
        };
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

        Ok(PreparedSessionCreation::Materialized {
            base_branch: project.base_branch,
            creation_kind,
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
            .map(|session| {
                session
                    .queued_messages
                    .iter()
                    .map(|message| message.transcript_text().to_string())
                    .collect()
            })
            .unwrap_or_default();

        build_api_session(row, message_rows, queued_messages).map(Some)
    }

    /// Sends one validated API message through the existing session workflow.
    async fn send_api_message(
        &mut self,
        session_id: &SessionId,
        message: String,
        access: SessionRuntimeAccess,
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
        if session.is_managed() && access != SessionRuntimeAccess::Coordinator {
            return Err(managed_session_error(session_id, "send messages"));
        }
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

    /// Submits a coordinator-owned turn only when it can bypass the lossy
    /// in-memory chat queue and enter the serialized session worker directly.
    async fn submit_api_coordinator_message(
        &mut self,
        session_id: &SessionId,
        request: CoordinatorMessageRequest,
    ) -> Result<(), ApiSessionError> {
        if request.message.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit an empty coordinator message".to_string(),
            ));
        }
        if request.operation_id.trim().is_empty() {
            return Err(ApiSessionError::Operation(
                "Cannot submit a coordinator message without an operation id".to_string(),
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
                "Session `{session_id}` cannot accept a coordinator message in status `{status}`"
            )));
        }

        if self
            .sessions
            .reply_to_coordinator_message(
                &self.services,
                session_id,
                request.operation_id,
                request.visibility == CoordinatorMessageVisibility::Visible,
                TurnPrompt::from_agent_data(request.message),
            )
            .await
        {
            return Ok(());
        }

        Err(ApiSessionError::Operation(format!(
            "Session `{session_id}` could not enqueue the coordinator message"
        )))
    }

    /// Cascades orchestrator cancellation to every active child before
    /// canceling the controller itself.
    ///
    /// A child cancellation failure aborts the cascade and leaves the
    /// orchestration active so the controller never reports a false terminal
    /// cancellation while a worker may still be running.
    async fn cancel_api_session(
        &mut self,
        session_id: &SessionId,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        if self
            .sessions
            .session_for_id(session_id)
            .is_some_and(Session::is_managed)
            && access != SessionRuntimeAccess::Coordinator
        {
            return Err(managed_session_error(session_id, "cancel"));
        }
        let is_orchestrator = self
            .sessions
            .session_for_id(session_id)
            .is_some_and(|session| session.role == SessionRole::Orchestrator);
        if is_orchestrator {
            self.cancel_api_orchestration(session_id).await?;
        }

        if access == SessionRuntimeAccess::Coordinator
            && self
                .sessions
                .session_for_id(session_id)
                .is_some_and(Session::is_managed)
        {
            return self
                .sessions
                .cancel_managed_session(&self.services, session_id)
                .await
                .map_err(api_error_from_session);
        }

        self.cancel_session(session_id)
            .await
            .map_err(api_error_from_app)
    }

    async fn cancel_api_orchestration(
        &mut self,
        session_id: &SessionId,
    ) -> Result<(), ApiSessionError> {
        let Some(orchestration) = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(());
        };
        let cancellation_started = self
            .services
            .db()
            .orchestrations()
            .begin_orchestration_cancellation(orchestration.id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        if !cancellation_started {
            return Ok(());
        }
        let tasks = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        for task in tasks.into_iter().filter(|task| {
            task.status
                .parse::<OrchestrationTaskStatus>()
                .is_ok_and(|status| !status.is_settled())
        }) {
            let child_session_id = if task.child_session_id.is_some() {
                task.child_session_id
            } else {
                self.services
                    .db()
                    .orchestrations()
                    .load_child_session_id_for_task(task.id)
                    .await
                    .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            };
            if let Some(child_session_id) = child_session_id.as_deref()
                && !child_session_is_stopped(task.child_status.as_deref())
            {
                self.sessions
                    .cancel_managed_session(&self.services, child_session_id)
                    .await
                    .map_err(api_error_from_session)?;
            }
            self.services
                .db()
                .orchestrations()
                .update_orchestration_task_status(
                    task.id,
                    &OrchestrationTaskStatus::Canceled.to_string(),
                    None,
                )
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        }
        self.services
            .db()
            .orchestrations()
            .update_orchestration_status(
                orchestration.id,
                &OrchestrationStatus::Canceled.to_string(),
            )
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        self.sessions
            .update_orchestration_progress(session_id, None);

        Ok(())
    }

    /// Returns the active child count displayed in orchestration cancellation
    /// confirmation.
    pub(crate) async fn orchestration_running_child_count(&self, session_id: &str) -> usize {
        ag_orchestration::running_child_count(self.services.db(), session_id).await
    }

    /// Claims structured question answers against the current persisted
    /// question set before resuming the session.
    async fn answer_api_questions(
        &mut self,
        session_id: &SessionId,
        request: AnswerQuestionsRequest,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        if self
            .sessions
            .session_for_id(session_id)
            .is_some_and(Session::is_managed)
            && access != SessionRuntimeAccess::Coordinator
        {
            return Err(managed_session_error(session_id, "answer questions"));
        }
        let session_role = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?
            .role;
        let question_relay = if session_role == SessionRole::Orchestrator {
            self.orchestration_question_target(session_id).await?
        } else {
            None
        };
        let target_session_id = question_relay
            .as_ref()
            .map_or(session_id, |(_, target_session_id)| target_session_id);
        let row = self
            .services
            .db()
            .sessions()
            .load_session(target_session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
            .ok_or(ApiSessionError::NotFound)?;
        let persisted_questions = row.questions.unwrap_or_default();
        let questions = api_questions_from_json(Some(&persisted_questions), target_session_id)?;
        validate_question_answers(&questions, &request.answers)?;
        let message = question_answer_message(&request.answers);
        let status = self
            .sessions
            .session_for_id(target_session_id)
            .ok_or(ApiSessionError::NotFound)?
            .status;

        self.services
            .db()
            .sessions()
            .update_session_questions(target_session_id, "")
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        let send_result = if self
            .sessions
            .reply_to_question_answers(&self.services, target_session_id, message)
            .await
        {
            Ok(())
        } else {
            Err(ApiSessionError::Operation(format!(
                "Session `{target_session_id}` cannot accept question answers in status `{status}`"
            )))
        };
        if let Err(send_error) = send_result {
            self.services
                .db()
                .sessions()
                .update_session_questions(target_session_id, &persisted_questions)
                .await
                .map_err(|restore_error| question_restore_error(&send_error, &restore_error))?;

            return Err(send_error);
        }
        self.clear_diff_comment_progress(target_session_id);
        if let Some((session_orchestration_id, _)) = question_relay {
            self.services
                .db()
                .orchestrations()
                .clear_orchestration_questions(session_orchestration_id)
                .await
                .map_err(|error| ApiSessionError::Operation(error.to_string()))?;
        }
        self.services.emit_app_event(AppEvent::RefreshSessions);

        Ok(())
    }

    /// Resolves the exact managed task claimed by the controller's question
    /// inbox.
    async fn orchestration_question_target(
        &self,
        controller_session_id: &SessionId,
    ) -> Result<Option<(i64, SessionId)>, ApiSessionError> {
        let Some(orchestration) = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_for_controller(controller_session_id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?
        else {
            return Ok(None);
        };
        let Some(relayed_question_task_id) = orchestration.relayed_question_task_id else {
            return Ok(None);
        };
        let tasks = self
            .services
            .db()
            .orchestrations()
            .load_orchestration_tasks(orchestration.id)
            .await
            .map_err(|error| ApiSessionError::Operation(error.to_string()))?;

        let target_session_id = tasks
            .into_iter()
            .find(|task| task.id == relayed_question_task_id)
            .and_then(|task| task.child_session_id)
            .map(SessionId::from)
            .ok_or_else(|| {
                ApiSessionError::Operation(format!(
                    "Orchestration question relay references unavailable task \
                     `{relayed_question_task_id}`"
                ))
            })?;

        Ok(Some((orchestration.id, target_session_id)))
    }

    /// Returns whether the controller's visible questions are a child relay.
    pub(crate) async fn has_orchestration_question_proxy(
        &self,
        controller_session_id: &str,
    ) -> bool {
        self.orchestration_question_target(&SessionId::from(controller_session_id))
            .await
            .is_ok_and(|relay| relay.is_some())
    }

    async fn merge_api_session(
        &mut self,
        session_id: &SessionId,
        access: SessionRuntimeAccess,
    ) -> Result<(), ApiSessionError> {
        let session = self
            .sessions
            .session_for_id(session_id)
            .ok_or(ApiSessionError::NotFound)?;
        if session.is_managed() && access != SessionRuntimeAccess::Coordinator {
            return Err(managed_session_error(session_id, "merge"));
        }

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
                role: SessionRole::Worker,
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
        .parse::<ag_agent::PermissionMode>()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?;
    let role = row
        .role
        .as_deref()
        .map(str::parse::<SessionRole>)
        .transpose()
        .map_err(|error| ApiSessionError::InvalidData(format!("session `{}`: {error}", row.id)))?
        .unwrap_or_default();
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
            role,
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

/// Builds the stable capability error returned for direct managed-worker
/// mutations.
fn managed_session_error(session_id: &SessionId, action: &str) -> ApiSessionError {
    ApiSessionError::Operation(format!(
        "Session `{session_id}` is managed by an orchestration campaign and cannot {action} \
         directly"
    ))
}

#[cfg(test)]
#[path = "session_api_test.rs"]
mod tests;
