use std::time::Duration;

use ag_contracts::{AgentError, TurnEvent, TurnRequest, TurnResult};
use ag_scheduler::SessionAdmission;
use ag_telemetry::{FutureExt as _, Outcome, Span};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

/// Worker-owned execution handle for one session's runtime.
///
/// Hosts retain this handle rather than a raw runtime adapter. Submit turns
/// from the session's serial worker, including nested assistance, so ordered
/// workflow effects finish before another command can execute.
#[derive(Clone)]
pub struct SessionRunClient {
    execution_policy: ag_contracts::ExecutionPolicy,
    runtime: ag_runtime::SessionRuntime,
    runtime_id: String,
    session_admission: SessionAdmission,
    session_id: String,
}

impl SessionRunClient {
    /// Creates the selected runtime inside the worker execution boundary.
    pub fn new(
        session_id: String,
        kind: ag_session::AgentKind,
        config: &crate::RuntimeConfig,
    ) -> Self {
        Self::from_runtime(
            session_id,
            config.factory.session(kind),
            config.execution_policy(kind),
            config.session_admission.clone(),
        )
    }

    /// Creates a task-owned runtime while retaining session admission and
    /// tracing.
    ///
    /// The host must submit this client from the owning session's serial
    /// worker. The context's host-allocated UUID selects independent retained
    /// processes and native history; it is never a caller's task ID or a
    /// provider conversation ID. All attempts reuse the persisted context.
    /// Hosts supply task-only continuation and shut down each owned client
    /// before deleting its session. This constructor does not apply role
    /// instructions or change request permissions.
    ///
    /// # Errors
    /// Returns a runtime error if the context ID is not a UUID, before
    /// composing an adapter or creating runtime resources.
    pub fn for_task_context(
        context: &ag_session::TaskAgentContext,
        config: &crate::RuntimeConfig,
    ) -> Result<Self, AgentError> {
        Self::for_isolated_context(
            &context.id,
            &context.session_id,
            context.assignment.agent.kind(),
            config,
        )
    }

    /// Creates an isolated conversation without assigning a workflow role.
    /// The host supplies a durable UUID and submits through the owning serial
    /// worker. Provider history and cleanup use this UUID rather than chat.
    ///
    /// # Errors
    /// Rejects an invalid context UUID before runtime composition.
    pub fn for_isolated_context(
        context_id: &str,
        session_id: &ag_session::SessionId,
        kind: ag_session::AgentKind,
        config: &crate::RuntimeConfig,
    ) -> Result<Self, AgentError> {
        let runtime_id = Self::isolated_runtime_id(context_id)?;
        let mut client = Self::new(session_id.to_string(), kind, config);
        client.runtime_id = runtime_id;

        Ok(client)
    }

    /// Returns the validated resource identity used by a task runtime.
    /// Hosts may retain this identity before deleting task rows, then remove
    /// the corresponding native history after worker shutdown.
    ///
    /// # Errors
    /// Returns an error when the host-allocated context ID is not a UUID.
    pub fn task_runtime_id(context: &ag_session::TaskAgentContext) -> Result<String, AgentError> {
        Self::isolated_runtime_id(&context.id)
    }

    /// Returns a safe runtime resource key for a durable host context UUID.
    ///
    /// # Errors
    /// Rejects caller-controlled paths and other non-UUID identities.
    pub fn isolated_runtime_id(context_id: &str) -> Result<String, AgentError> {
        let context_id = Uuid::parse_str(context_id)
            .map_err(|_| AgentError::Runtime("Task context ID must be a UUID".to_string()))?;

        Ok(format!("task-{context_id}"))
    }

