use std::time::Duration;

use ag_contracts::{AgentError, TurnEvent, TurnRequest, TurnResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Worker-owned execution handle for one session's runtime.
///
/// Hosts retain this handle rather than a raw runtime adapter. Submit turns
/// from the session's serial worker, including nested assistance, so ordered
/// workflow effects finish before another command can execute.
#[derive(Clone)]
pub struct SessionRunClient {
    runtime: ag_runtime::SessionRuntime,
    session_id: String,
}

impl SessionRunClient {
    /// Creates the selected runtime inside the worker execution boundary.
    pub fn new(
        session_id: String,
        kind: ag_session::AgentKind,
        config: &crate::RuntimeConfig,
    ) -> Self {
        Self::from_runtime(session_id, config.factory.session(kind))
    }

    /// Executes a turn under worker cancellation and runtime cleanup.
    /// Cancellation drops the turn before allowing up to five seconds for
    /// adapter shutdown, including when the turn has never been polled.
    ///
    /// # Errors
    /// Returns the runtime failure or a typed user interruption.
    pub async fn submit(
        &self,
        request: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
        cancellation: CancellationToken,
    ) -> Result<TurnResult, AgentError> {
        let runtime = &self.runtime;
        let session_id = self.session_id.clone();
        let interrupted =
            || AgentError::InterruptedByUser("[Stopped] Session interrupted by user.".to_string());
        if cancellation.is_cancelled() {
            let _ =
                tokio::time::timeout(Duration::from_secs(5), runtime.shutdown(session_id)).await;
            return Err(interrupted());
        }
        let mut turn = runtime.run_turn(session_id.clone(), request, events);
        tokio::select! {
            biased;
            () = cancellation.cancelled() => {
                drop(turn);
                let _ = tokio::time::timeout(Duration::from_secs(5), runtime.shutdown(session_id)).await;
                Err(interrupted())
            }
            result = &mut turn => result,
        }
    }

    /// Releases the runtime after the session mailbox drains or is abandoned.
    ///
    /// # Errors
    /// Returns the runtime's cleanup failure.
    pub async fn shutdown(&self) -> Result<(), AgentError> {
        self.runtime.shutdown(self.session_id.clone()).await
    }

    pub(crate) fn from_runtime(session_id: String, runtime: ag_runtime::SessionRuntime) -> Self {
        Self {
            runtime,
            session_id,
        }
    }
}

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
