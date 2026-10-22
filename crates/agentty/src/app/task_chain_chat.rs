//! Session-chat entry points for the orchestrated Planner → Implementer chain.
//!
//! Routing is data-driven: once a session owns a task chain, every composer
//! message, `/apply` instruction, and automatic remediation reaches the
//! orchestrator instead of an ordinary turn. Ownership is read from durable
//! storage at submit time, so routing is correct right after a restart, before
//! the transcript or graph snapshot of the session has loaded.

use std::sync::Arc;

use ag_session::{
    ReplyToTaskChainRequest, SessionError, StartPlanThenImplementRequest, TaskChainPhase,
    TaskChainReplyOutcome,
};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;
use tracing::warn;
use uuid::Uuid;

use crate::app::App;
use crate::app::command_gateway::{
    CommandDelivery, CommandInitiator, SessionCommand, TaskChainReplyCommand,
};
use crate::app::prompt_intent::{PromptSubmission, PromptWorkflowOutcome};
use crate::app::session::SessionTaskService;
use crate::app::task_chain::api_error;
use crate::domain::session::SessionId;
use crate::domain::transcript_notice::TranscriptNotice;
use crate::domain::turn_prompt::TurnPrompt;

/// Rejection shown when `/plan` targets a session that already owns a chain.
const PLAN_ALREADY_STARTED: &str =
    "This session already runs a plan workflow; send your message without `/plan` to continue it.";

/// Response channel of one routed orchestrator reply.
type ReplyResponse = oneshot::Receiver<Result<TaskChainReplyOutcome, SessionError>>;

impl App {
    /// Starts the Planner → Implementer chain from an armed `/plan` composer.
    ///
    /// The typed text becomes the original request and the session's current
    /// agent selection is captured for both roles. Rejections keep the
    /// composer, and its draft, open.
    pub(crate) async fn submit_plan_request(
        &mut self,
        submission: PromptSubmission,
    ) -> PromptWorkflowOutcome {
        let PromptSubmission {
            prompt, session_id, ..
        } = submission;
        if prompt.text.trim().is_empty() {
            return PromptWorkflowOutcome::KeepPrompt;
        }
        if let Some(rejection) = self.task_chain_message_rejection(&session_id).await {
            self.append_prompt_status_line(&session_id, TranscriptNotice::ReplyError, &rejection)
                .await;

            return PromptWorkflowOutcome::KeepPrompt;
        }
        let Some(agent) = self
            .sessions
            .session_for_id(&session_id)
            .map(|session| session.agent)
        else {
            return PromptWorkflowOutcome::KeepPrompt;
        };
        let launch = async {
            let agents = ag_session::PlanThenImplementAgents::bundled().ok_or(
                SessionError::Operation("Bundled workflow agents are unavailable.".into()),
            )?;
            let request = StartPlanThenImplementRequest {
                agent,
                agents: Box::new(agents),
                attachments: prompt.attachments,
                chain_id: Uuid::new_v4().to_string(),
                planner_attempt_id: Uuid::new_v4().to_string(),
                user_request: prompt.text,
            };
            let service = self.session_view_service();

            self.drive_session_request(service.start_plan_then_implement(&session_id, request))
                .await
        }
        .await;
        if let Err(error) = launch {
            self.append_prompt_status_line(
                &session_id,
                TranscriptNotice::ReplyError,
                &format!("The Planner could not start: {error}"),
            )
            .await;

            return PromptWorkflowOutcome::KeepPrompt;
        }
        self.auto_address_review_iterations.remove(&session_id);

        PromptWorkflowOutcome::ShowSession { session_id }
    }

