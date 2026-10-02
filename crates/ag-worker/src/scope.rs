use std::future::{Future, poll_fn};
use std::sync::Arc;
use std::task::Poll;

use ag_contracts::{OneShotError, OneShotRequest, OneShotSubmission};
use ag_telemetry::{CaptureToolContent, Context, FutureExt as _, TraceContextExt as _};
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::RunClient;

tokio::task_local! {
    static CURRENT: RunContext;
}

/// Ownership inherited by child model calls, independently of queue ordering.
#[derive(Clone, Default)]
pub struct RunScope {
    /// Cancellation source added by this scope; enclosing tokens remain active.
    pub cancellation: Option<CancellationToken>,
    /// Enclosing workflow operation or logical run.
    pub parent_id: Option<String>,
    /// Owning project, including work outside a session.
    pub project_id: Option<i64>,
    /// Human-readable execution purpose.
    pub purpose: Option<String>,
    /// Owning session, when applicable.
    pub session_id: Option<String>,
}

/// Captured ownership and every independently cancelable enclosing scope.
#[derive(Clone, Default)]
pub(crate) struct RunContext {
    pub(crate) scope: RunScope,
    pub(crate) trace: Context,
    cancellations: Vec<CancellationToken>,
}

impl RunContext {
    fn new(scope: RunScope) -> Self {
        Self {
            cancellations: scope.cancellation.iter().cloned().collect(),
            scope,
            trace: Context::current(),
        }
    }

    pub(crate) fn current() -> Self {
        CURRENT
            .try_with(Clone::clone)
            .unwrap_or_else(|_| Self::new(RunScope::default()))
    }

    fn inherited(mut self) -> Self {
        let parent = Self::current();
        let capture_policy = self
            .trace
            .get::<CaptureToolContent>()
            .or_else(|| parent.trace.get::<CaptureToolContent>())
            .copied();
        if !self.trace.span().span_context().is_valid() {
            self.trace = parent.trace;
        }
        if let Some(policy) = capture_policy {
            self.trace = self.trace.with_value(policy);
        }
        self.cancellations.extend(parent.cancellations);
        self.scope = RunScope {
            cancellation: self.scope.cancellation.or(parent.scope.cancellation),
            parent_id: self.scope.parent_id.or(parent.scope.parent_id),
            project_id: self.scope.project_id.or(parent.scope.project_id),
            purpose: self.scope.purpose.or(parent.scope.purpose),
            session_id: self.scope.session_id.or(parent.scope.session_id),
        };

        self
    }

    /// Waits for any enclosing token without spawning propagation tasks.
    pub(crate) async fn cancelled(&self) {
        let mut cancellations: Vec<_> = self
            .cancellations
            .iter()
            .map(|token| Box::pin(token.cancelled()))
            .collect();
        poll_fn(|context| {
            if cancellations
                .iter_mut()
                .any(|future| future.as_mut().poll(context).is_ready())
            {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        })
        .await;
    }
}

/// Executes host work with inherited run ownership. Spawned tasks must carry a
/// scoped client explicitly; Tokio task-local values do not cross task spawns.
pub async fn in_scope<T>(scope: RunScope, work: impl Future<Output = T>) -> T {
    let context = RunContext::new(scope).inherited();
    CURRENT
        .scope(context.clone(), work.with_context(context.trace))
        .await
}

/// Captures ownership for a utility client passed to another task or workflow.
/// Child submissions execute directly under worker supervision, never behind
/// a waiting parent in the session command queue.
/// Clients captured without a span inherit the submitting scope's span.
/// An explicit captured tool-content policy is retained independently;
/// otherwise the submitting scope supplies it.
pub fn scoped_client(client: Arc<dyn RunClient>, scope: RunScope) -> Arc<dyn RunClient> {
    Arc::new(ScopedClient {
        client,
        context: RunContext::new(scope).inherited(),
    })
}

struct ScopedClient {
    client: Arc<dyn RunClient>,
    context: RunContext,
}

#[async_trait]
impl RunClient for ScopedClient {
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError> {
        let context = self.context.clone().inherited();
        CURRENT
            .scope(
                context.clone(),
                self.client.submit(request).with_context(context.trace),
            )
            .await
    }
}
