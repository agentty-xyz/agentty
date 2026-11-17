//! Task spawning that states how each task relates to the caller's trace.
//!
//! `tokio::spawn` and `tokio::task::spawn_blocking` drop the caller's
//! OpenTelemetry context, so spans a task records start detached traces or
//! are not recorded at all. Application and runtime code spawns through these
//! helpers so every call site chooses one behavior explicitly.

use std::future::Future;

use ag_telemetry::{Context, FutureExt as _};
use tokio::task::JoinHandle;

/// Spawns `future` inside the caller's current trace context.
///
/// Spans the task records nest under the span that was active at the call
/// site.
pub(crate) fn spawn_traced<Work>(future: Work) -> JoinHandle<Work::Output>
where
    Work: Future + Send + 'static,
    Work::Output: Send + 'static,
{
    tokio::spawn(future.with_context(Context::current()))
}

/// Spawns `future` without the caller's trace context.
///
/// Use this for background work that records no spans, or that carries its
/// own root span created before spawning.
pub(crate) fn spawn_detached<Work>(future: Work) -> JoinHandle<Work::Output>
where
    Work: Future + Send + 'static,
    Work::Output: Send + 'static,
{
    tokio::spawn(future)
}

/// Runs blocking `work` on Tokio's blocking pool without the caller's trace
/// context.
pub(crate) fn spawn_blocking_detached<Work, Output>(work: Work) -> JoinHandle<Output>
where
    Work: FnOnce() -> Output + Send + 'static,
    Output: Send + 'static,
{
    tokio::task::spawn_blocking(work)
}

#[cfg(test)]
#[path = "spawn_test.rs"]
mod tests;
