//! Foreground dispatch preserving each workflow's admission and reply policy.

use ag_session::{CreateSessionRequest, SessionError, SessionId};
use tokio::sync::oneshot;

use super::{CommandInitiator, ReviewRequestCommand, SessionCommand, SessionMutation};
use crate::app::App;
use crate::app::prompt_intent::PromptSessionMode;
use crate::app::session_api::api_error_from_app;
use crate::domain::session::Status;
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
            request,
        } = command;
        let (kind, session_id) = request.target();
        tracing::debug!(
            kind,
            session_id = ?session_id,
            command_id = %id,
            correlation_id = ?correlation_id,
            initiator = ?initiator,
            "admitting session command"
        );
        match request {
            SessionMutation::Prompt {
                session_id,
                session_mode,
                prompt,
            } => {
                self.execute_prompt_command(session_id, session_mode, prompt)
                    .await;
            }
            SessionMutation::StartStaged { session_id } => {
                return self
                    .start_staged_session(&session_id)
                    .await
                    .map_err(api_error_from_app);
            }
            SessionMutation::Create {
                request,
                response_tx,
            } => {
                self.begin_session_creation(request, response_tx).await;
            }
            SessionMutation::SendMessage {
                message,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.send_api_message(&session_id, message).await);
            }
            SessionMutation::SubmitDurableMessage {
                request,
                response_tx,
                session_id,
            } => {
                let _ =
                    response_tx.send(self.submit_api_durable_message(&session_id, request).await);
            }
            SessionMutation::AnswerQuestions {
                request,
                response_tx,
                session_id,
            } => {
                let _ = response_tx.send(self.answer_api_questions(&session_id, request).await);
            }
            SessionMutation::Cancel {
                response_tx,
                session_id,
            } => {
                let result = self.cancel_api_session(&session_id).await;
                let _ = response_tx.send(result);
            }
            SessionMutation::Merge {
                response_tx,
                session_id,
            } => {
                let result = self.merge_api_session(&session_id).await;
                let _ = response_tx.send(result);
            }
            SessionMutation::CreateReviewRequest(command) => {
                return self.execute_review_request(command).await;
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
            SessionMutation::Create {
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
    ) -> Result<(), SessionError> {
        let command = SessionCommand::new(
            request.initiator(),
            SessionMutation::CreateReviewRequest(request),
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
            SessionMutation::StartStaged { session_id },
        );

        self.admit_session_command(command).await
    }

    /// Routes one prepared turn prompt through the lifecycle path for the
    /// active prompt session.
    async fn execute_prompt_command(
        &mut self,
        session_id: SessionId,
        session_mode: PromptSessionMode,
        prompt: TurnPrompt,
    ) {
        if session_mode == PromptSessionMode::NewDraft {
            if let Err(error) = self.stage_draft_message(&session_id, prompt).await {
                self.append_output_for_session(&session_id, &TranscriptNotice::Error.format(error))
                    .await;
            }
        } else if session_mode != PromptSessionMode::Existing {
            if let Err(error) = self.start_session(&session_id, prompt).await {
                self.append_output_for_session(&session_id, &TranscriptNotice::Error.format(error))
                    .await;
            }
        } else if self.session_queues_messages(&session_id) {
            if let Err(error) = self.enqueue_message(&session_id, prompt) {
                self.append_output_for_session(
                    &session_id,
                    &TranscriptNotice::QueueError.format(error),
                )
                .await;
            }
        } else {
            self.reply(&session_id, prompt).await;
        }
    }

    /// Returns whether the targeted session is running a turn or rebase, used
    /// to route submissions into the in-memory message queue.
    fn session_queues_messages(&self, session_id: &str) -> bool {
        self.sessions
            .sessions()
            .iter()
            .find(|session| session.id == session_id)
            .is_some_and(|session| matches!(session.status, Status::InProgress | Status::Rebasing))
    }
}