    /// Routes one composer message to the session's chain.
    ///
    /// Returns `None` when the session owns no chain, so the caller submits
    /// an ordinary turn. Busy chains are rejected rather than queued, because
    /// the orchestrator decides what a message means only once the current
    /// role settles; the rejected draft stays in the composer.
    pub(crate) async fn submit_task_chain_prompt(
        &mut self,
        session_id: &SessionId,
        prompt: &TurnPrompt,
    ) -> Option<PromptWorkflowOutcome> {
        match self.session_owns_task_chain(session_id).await {
            Ok(false) => return None,
            Ok(true) => {}
            Err(error) => {
                self.append_prompt_status_line(
                    session_id,
                    TranscriptNotice::ReplyError,
                    &task_chain_reply_failure(&error),
                )
                .await;

                return Some(PromptWorkflowOutcome::KeepPrompt);
            }
        }
        if !self
            .reply_to_owned_task_chain(session_id, prompt.clone())
            .await
        {
            return Some(PromptWorkflowOutcome::KeepPrompt);
        }
        self.auto_address_review_iterations.remove(session_id);

        Some(PromptWorkflowOutcome::ShowSession {
            session_id: session_id.clone(),
        })
    }

    /// Sends a generated instruction, such as an `/apply` prompt, to the
    /// session's chain, or starts an ordinary turn for sessions without one.
    ///
    /// Returns whether the instruction was accepted.
    pub(crate) async fn reply_through_task_chain_or_session(
        &mut self,
        session_id: &SessionId,
        prompt: TurnPrompt,
    ) -> bool {
        match self.session_owns_task_chain(session_id).await {
            Ok(true) => {
                // Role admission resets user-initiated review cycles. Generated
                // remediation continues the current cycle instead.
                let completed_iterations =
                    self.auto_address_review_iterations.get(session_id).copied();
                let accepted = self.reply_to_owned_task_chain(session_id, prompt).await;
                if let Some(completed_iterations) = completed_iterations {
                    self.auto_address_review_iterations
                        .insert(session_id.clone(), completed_iterations);
                }

                return accepted;
            }
            Ok(false) => {}
            Err(error) => {
                self.append_prompt_status_line(
                    session_id,
                    TranscriptNotice::ReplyError,
                    &task_chain_reply_failure(&error),
                )
                .await;

                return false;
            }
        }

        self.reply(session_id, prompt).await
    }

    /// Cancels the role attempt or orchestrator decision a session's chain is
    /// running.
    ///
    /// Returns `false` when the session has no running chain work, so the
    /// caller stops the ordinary turn instead. The canceled role is retried by
    /// the user's next message.
    pub(crate) async fn cancel_running_task_chain(&mut self, session_id: &SessionId) -> bool {
        let chain = match self
            .services
            .db()
            .tasks()
            .list_task_chains(session_id)
            .await
        {
            Ok(mut chains) => chains.pop(),
            Err(error) => {
                warn!(%session_id, %error, "failed to load task chain for cancellation");
                self.append_prompt_status_line(
                    session_id,
                    TranscriptNotice::Error,
                    &format!("Failed to load the running agent: {error}"),
                )
                .await;

                return true;
            }
        };
        let Some(chain) = chain.filter(|chain| {
            self.sessions
                .deciding_task_chain_ids
                .contains_key(&chain.id)
                || matches!(
                    chain.phase,
                    TaskChainPhase::Planning | TaskChainPhase::Implementing
                )
        }) else {
            return false;
        };
        let service = self.session_view_service();
        if let Err(error) = self
            .drive_session_request(service.cancel_task_chain(&chain.id))
            .await
        {
            self.append_prompt_status_line(
                session_id,
                TranscriptNotice::Error,
                &format!("Failed to cancel the running agent: {error}"),
            )
            .await;
        }

        true
    }

    /// Returns whether the session's loaded transcript or graph snapshot
    /// already shows a chain. Used for display only; routing reads storage.
    pub(crate) fn session_shows_task_chain(&self, session_id: &str) -> bool {
        self.task_chain_graphs.graph(session_id).is_some()
            || self.session_has_task_chain_graph_marker(session_id)
    }

    /// Returns why a new chain or chain message cannot be sent, if anything.
    async fn task_chain_message_rejection(&self, session_id: &SessionId) -> Option<String> {
        match self.session_owns_task_chain(session_id).await {
            Ok(true) => Some(PLAN_ALREADY_STARTED.to_string()),
            Ok(false) => None,
            Err(error) => Some(task_chain_reply_failure(&error)),
        }
    }

