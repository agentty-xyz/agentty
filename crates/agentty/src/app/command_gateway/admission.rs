//! Foreground dispatch preserving each workflow's admission and reply policy.

use ag_session::{CreateSessionRequest, ProjectScope, ReviewRequest, SessionError, SessionId};
use tokio::sync::oneshot;

use super::{
    CommandDelivery, CommandInitiator, RemoteScope, ReviewRequestCommand, SessionCommand,
    SessionMutation,
};
use crate::app::message_admission::{MessageIntent, MessagePlan};
use crate::app::session_api::api_error_from_app;
use crate::app::{App, AppError};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;

/// Refusal of a request from a paired device that remote control does not
/// serve.
const REMOTE_NOT_SERVED: &str = "Remote control does not serve this operation";

impl App {
    /// Admits a typed mutation on the foreground owner. Response senders retain
    /// their existing lifetime: immediate admission, prepared creation, or
    /// worker-owned publication completion.
    ///
    /// A paired device's mutation is refused first, in this same turn, when
    /// remote control does not serve it or it names a session the host does
    /// not list; see [`Self::remote_refusal`].
    pub(crate) async fn admit_session_command(
        &mut self,
        command: SessionCommand,
    ) -> Result<(), SessionError> {
        let SessionCommand {
            id,
            correlation_id,
            initiator,
            delivery,
        } = command;
        let (kind, session_id) = delivery.target();
        tracing::debug!(
            kind,
            session_id = ?session_id,
            command_id = %id,
            correlation_id = ?correlation_id,
            initiator = ?initiator,
            "admitting session command"
        );
        if let Some(refusal) = self.remote_refusal(initiator, delivery.remote_scope()) {
            return delivery.refuse(refusal);
        }
        if let Some(session_id) = session_id.cloned() {
            self.renew_session_page_lease(initiator, &session_id);
        }
        match delivery {
            CommandDelivery::Create {
                request,
                response_tx,
            } => {
                self.begin_session_creation(request, response_tx).await;
            }
            CommandDelivery::Admission {
                request,
                response_tx,
            } => {
                let changes_graph = request.changes_graph();
                let result = self.execute_session_mutation(request).await;
                // Remote devices refetch the graphs they show, whichever
                // frontend admitted the change.
                if changes_graph {
                    self.publish_session_change();
                }
                if let Some(response_tx) = response_tx {
                    let _ = response_tx.send(result);
                } else {
                    return result;
                }
            }
            CommandDelivery::Review {
                request,
                response_tx,
            } => {
                return self.execute_review_request(request, response_tx).await;
            }
        }

        Ok(())
    }

    /// Returns the error that turns away a request from `initiator` that
    /// reaches `scope`, checked in the foreground turn that serves it.
    ///
    /// Only a paired device is limited: it reaches the operations remote
    /// control serves, only the active project, which it otherwise answers
    /// as inactive, and a session only while the host lists it for the
    /// active project, which it otherwise answers as missing. A session on
    /// a page that session-API callers view is outside its reach.
    pub(crate) fn remote_refusal(
        &self,
        initiator: CommandInitiator,
        scope: RemoteScope<'_>,
    ) -> Option<SessionError> {
        if initiator != CommandInitiator::Remote {
            return None;
        }

        match scope {
            RemoteScope::NotServed => Some(SessionError::Operation(REMOTE_NOT_SERVED.to_string())),
            RemoteScope::Project(ProjectScope::Active) => None,
            RemoteScope::Project(ProjectScope::Project(project_id)) => (project_id
                != self.active_project_id())
            .then(|| SessionError::Operation(format!("Project `{project_id}` is not active"))),
            RemoteScope::Session(session_id) => {
                (!self.sessions.is_in_active_page(session_id)).then_some(SessionError::NotFound)
            }
        }
    }

    /// Admits terminal creation directly, retaining staged workspace setup.
    pub(crate) async fn start_session_creation(
        &mut self,
        request: CreateSessionRequest,
        response_tx: Option<oneshot::Sender<Result<SessionId, SessionError>>>,
    ) {
        let initiator = if response_tx.is_some() {
            CommandInitiator::SessionApi
        } else {
            CommandInitiator::SessionView
        };
        let command = SessionCommand::new(
            initiator,
            CommandDelivery::Create {
                request,
                response_tx,
            },
        );
        let _ = self.admit_session_command(command).await;
    }

    /// Admits publication with its original terminal or worker-owned reply.
    pub(crate) async fn admit_review_request(
        &mut self,
        request: ReviewRequestCommand,
        response_tx: Option<oneshot::Sender<Result<ReviewRequest, SessionError>>>,
    ) -> Result<(), SessionError> {
        let initiator = if response_tx.is_some() {
            CommandInitiator::SessionApi
        } else {
            CommandInitiator::SessionView
        };
        let command = SessionCommand::new(
            initiator,
            CommandDelivery::Review {
                request,
                response_tx,
            },
        );

        self.admit_session_command(command).await
    }

    /// Admits the terminal's explicit start action separately from staging.
    pub(crate) async fn admit_staged_start(
        &mut self,
        session_id: SessionId,
    ) -> Result<(), SessionError> {
        let command = SessionCommand::new(
            CommandInitiator::SessionView,
            CommandDelivery::Admission {
                request: SessionMutation::StartStaged { session_id },
                response_tx: None,
            },
        );

        self.admit_session_command(command).await
    }

