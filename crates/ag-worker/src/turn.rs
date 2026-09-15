use std::time::Duration;

use ag_runtime::{AgentChannel, AgentError, TurnEvent, TurnRequest, TurnResult};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// Runs one turn and delegates resource cancellation to the runtime owner.
///
/// Cancellation drops the turn without polling it again, then gives the runtime
/// owner a bounded shutdown period. Adapters must clean up when their future
/// drops, including when it has never been polled.
///
/// # Errors
/// Returns the runtime failure or a typed user interruption.
pub async fn run_turn(
    channel: &dyn AgentChannel,
    session_id: String,
    request: TurnRequest,
    events: mpsc::UnboundedSender<TurnEvent>,
    cancellation: CancellationToken,
) -> Result<TurnResult, AgentError> {
    let interrupted =
        || AgentError::InterruptedByUser("[Stopped] Session interrupted by user.".to_string());
    if cancellation.is_cancelled() {
        let _ = tokio::time::timeout(Duration::from_secs(5), channel.shutdown_session(session_id))
            .await;
        return Err(interrupted());
    }
    let mut turn = channel.run_turn(session_id.clone(), request, events);
    tokio::select! {
        biased;
        () = cancellation.cancelled() => {
            drop(turn);
            let _ = tokio::time::timeout(Duration::from_secs(5), channel.shutdown_session(session_id)).await;
            Err(interrupted())
        }
        result = &mut turn => result,
    }
}

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
