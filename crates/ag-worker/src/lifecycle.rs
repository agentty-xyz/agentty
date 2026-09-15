use std::future::Future;
use std::time::Duration;

use async_trait::async_trait;

use crate::{OperationRepository, SessionOperationRow};

/// Injected monotonic wait boundary for run heartbeats.
#[async_trait]
pub trait Clock: Send + Sync {
    /// Waits until the next heartbeat is due.
    async fn wait(&self);
}

/// Production heartbeat clock, driven by the host Tokio runtime.
pub struct HeartbeatClock;

#[async_trait]
impl Clock for HeartbeatClock {
    async fn wait(&self) {
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

/// Executes an already-started operation and records its terminal state.
///
/// Tracking is best effort: storage failures are reported to `on_store_error`
/// without dropping in-flight work. A heartbeat cannot resurrect terminal rows.
/// The operation includes host post-processing, so its result is distinct from
/// the runtime's model-turn completion event.
/// `is_canceled` identifies host cancellation errors so they are persisted as
/// canceled rather than failed, while preserving the original result.
/// Repository terminal updates also honor persisted cancellation requests,
/// including operations that return success or an ordinary error after a stop.
///
/// # Errors
/// Returns the operation's own failure, preserving its original error type.
pub async fn execute<E, F>(
    store: &dyn OperationRepository<E>,
    clock: &dyn Clock,
    operation_id: &str,
    operation: impl Future<Output = Result<(), F>>,
    is_canceled: impl Fn(&F) -> bool,
    on_store_error: impl Fn(E),
) -> Result<(), F>
where
    E: Send + Sync + 'static,
    F: std::fmt::Display,
{
    tokio::pin!(operation);
    let result = loop {
        tokio::select! {
            result = &mut operation => break result,
            () = clock.wait() => {
                tokio::select! {
                    result = &mut operation => break result,
                    heartbeat = store.heartbeat(operation_id) => {
                        if let Err(error) = heartbeat { on_store_error(error); }
                    }
                }
            }
        }
    };
    let recorded = match &result {
        Ok(()) => store.mark_session_operation_done(operation_id).await,
        Err(error) if is_canceled(error) => {
            store
                .mark_session_operation_canceled(operation_id, &error.to_string())
                .await
        }
        Err(error) => {
            store
                .mark_session_operation_failed(operation_id, &error.to_string())
                .await
        }
    };
    if let Err(error) = recorded {
        on_store_error(error);
    }
    result
}

/// Reconciles host state before marking abandoned operations failed.
///
/// # Errors
/// Leaves operations recoverable if loading or host reconciliation fails.
/// Hosts must ensure the previous worker process has stopped before calling.
pub async fn recover<E, F, R, Fut>(
    store: &dyn OperationRepository<E>,
    reason: &str,
    reconcile: R,
) -> Result<(), F>
where
    E: Send + Sync + 'static,
    F: From<E>,
    R: FnOnce(Vec<SessionOperationRow>) -> Fut,
    Fut: Future<Output = Result<(), F>>,
{
    let unfinished = store.load_unfinished_session_operations().await?;
    reconcile(unfinished).await?;
    store.fail_unfinished_session_operations(reason).await?;
    Ok(())
}

#[cfg(test)]
#[path = "lifecycle_test.rs"]
mod tests;