    /// Applies a pure routing decision through existing workflow boundaries.
    /// Returns whether the workflow accepted the prompt, without waiting for
    /// worker completion or changing the caller's acknowledgement policy.
    pub(in crate::app) async fn execute_message_plan(
        &mut self,
        session_id: &SessionId,
        prompt: TurnPrompt,
        plan: MessagePlan,
    ) -> Result<bool, AppError> {
        match plan {
            MessagePlan::Stage => self.stage_draft_message(session_id, prompt).await?,
            MessagePlan::StageAndStart => {
                self.stage_draft_message(session_id, prompt).await?;
                self.start_staged_session(session_id).await?;
            }
            MessagePlan::Start => self.start_session(session_id, prompt).await?,
            MessagePlan::Queue => self.enqueue_message(session_id, prompt)?,
            MessagePlan::Resume => return Ok(self.reply(session_id, prompt).await),
        }

        Ok(true)
    }

    /// Applies application inputs without owning transport acknowledgement.
    async fn execute_session_mutation(
        &mut self,
        request: SessionMutation,
    ) -> Result<(), SessionError> {
        match request {
            SessionMutation::GraphProposal { command } => {
                self.admit_graph_proposal_command(command).await
            }
            SessionMutation::CancelExecutionGraph { request } => {
                self.cancel_api_execution_graph(request).await
            }
            SessionMutation::ResumeExecutionGraph { request } => {
                self.resume_api_execution_graph(request).await
            }
            SessionMutation::ExtendExecutionGraphBudget { request } => {
                self.extend_api_execution_graph_budget(request).await
            }
            SessionMutation::ContinueExecutionGraphNode { request } => {
                self.continue_api_execution_graph_node(*request).await
            }
            SessionMutation::StartExecutionGraph { request } => {
                self.start_api_execution_graph(*request).await
            }
            SessionMutation::AcceptExecutionGraphResult { reference } => {
                self.accept_api_execution_graph_result(*reference).await
            }
            SessionMutation::ActOnSessionGraph { request } => {
                match self.api_graph_action_mutation(*request).await {
                    Ok(mutation) => Box::pin(self.execute_session_mutation(mutation)).await,
                    Err(error) => Err(SessionError::Operation(error)),
                }
            }
            SessionMutation::SubmitAgentTask {
                request,
                session_id,
            } => Box::pin(self.submit_api_agent_task(&session_id, *request)).await,
            SessionMutation::AcceptAgentPlan {
                request,
                session_id,
            } => self
                .services
                .db()
                .tasks()
                .accept_agent_plan(&session_id, &request)
                .await
                .map(|_| ())
                .map_err(|error| SessionError::Operation(error.to_string())),
            SessionMutation::Prompt {
                session_id,
                intent,
                prompt,
            } => {
                self.execute_prompt_command(session_id, intent, prompt)
                    .await
            }
            SessionMutation::StartStaged { session_id } => self
                .start_staged_session(&session_id)
                .await
                .map_err(api_error_from_app),
            SessionMutation::SendMessage {
                images,
                message,
                session_id,
            } => self.send_api_message(&session_id, message, images).await,
            SessionMutation::SubmitDurableMessage {
                request,
                session_id,
            } => self.submit_api_durable_message(&session_id, request).await,
            SessionMutation::CreateTask {
                request,
                session_id,
            } => self
                .services
                .db()
                .tasks()
                .create_task(&session_id, &request)
                .await
                .map_err(|error| SessionError::Operation(error.to_string())),
            SessionMutation::SubmitTask {
                request,
                session_id,
            } => self.submit_api_task(&session_id, request).await,
            SessionMutation::CompleteTask { task_id } => self
                .services
                .db()
                .tasks()
                .complete_task(&task_id)
                .await
                .map_err(|error| SessionError::Operation(error.to_string())),
            SessionMutation::AnswerQuestions {
                request,
                session_id,
            } => self.answer_api_questions(&session_id, request).await,
            SessionMutation::Cancel { session_id } => self.cancel_api_session(&session_id).await,
            SessionMutation::StopTurn { session_id } => self.stop_api_turn(&session_id).await,
            SessionMutation::Rebase { session_id } => self.rebase_api_session(&session_id).await,
            SessionMutation::UpdateSettings { change, session_id } => {
                self.update_api_session_settings(&session_id, change).await
            }
            SessionMutation::Merge { session_id } => self.merge_api_session(&session_id).await,
            SessionMutation::SyncProject => self.sync_api_project(),
        }
    }

    /// Routes one prepared turn prompt through the lifecycle path for the
    /// active prompt session.
    ///
    /// # Errors
    /// Returns [`SessionError::Operation`] when the plan fails, after writing
    /// its transcript notice, or when the session declines the reply, so
    /// automatic callers can retry instead of assuming the turn started.
    async fn execute_prompt_command(
        &mut self,
        session_id: SessionId,
        intent: MessageIntent,
        prompt: TurnPrompt,
    ) -> Result<(), SessionError> {
        let status = self
            .sessions
            .session_for_id(&session_id)
            .map(|session| session.status);
        let plan = MessagePlan::for_prompt(intent, status);

        match self.execute_message_plan(&session_id, prompt, plan).await {
            Ok(true) => Ok(()),
            Ok(false) => Err(SessionError::Operation(
                "the session did not accept the prompt".to_string(),
            )),
            Err(error) => {
                let notice = if plan == MessagePlan::Queue {
                    TranscriptNotice::QueueError
                } else {
                    TranscriptNotice::Error
                };
                let message = error.to_string();
                self.append_output_for_session(&session_id, &notice.format(&message))
                    .await;

                Err(SessionError::Operation(message))
            }
        }
    }
}
