//! Natural-language reply routing for a session's latest task chain.
//!
//! A reply is recorded once as a visible user prompt, then routed by the
//! workflow phase observed at decision time. While a plan awaits acceptance,
//! an orchestrator decision runs in the background through the Run Worker and
//! applies its action through the command gateway again.

use std::sync::PoisonError;

use ag_protocol::AgentResponse;
use ag_session::{
    AcceptTaskChainPlanRequest, ContinueTaskChainRequest, ReplyToTaskChainRequest, SessionError,
    SessionId, SessionMessageKind, TaskChain, TaskChainPhase, TaskChainReplyOutcome,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;

use crate::app::App;
use crate::app::command_gateway::{TaskChainReplyAction, TaskChainReplyCommand};
use crate::app::session::SessionTaskService;
use crate::app::task_chain::api_error;
use crate::app::task_chain_decision::TaskChainReplyDecision;

/// Workflow notice shown after a reply accepted the plan.
const PLAN_ACCEPTED_NOTICE: &str = "Plan accepted · Implementer started";

/// Run purpose recorded for the orchestrator decision.
const DECISION_RUN_PURPOSE: &str = "task chain reply decision";

/// Routed reply: admitted now, or waiting for an orchestrator decision.
enum TaskChainReplyRoute {
    Admitted(TaskChainReplyOutcome),
    Decide {
        action: TaskChainReplyAction,
        decision: TaskChainReplyDecision,
    },
}

impl App {
    /// Routes a reply or applies a decided action. A reply waiting for a
    /// decision transfers its response to the background decision task.
    pub(in crate::app) async fn execute_task_chain_reply(
        &mut self,
        command: TaskChainReplyCommand,
        response_tx: oneshot::Sender<Result<TaskChainReplyOutcome, SessionError>>,
    ) {
        let result = match command {
            TaskChainReplyCommand::Reply {
                request,
                session_id,
            } => match self.route_task_chain_reply(&session_id, request).await {
                Ok(TaskChainReplyRoute::Admitted(outcome)) => Ok(outcome),
                Ok(TaskChainReplyRoute::Decide { action, decision }) => {
                    self.spawn_task_chain_reply_decision(action, decision, response_tx);

                    return;
                }
                Err(error) => Err(error),
            },
            TaskChainReplyCommand::Apply(action) => {
                self.sessions
                    .deciding_task_chain_ids
                    .remove(&action.chain_id);

                self.apply_task_chain_reply(action).await
            }
        };
        // Fire-and-forget: the caller may have stopped waiting.
        let _ = response_tx.send(result);
    }

    /// Returns an earlier receipt, rejects busy or missing workflows, records
    /// the reply once, and then admits or defers its action.
    async fn route_task_chain_reply(
        &mut self,
        session_id: &SessionId,
        request: ReplyToTaskChainRequest,
    ) -> Result<TaskChainReplyRoute, SessionError> {
        if request.message.trim().is_empty() || request.reply_id.trim().is_empty() {
            return Err(SessionError::Operation(
                "A task chain reply needs a message and a reply id".to_string(),
            ));
        }
        let chain = self
            .services
            .db()
            .tasks()
            .list_task_chains(session_id)
            .await
            .map_err(|error| api_error(&error))?
            .pop()
            .ok_or(SessionError::TaskChainNotFound)?;
        if let Some(outcome) = self
            .task_chain_reply_receipt(&chain, &request.reply_id)
            .await?
        {
            self.record_task_chain_reply(&chain, &request).await?;

            return Ok(TaskChainReplyRoute::Admitted(outcome));
        }
        if matches!(
            chain.phase,
            TaskChainPhase::Planning | TaskChainPhase::Implementing
        ) || self
            .sessions
            .deciding_task_chain_ids
            .contains_key(&chain.id)
        {
            return Err(SessionError::TaskChainBusy(chain.phase));
        }
        let folder = self
            .sessions
            .session_for_id(session_id)
            .map(|session| session.folder.clone())
            .ok_or(SessionError::NotFound)?;
        self.require_task_chain_session_ready(
            session_id,
            chain.current_request.assignment.definition,
        )?;
        let plan = if chain.phase == TaskChainPhase::AwaitingPlanAcceptance {
            Some(self.task_chain_plan(&chain).await?)
        } else {
            None
        };
        self.record_task_chain_reply(&chain, &request).await?;
        let action = TaskChainReplyAction {
            chain_id: chain.id,
            expected_attempt_id: chain.current_request.attempt_id,
            message: request.message,
            outcome: TaskChainReplyOutcome::Continued,
            reply_id: request.reply_id,
            session_id: session_id.clone(),
        };
        let Some(plan) = plan else {
            return self
                .apply_task_chain_reply(action)
                .await
                .map(TaskChainReplyRoute::Admitted);
        };

        Ok(TaskChainReplyRoute::Decide {
            decision: TaskChainReplyDecision {
                agent: chain.agent,
                folder,
                plan,
                reply: action.message.clone(),
            },
            action,
        })
    }

    /// Admits one decided action, anchored to the observed attempt. A reply
    /// whose action was already admitted returns that receipt instead.
    async fn apply_task_chain_reply(
        &mut self,
        action: TaskChainReplyAction,
    ) -> Result<TaskChainReplyOutcome, SessionError> {
        let chain = self.require_task_chain(&action.chain_id).await?;
        if let Some(outcome) = self
            .task_chain_reply_receipt(&chain, &action.reply_id)
            .await?
        {
            return Ok(outcome);
        }
        if action.outcome != TaskChainReplyOutcome::Continued
            && chain.phase != TaskChainPhase::AwaitingPlanAcceptance
        {
            return Err(SessionError::Operation(
                "Workflow changed before reply decision completed".to_string(),
            ));
        }
        let attempt_id = action.outcome.attempt_id(&action.reply_id);
        if action.outcome == TaskChainReplyOutcome::PlanAccepted {
            self.accept_api_task_chain_plan(
                &action.chain_id,
                AcceptTaskChainPlanRequest {
                    attempt_id,
                    plan_attempt_id: action.expected_attempt_id,
                },
            )
            .await?;
            self.append_task_chain_transcript_message(
                &action.session_id,
                SessionMessageKind::WorkflowNotice,
                PLAN_ACCEPTED_NOTICE,
            )
            .await;
        } else {
            self.continue_api_task_chain(
                &action.chain_id,
                ContinueTaskChainRequest {
                    attempt_id,
                    clarification: Some(action.message),
                    expected_attempt_id: action.expected_attempt_id,
                },
            )
            .await?;
        }

        Ok(action.outcome)
    }

    /// Decides in the background with the workflow's captured selection and
    /// session ownership, then applies the decision through the gateway. The
    /// chain stays busy for other replies until that action arrives.
    fn spawn_task_chain_reply_decision(
        &mut self,
        action: TaskChainReplyAction,
        decision: TaskChainReplyDecision,
        response_tx: oneshot::Sender<Result<TaskChainReplyOutcome, SessionError>>,
    ) {
        let cancellation = CancellationToken::new();
        self.sessions
            .deciding_task_chain_ids
            .insert(action.chain_id.clone(), cancellation.clone());
        let run_client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                cancellation: Some(cancellation),
                purpose: Some(DECISION_RUN_PURPOSE.to_string()),
                session_id: Some(action.session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        );
        let handle = self.sessions.handle();
        tokio::spawn(async move {
            let outcome = decision.decide(run_client.as_ref()).await;
            let result = handle
                .apply_task_chain_reply(TaskChainReplyAction { outcome, ..action })
                .await;
            // Fire-and-forget: the caller may have stopped waiting.
            let _ = response_tx.send(result);
        });
    }

    /// Finds the outcome whose derived attempt identity was already admitted.
    async fn task_chain_reply_receipt(
        &self,
        chain: &TaskChain,
        reply_id: &str,
    ) -> Result<Option<TaskChainReplyOutcome>, SessionError> {
        for outcome in TaskChainReplyOutcome::ALL {
            if self
                .task_chain_has_receipt(chain, &outcome.attempt_id(reply_id))
                .await?
            {
                return Ok(Some(outcome));
            }
        }

        Ok(None)
    }

    /// Loads the answer of the Planner attempt awaiting acceptance.
    async fn task_chain_plan(&self, chain: &TaskChain) -> Result<String, SessionError> {
        let plan = self
            .services
            .db()
            .tasks()
            .load_agent_history(&chain.planner_task_id)
            .await
            .map_err(|error| api_error(&error))?
            .and_then(|history| {
                history
                    .turns
                    .into_iter()
                    .find(|turn| turn.attempt_id == chain.current_request.attempt_id)
            })
            .and_then(|turn| serde_json::from_str::<AgentResponse>(&turn.assistant_message).ok())
            .map_or_default(|response| response.answer);

        Ok(plan)
    }

    /// Persists the reply with its transcript prompt atomically, then mirrors
    /// a newly recorded prompt into the live transcript.
    async fn record_task_chain_reply(
        &self,
        chain: &TaskChain,
        request: &ReplyToTaskChainRequest,
    ) -> Result<(), SessionError> {
        let recorded = self
            .services
            .db()
            .tasks()
            .record_task_chain_reply(&chain.id, &request.reply_id, &request.message)
            .await
            .map_err(|error| api_error(&error))?;
        if recorded && let Ok(handles) = self.sessions.session_handles_or_err(&chain.session_id) {
            handles
                .transcript
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .append_message(SessionMessageKind::UserPrompt, &request.message);
            SessionTaskService::emit_session_updated(
                &self.services.event_sender(),
                &self.services.session_update_versions(),
                &chain.session_id,
            );
        }

        Ok(())
    }
}

#[cfg(test)]
#[path = "task_chain_reply_test.rs"]
mod tests;
