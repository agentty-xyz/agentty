//! Natural-language reply routing for a session's latest task chain.
//!
//! A reply is recorded once as a visible user prompt, bound to the exact
//! execution it answers. Its first decision is durable, so redelivery reapplies
//! the same idempotent graph action. While a plan awaits acceptance, an
//! orchestrator decision runs in the background through the Run Worker and
//! applies its action through the command gateway again.

use std::sync::{Arc, PoisonError};

use ag_protocol::{ProtocolRequestProfile, parse_protocol_response_strict};
use ag_session::{
    AcceptTaskChainPlanRequest, ContinueTaskChainRequest, ReplyToTaskChainRequest, SessionError,
    SessionId, SessionMessageKind, TASK_CHAIN_PLAN_OUTPUT, TaskAgentRole, TaskChain,
    TaskChainPhase, TaskChainReplyOutcome,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::warn;
use uuid::Uuid;

use crate::app::App;
use crate::app::command_gateway::{TaskChainReplyAction, TaskChainReplyCommand};
use crate::app::session::SessionTaskService;
use crate::app::task_chain::{api_error, execution_attempt_id};
use crate::app::task_chain_decision::TaskChainReplyDecision;
use crate::domain::turn_prompt::TurnPrompt;
use crate::infra::db::TaskChainReplyRecord;

/// Workflow notice shown after a reply accepted the plan.
const PLAN_ACCEPTED_NOTICE: &str = "Plan accepted · Implementer started";

/// Run purpose recorded for the orchestrator decision.
const DECISION_RUN_PURPOSE: &str = "task chain reply decision";

/// Routed reply: admitted now, or waiting for an orchestrator decision.
enum TaskChainReplyRoute {
    Admitted(TaskChainReplyOutcome),
    Decide {
        action: Box<TaskChainReplyAction>,
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
                    self.spawn_task_chain_reply_decision(*action, decision, response_tx);

                    return;
                }
                Err(error) => Err(error),
            },
            TaskChainReplyCommand::Apply {
                action,
                cancellation,
            } => {
                if cancellation.is_cancelled()
                    || !self
                        .sessions
                        .deciding_task_chain_ids
                        .get(&action.chain_id)
                        .is_some_and(|pending| Arc::ptr_eq(pending, &cancellation))
                {
                    Err(Self::changed_before_decision())
                } else {
                    self.sessions
                        .deciding_task_chain_ids
                        .remove(&action.chain_id);

                    self.apply_task_chain_reply(action).await
                }
            }
        };
        // Fire-and-forget: the caller may have stopped waiting.
        let _ = response_tx.send(result);
    }

    /// Reapplies an earlier decision, rejects busy or missing workflows,
    /// records a new reply once, and then admits or defers its action.
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
        let prompt = TurnPrompt {
            attachments: request.attachments.clone(),
            ..TurnPrompt::from_text(request.message.clone())
        };
        let recorded = self
            .services
            .db()
            .graphs()
            .load_task_chain_reply(session_id, &request.reply_id)
            .await
            .map_err(|error| api_error(&error))?;
        let chain = if let Some(reply) = recorded {
            if reply.prompt != prompt {
                return Err(SessionError::Operation(
                    "Reply identity was already used with different inputs".into(),
                ));
            }
            if let Some(outcome) = reply.outcome {
                return self
                    .apply_task_chain_reply(Self::reply_action(
                        session_id,
                        &request.reply_id,
                        &reply,
                        outcome,
                    ))
                    .await
                    .map(TaskChainReplyRoute::Admitted);
            }
            let chain = self.require_task_chain(&reply.chain_id).await?;
            if chain.current_execution.as_ref() != Some(&reply.source) {
                return Err(Self::changed_before_decision());
            }

            chain
        } else {
            self.record_new_task_chain_reply(session_id, &request.reply_id, &prompt)
                .await?
        };
        if self
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
        let plan = if chain.phase == TaskChainPhase::AwaitingPlanAcceptance {
            Some(self.task_chain_plan(&chain).await?)
        } else {
            None
        };
        let action = TaskChainReplyAction {
            attachments: request.attachments,
            chain_id: chain.id.clone(),
            expected_attempt_id: chain
                .current_attempt_id()
                .map_or_default(ToString::to_string),
            message: request.message,
            outcome: if plan.is_some() && !prompt.attachments.is_empty() {
                TaskChainReplyOutcome::PlanRevised
            } else {
                TaskChainReplyOutcome::Continued
            },
            reply_id: request.reply_id,
            session_id: session_id.clone(),
        };
        // A reply carrying new visual guidance must reach the Planner before
        // acceptance; the text-only approval classifier cannot interpret it.
        let Some(plan) = plan.filter(|_| action.attachments.is_empty()) else {
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
            action: Box::new(action),
        })
    }

    /// Anchors a new visible reply to the latest settled workflow execution.
    async fn record_new_task_chain_reply(
        &mut self,
        session_id: &SessionId,
        reply_id: &str,
        prompt: &TurnPrompt,
    ) -> Result<TaskChain, SessionError> {
        let chain = self
            .services
            .db()
            .graphs()
            .list_task_chains(session_id)
            .await
            .map_err(|error| api_error(&error))?
            .pop()
            .ok_or(SessionError::TaskChainNotFound)?;
        self.require_idle_task_chain(&chain).await?;
        let source = chain.current_execution.clone().ok_or_else(|| {
            SessionError::Operation("The workflow has no execution to reply to".into())
        })?;
        self.require_task_chain_session_ready(session_id, chain.current_role)?;
        self.record_task_chain_reply(&chain, reply_id, prompt, &source)
            .await?;

        Ok(chain)
    }

    /// Records the first decision for a reply, then applies that durable
    /// decision through idempotent graph actions. A reply that was already
    /// decided reapplies its original decision.
    async fn apply_task_chain_reply(
        &mut self,
        action: TaskChainReplyAction,
    ) -> Result<TaskChainReplyOutcome, SessionError> {
        let reply = self
            .services
            .db()
            .graphs()
            .load_task_chain_reply(&action.session_id, &action.reply_id)
            .await
            .map_err(|error| api_error(&error))?
            .ok_or_else(|| SessionError::Operation("Task chain reply is missing".into()))?;
        if reply.outcome.is_none() {
            let chain = self.require_task_chain(&reply.chain_id).await?;
            if chain.current_execution.as_ref() != Some(&reply.source)
                || (action.outcome != TaskChainReplyOutcome::Continued
                    && chain.phase != TaskChainPhase::AwaitingPlanAcceptance)
            {
                return Err(Self::changed_before_decision());
            }
        }
        let continuation_id = (action.outcome != TaskChainReplyOutcome::PlanAccepted)
            .then(|| Uuid::new_v4().to_string());
        let decided = self
            .services
            .db()
            .graphs()
            .decide_task_chain_reply(&action.reply_id, action.outcome, continuation_id)
            .await
            .map_err(|error| api_error(&error))?;
        let outcome = decided
            .outcome
            .ok_or_else(|| SessionError::Operation("Reply decision was not recorded".into()))?;
        let source_attempt_id = execution_attempt_id(&decided.source)
            .cloned()
            .ok_or_else(|| SessionError::Operation("Reply source is not a task".into()))?;
        if outcome == TaskChainReplyOutcome::PlanAccepted {
            let transcript = Arc::clone(
                &self
                    .sessions
                    .session_handles_or_err(&action.session_id)
                    .map_err(|error| SessionError::Operation(error.to_string()))?
                    .transcript,
            );
            let accepted = self
                .accept_api_task_chain_plan(
                    &decided.chain_id,
                    AcceptTaskChainPlanRequest {
                        plan_attempt_id: source_attempt_id,
                    },
                )
                .await?;
            if accepted {
                SessionTaskService::append_workflow_notice(
                    &transcript,
                    self.services.db(),
                    &self.services.event_sender(),
                    &self.services.session_update_versions(),
                    &action.session_id,
                    PLAN_ACCEPTED_NOTICE,
                )
                .await;
            }
        } else {
            let attempt_id = decided.action_id.clone().ok_or_else(|| {
                SessionError::Operation("Reply continuation identity is missing".into())
            })?;
            self.continue_api_task_chain(
                &decided.chain_id,
                ContinueTaskChainRequest {
                    attachments: decided.prompt.attachments.clone(),
                    attempt_id,
                    clarification: Some(decided.prompt.text.clone()),
                    expected_attempt_id: source_attempt_id,
                },
            )
            .await?;
        }

        Ok(outcome)
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
        let cancellation = Arc::new(CancellationToken::new());
        self.sessions
            .deciding_task_chain_ids
            .insert(action.chain_id.clone(), cancellation.clone());
        let run_client = ag_worker::scoped_client(
            self.services.run_client(),
            ag_worker::RunScope {
                cancellation: Some(cancellation.as_ref().clone()),
                purpose: Some(DECISION_RUN_PURPOSE.to_string()),
                session_id: Some(action.session_id.to_string()),
                ..ag_worker::RunScope::default()
            },
        );
        let handle = self.sessions.handle();
        tokio::spawn(async move {
            let outcome = decision.decide(run_client.as_ref()).await;
            // Cancellation already released the busy marker; a canceled
            // decision leaves the plan awaiting a new reply.
            if cancellation.is_cancelled() {
                let _ = response_tx.send(Err(Self::changed_before_decision()));

                return;
            }
            let result = handle
                .apply_task_chain_reply(TaskChainReplyAction { outcome, ..action }, cancellation)
                .await;
            // Fire-and-forget: the caller may have stopped waiting.
            let _ = response_tx.send(result);
        });
    }

    /// Rejects replies while a role works or a decision is pending. Work
    /// waiting for dispatch restarts its driver, for example after restart.
    async fn require_idle_task_chain(&mut self, chain: &TaskChain) -> Result<(), SessionError> {
        if self
            .sessions
            .deciding_task_chain_ids
            .contains_key(&chain.id)
        {
            return Err(SessionError::TaskChainBusy(chain.phase));
        }
        if !matches!(
            chain.phase,
            TaskChainPhase::Planning | TaskChainPhase::Implementing
        ) {
            return Ok(());
        }
        if let Err(error) = self.ensure_task_chain_driver(&chain.id).await {
            warn!(chain_id = chain.id, %error, "failed to resume task chain dispatch");
        }

        Err(SessionError::TaskChainBusy(chain.phase))
    }

    /// Loads the exact Planner answer awaiting acceptance.
    async fn task_chain_plan(&self, chain: &TaskChain) -> Result<String, SessionError> {
        let body = self
            .services
            .db()
            .graphs()
            .list_results(
                &chain.id,
                TaskAgentRole::Planner.id(),
                TASK_CHAIN_PLAN_OUTPUT,
            )
            .await
            .map_err(|error| api_error(&error))?
            .into_iter()
            .find(|result| {
                Some(&result.result.reference.execution) == chain.current_execution.as_ref()
            })
            .map(|result| result.body)
            .ok_or_else(|| SessionError::Operation("Plan version is missing".into()))?;

        Ok(
            parse_protocol_response_strict(&body, ProtocolRequestProfile::SessionTurn)
                .map_or_default(|response| response.answer),
        )
    }

    /// Persists the reply with its transcript prompt atomically, then mirrors
    /// a newly recorded prompt into the live transcript.
    async fn record_task_chain_reply(
        &self,
        chain: &TaskChain,
        reply_id: &str,
        prompt: &TurnPrompt,
        source: &ag_session::GraphExecutionRef,
    ) -> Result<(), SessionError> {
        let recorded = self
            .services
            .db()
            .graphs()
            .record_task_chain_reply(&chain.id, reply_id, prompt, source)
            .await
            .map_err(|error| api_error(&error))?;
        if recorded && let Ok(handles) = self.sessions.session_handles_or_err(&chain.session_id) {
            handles
                .transcript
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .append_message(SessionMessageKind::UserPrompt, &prompt.transcript_text());
            SessionTaskService::emit_session_updated(
                &self.services.event_sender(),
                &self.services.session_update_versions(),
                &chain.session_id,
            );
        }

        Ok(())
    }

    /// Rebuilds the action for a reply whose decision is already durable.
    fn reply_action(
        session_id: &SessionId,
        reply_id: &str,
        reply: &TaskChainReplyRecord,
        outcome: TaskChainReplyOutcome,
    ) -> TaskChainReplyAction {
        TaskChainReplyAction {
            attachments: reply.prompt.attachments.clone(),
            chain_id: reply.chain_id.clone(),
            expected_attempt_id: execution_attempt_id(&reply.source)
                .cloned()
                .unwrap_or_default(),
            message: reply.prompt.text.clone(),
            outcome,
            reply_id: reply_id.to_string(),
            session_id: session_id.clone(),
        }
    }

    fn changed_before_decision() -> SessionError {
        SessionError::Operation("Workflow changed before reply decision completed".to_string())
    }
}

#[cfg(test)]
#[path = "task_chain_reply_test.rs"]
mod tests;
