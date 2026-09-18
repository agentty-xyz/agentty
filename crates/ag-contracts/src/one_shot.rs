use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ag_protocol::AgentResponse;
use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use crate::{AgentRequestKind, PermissionMode, ReasoningLevel, SessionStats, SpeedMode};

/// Input payload for one isolated prompt that prefers structured protocol
/// output.
#[derive(Clone, Debug)]
pub struct OneShotRequest {
    /// Optional PID slot for resource accounting while a prompt is running.
    /// Subprocess lifetime and cancellation belong to the runtime adapter.
    pub child_pid: Option<Arc<Mutex<Option<u32>>>>,
    /// Working directory where the prompt command runs.
    pub folder: PathBuf,
    /// Harness identifier resolved by the host-selected runtime implementation.
    pub harness: String,
    /// Provider-specific model used for command construction and parsing.
    pub model: String,
    /// Filesystem and command permission policy for this isolated prompt.
    pub permission_mode: PermissionMode,
    /// Prompt text submitted to the agent.
    pub prompt: String,
    /// Optional shared limit, charged for every provider turn including
    /// repairs.
    pub provider_call_budget: Option<crate::ProviderCallBudget>,
    /// Reasoning effort preference for the one-shot prompt.
    pub reasoning_level: ReasoningLevel,
    /// Canonical request kind for this isolated prompt.
    pub request_kind: AgentRequestKind,
    /// Response-speed preference for the one-shot prompt.
    pub speed_mode: SpeedMode,
}

/// Parsed result returned by one isolated prompt execution.
#[derive(Clone, Debug, PartialEq)]
pub struct OneShotSubmission {
    /// Structured protocol response parsed from the final successful attempt.
    pub response: AgentResponse,
    /// Aggregated token usage for the one-shot prompt execution.
    pub stats: SessionStats,
}

/// Typed failure returned by [`OneShotClient`] submissions.
///
/// The concrete transport, protocol-repair, and provider diagnostics remain
/// available through [`std::fmt::Display`] without exposing transport-specific
/// variants to callers.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct OneShotError {
    message: String,
}

impl OneShotError {
    /// Creates an error from one already formatted submission diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

/// Provider-neutral boundary for isolated structured agent prompts.
///
/// Implementations own transport selection, protocol repair, temporary
/// app-server lifecycle, and usage aggregation so callers submit one request
/// without selecting a CLI or app-server execution helper.
#[cfg_attr(any(test, feature = "test-utils"), mockall::automock)]
#[async_trait]
pub trait OneShotClient: Send + Sync {
    /// Escalates application shutdown after its graceful deadline. This must
    /// return immediately and permanently stop detached submission tasks.
    /// The owner also drops in-flight submission futures; adapters whose
    /// resources are owned entirely by those futures need no extra action.
    fn force_shutdown(&self) {}

    /// Executes one isolated prompt and returns its parsed response and usage.
    /// Dropping this future must initiate cleanup of owned provider resources.
    /// Use `submit_cancellable` to wait for asynchronous cleanup on
    /// cancellation. Implementations must enforce `provider_call_budget`
    /// for every underlying provider attempt, including protocol repairs
    /// and transport retries.
    async fn submit(&self, request: OneShotRequest) -> Result<OneShotSubmission, OneShotError>;

    /// Cancels an isolated prompt and waits for adapter cleanup before
    /// returning. The default drops `submit`, suitable for transports with
    /// synchronous drop cleanup. Adapters requiring asynchronous shutdown
    /// must override this method.
    async fn submit_cancellable(
        &self,
        request: OneShotRequest,
        cancellation: CancellationToken,
    ) -> Result<OneShotSubmission, OneShotError> {
        tokio::select! {
            biased;
            () = cancellation.cancelled() => Err(OneShotError::new("[Stopped] Agent run canceled")),
            result = self.submit(request) => result,
        }
    }

    /// Releases retained provider resources after all submissions and their
    /// asynchronous cancellation cleanup have finished. Stateless adapters need
    /// no cleanup.
    async fn close(&self) {}
}

#[cfg(test)]
#[path = "one_shot_test.rs"]
mod tests;