    /// Reads chain ownership from durable storage.
    ///
    /// A failed read rejects submission so a chain cannot accidentally receive
    /// an ordinary session turn.
    async fn session_owns_task_chain(&self, session_id: &SessionId) -> Result<bool, SessionError> {
        self.services
            .db()
            .tasks()
            .list_task_chains(session_id)
            .await
            .map(|chains| !chains.is_empty())
            .map_err(|error| api_error(&error))
    }

    /// Admits one reply with a fresh delivery identity and reports
    /// rejections in the transcript. Returns whether the reply was accepted.
    ///
    /// A reply waiting for an orchestrator decision is accepted at once; a
    /// failure applying the decided action is reported when it arrives.
    async fn reply_to_owned_task_chain(
        &mut self,
        session_id: &SessionId,
        prompt: TurnPrompt,
    ) -> bool {
        let (response_tx, mut response_rx) = oneshot::channel();
        let command = SessionCommand::new(
            CommandInitiator::SessionView,
            CommandDelivery::TaskChainReply {
                request: TaskChainReplyCommand::Reply {
                    request: ReplyToTaskChainRequest {
                        attachments: prompt.attachments,
                        message: prompt.text,
                        reply_id: Uuid::new_v4().to_string(),
                    },
                    session_id: session_id.clone(),
                },
                response_tx,
            },
        );
        let _ = self.admit_session_command(command).await;
        let error = match response_rx.try_recv() {
            Ok(Ok(_)) => return true,
            Ok(Err(error)) => error,
            Err(TryRecvError::Empty | TryRecvError::Closed) => {
                self.report_deferred_task_chain_reply(session_id, response_rx);

                return true;
            }
        };
        self.append_prompt_status_line(
            session_id,
            TranscriptNotice::ReplyError,
            &task_chain_reply_failure(&error),
        )
        .await;

        false
    }

    /// Reports a failure of the action decided for one reply.
    fn report_deferred_task_chain_reply(&self, session_id: &SessionId, response_rx: ReplyResponse) {
        if let Ok(handles) = self.sessions.session_handles_or_err(session_id) {
            let transcript = Arc::clone(&handles.transcript);
            let services = self.services.clone();
            let session_id = session_id.clone();
            tokio::spawn(async move {
                if let Some(error) = deferred_reply_error(response_rx.await) {
                    SessionTaskService::append_workflow_notice(
                        &transcript,
                        services.db(),
                        &services.event_sender(),
                        &services.session_update_versions(),
                        &session_id,
                        &TranscriptNotice::ReplyError.format(task_chain_reply_failure(&error)),
                    )
                    .await;
                }
            });
        }
    }
}

/// Preserves routing errors and reports a lost completion channel visibly.
fn deferred_reply_error(
    response: Result<Result<TaskChainReplyOutcome, SessionError>, oneshot::error::RecvError>,
) -> Option<SessionError> {
    match response {
        Ok(Ok(_)) => None,
        Ok(Err(error)) => Some(error),
        Err(_) => Some(SessionError::Operation(
            "The orchestrator response was interrupted; check the execution graph before retrying"
                .to_string(),
        )),
    }
}

/// Describes a rejected orchestrator reply and how to proceed.
fn task_chain_reply_failure(error: &SessionError) -> String {
    match error {
        SessionError::TaskChainBusy(TaskChainPhase::Planning) => {
            "The Planner is still working. Wait for it to finish, or close the composer with Esc \
             and press Ctrl+c to cancel it."
                .to_string()
        }
        SessionError::TaskChainBusy(TaskChainPhase::Implementing) => {
            "The Implementer is still working. Wait for it to finish, or close the composer with \
             Esc and press Ctrl+c to cancel it."
                .to_string()
        }
        SessionError::TaskChainBusy(_) => "The orchestrator is still deciding on your previous \
                                           reply. Wait for it to finish."
            .to_string(),
        other => format!("The orchestrator could not route your message: {other}"),
    }
}

#[cfg(test)]
#[path = "task_chain_chat_test.rs"]
mod tests;