    /// Executes a turn under worker cancellation and runtime cleanup.
    /// Activity snapshots are forwarded unchanged through `events`; hosts own
    /// their projection and persistence alongside the final response.
    /// Cancellation drops the turn before allowing up to five seconds for
    /// adapter shutdown. Before a turn starts, cleanup admission may delay
    /// the interruption result; its five-second timer starts after admission.
    ///
    /// # Errors
    /// Returns the runtime failure or a typed user interruption.
    pub async fn submit(
        &self,
        mut request: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
        cancellation: CancellationToken,
    ) -> Result<TurnResult, AgentError> {
        request.execution_policy = self.execution_policy.clone();
        let runtime = &self.runtime;
        let session_id = self.session_id.clone();
        let runtime_id = self.runtime_id.clone();
        let interrupted =
            || AgentError::InterruptedByUser("[Stopped] Session interrupted by user.".to_string());
        if cancellation.is_cancelled() {
            self.shutdown_after_cancellation().await;
            return Err(interrupted());
        }
        let admission = Span::child("admission.wait");
        let turn_permit = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                self.shutdown_after_cancellation().await;
                return Err(interrupted());
            }
            permit = self.session_admission.acquire() => {
                permit.map_err(|error| AgentError::Runtime(format!("Session admission closed: {error}")))?
            }
        };
        admission.finish(Outcome::Completed);
        let span = Span::child("agent.run");
        span.attribute("agentty.session.id", session_id.clone());
        span.attribute("agentty.context.id", runtime_id.clone());
        span.attribute("gen_ai.request.model", request.model.clone());
        let mut turn = Box::pin(
            runtime
                .run_turn(runtime_id.clone(), request, events)
                .with_context(span.context()),
        );
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                ag_telemetry::milestone("cancellation_observed");
                drop(turn);
                let _ = tokio::time::timeout(Duration::from_secs(5), Span::child("cleanup").run(runtime.shutdown(runtime_id), |_| Outcome::Failed)).await;
                Err(interrupted())
            }
            result = &mut turn => result,
        };
        let outcome = match &result {
            Ok(result) => {
                ag_telemetry::milestone("agent.completed");
                span.attribute(
                    "gen_ai.usage.input_tokens",
                    i64::try_from(result.input_tokens).unwrap_or(i64::MAX),
                );
                span.attribute(
                    "gen_ai.usage.output_tokens",
                    i64::try_from(result.output_tokens).unwrap_or(i64::MAX),
                );
                span.attribute("agentty.context_reset", result.context_reset);
                Outcome::Completed
            }
            Err(AgentError::InterruptedByUser(_)) => Outcome::Canceled,
            Err(_) => Outcome::Failed,
        };
        span.finish(outcome);
        drop(turn_permit);

        result
    }

    /// Releases the runtime after the session mailbox drains or is abandoned.
    ///
    /// # Errors
    /// Returns the runtime's cleanup failure or an admission closure.
    pub async fn shutdown(&self) -> Result<(), AgentError> {
        let _permit = self
            .session_admission
            .acquire_cleanup()
            .await
            .map_err(|error| {
                AgentError::Runtime(format!("Session cleanup admission closed: {error}"))
            })?;

        Span::child("cleanup")
            .run(self.runtime.shutdown(self.runtime_id.clone()), |_| {
                Outcome::Failed
            })
            .await
    }

    pub(crate) fn from_runtime(
        session_id: String,
        runtime: ag_runtime::SessionRuntime,
        execution_policy: ag_contracts::ExecutionPolicy,
        session_admission: SessionAdmission,
    ) -> Self {
        Self {
            execution_policy,
            runtime,
            runtime_id: session_id.clone(),
            session_admission,
            session_id,
        }
    }

    async fn shutdown_after_cancellation(&self) {
        ag_telemetry::milestone("cancellation_observed");
        // Closed admission means the process is stopping; still attempt
        // provider shutdown before reporting the cancellation.
        let _permit = self.session_admission.acquire_cleanup().await.ok();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            Span::child("cleanup").run(self.runtime.shutdown(self.runtime_id.clone()), |_| {
                Outcome::Failed
            }),
        )
        .await;
    }
}

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
