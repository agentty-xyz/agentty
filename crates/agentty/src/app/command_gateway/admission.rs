//! Foreground dispatch preserving each workflow's admission and reply policy.

use ag_session::{CreateSessionRequest, ReviewRequest, SessionError, SessionId};
use tokio::sync::oneshot;

use super::{
    CommandDelivery, CommandInitiator, ReviewRequestCommand, SessionCommand, SessionMutation,
};
use crate::app::message_admission::{MessageIntent, MessagePlan};
use crate::app::session_api::api_error_from_app;
use crate::app::{App, AppError};
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;

impl App {
    /// Admits a typed mutation on the foreground owner. Response senders retain
    /// their existing lifetime: immediate admission, prepared creation, or
    /// worker-owned publication completion.
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
                let result = self.execute_session_mutation(request).await;
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
            CommandDelivery::TaskChainReply {
                request,
                response_tx,
            } => {
                self.execute_task_chain_reply(request, response_tx).await;
            }
        }

        Ok(())
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
            SessionMutation::StartExecutionGraph { request } => {
                self.start_api_execution_graph(*request).await
            }
            SessionMutation::AcceptExecutionGraphResult { reference } => {
                self.accept_api_execution_graph_result(*reference).await
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
            SessionMutation::StartTaskChain {
                request,
                session_id,
            } => self.start_api_task_chain(&session_id, request).await,
            SessionMutation::AcceptTaskChainPlan { chain_id, request } => {
                self.accept_api_task_chain_plan(&chain_id, request).await
            }
            SessionMutation::ContinueTaskChain { chain_id, request } => {
                self.continue_api_task_chain(&chain_id, request).await
            }
            SessionMutation::CancelTaskChain { chain_id } => {
                self.cancel_api_task_chain(&chain_id).await
            }
            SessionMutation::Prompt {
                session_id,
                intent,
                prompt,
            } => {
                self.execute_prompt_command(session_id, intent, prompt)
                    .await;

                Ok(())
            }
            SessionMutation::StartStaged { session_id } => self
                .start_staged_session(&session_id)
                .await
                .map_err(api_error_from_app),
            SessionMutation::SendMessage {
                message,
                session_id,
            } => self.send_api_message(&session_id, message).await,
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
            SessionMutation::Merge { session_id } => self.merge_api_session(&session_id).await,
        }
    }

    /// Routes one prepared turn prompt through the lifecycle path for the
    /// active prompt session.
    async fn execute_prompt_command(
        &mut self,
        session_id: SessionId,
        intent: MessageIntent,
        prompt: TurnPrompt,
    ) {
        let status = self
            .sessions
            .session_for_id(&session_id)
            .map(|session| session.status);
        let plan = MessagePlan::for_prompt(intent, status);

        if let Err(error) = self.execute_message_plan(&session_id, prompt, plan).await {
            let notice = if plan == MessagePlan::Queue {
                TranscriptNotice::QueueError
            } else {
                TranscriptNotice::Error
            };
            self.append_output_for_session(&session_id, &notice.format(error))
                .await;
        }
    }
}
