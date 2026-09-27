use std::time::Duration;

use ag_contracts::{AgentError, TurnEvent, TurnRequest, TurnResult};
use ag_scheduler::SessionAdmission;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Worker-owned execution handle for one session's runtime.
///
/// Hosts retain this handle rather than a raw runtime adapter. Submit turns
/// from the session's serial worker, including nested assistance, so ordered
/// workflow effects finish before another command can execute.
#[derive(Clone)]
pub struct SessionRunClient {
    execution_policy: ag_contracts::ExecutionPolicy,
    runtime: ag_runtime::SessionRuntime,
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

    /// Executes a turn under worker cancellation and runtime cleanup.
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
        let interrupted =
            || AgentError::InterruptedByUser("[Stopped] Session interrupted by user.".to_string());
        if cancellation.is_cancelled() {
            self.shutdown_after_cancellation().await;
            return Err(interrupted());
        }
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
        let mut turn = runtime.run_turn(session_id.clone(), request, events);
        let result = tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                drop(turn);
                let _ = tokio::time::timeout(Duration::from_secs(5), runtime.shutdown(session_id)).await;
                Err(interrupted())
            }
            result = &mut turn => result,
        };
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

        self.runtime.shutdown(self.session_id.clone()).await
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
            session_admission,
            session_id,
        }
    }

    async fn shutdown_after_cancellation(&self) {
        // Closed admission means the process is stopping; still attempt
        // provider shutdown before reporting the cancellation.
        let _permit = self.session_admission.acquire_cleanup().await.ok();
        let _ = tokio::time::timeout(
            Duration::from_secs(5),
            self.runtime.shutdown(self.session_id.clone()),
        )
        .await;
    }
}

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
