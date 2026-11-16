//! Per-run limits that stop a run before it spends more model requests,
//! tool calls, tokens, or wall-clock time than its host allows.

use std::fmt;
use std::num::{NonZeroU32, NonZeroU64};
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

/// Bounds on what one run may spend; every unset limit is unlimited.
///
/// Limits apply to the run they are set on only and are never stored as a
/// session default. A run that reaches one fails with
/// [`crate::ErrorKind::BudgetExhausted`] naming the [`LimitKind`]; its turn
/// is recorded as failed like any other failure, or as interrupted when the
/// wall-clock limit drops a running tool call, its outcome keeps the usage
/// of every completed model request, and the session accepts the next run.
/// Counted limits are nonzero; to run without tools, use
/// [`crate::ToolPolicy`].
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct RunLimits {
    /// Model requests the run may send, counting re-sent requests and
    /// context summaries. Checked before each request of the run's
    /// conversation, so one context summary started below the limit may
    /// still send up to three requests past it.
    pub max_model_requests: Option<NonZeroU32>,
    /// Tool calls the model may request, counting calls that never run,
    /// such as calls to denied or unknown tools. A response whose calls
    /// would exceed the limit stops the run before any of them runs.
    pub max_tool_calls: Option<NonZeroU32>,
    /// Input (cached input included) plus output tokens the provider
    /// reported for the run's completed model requests. Checked before each
    /// request against the usage already known, so the last request, or one
    /// context summary, may overshoot the limit. Requests whose provider
    /// reports no usage count as zero.
    pub max_total_tokens: Option<NonZeroU64>,
    /// Wall-clock time from the start of the run's turn, which excludes
    /// waiting for the session to become free. Reaching it stops the run at
    /// once, also while it awaits a model request or a tool call, which are
    /// then dropped as on cancellation. A zero duration is rejected with
    /// [`crate::ErrorKind::Config`] before the run starts.
    pub max_wall_time: Option<Duration>,
}

impl RunLimits {
    /// Limits the model requests the run may send.
    #[must_use]
    pub fn with_max_model_requests(mut self, requests: NonZeroU32) -> Self {
        self.max_model_requests = Some(requests);

        self
    }

    /// Limits the tool calls the model may request.
    #[must_use]
    pub fn with_max_tool_calls(mut self, tool_calls: NonZeroU32) -> Self {
        self.max_tool_calls = Some(tool_calls);

        self
    }

    /// Limits the input plus output tokens the run's model requests may use.
    #[must_use]
    pub fn with_max_total_tokens(mut self, tokens: NonZeroU64) -> Self {
        self.max_total_tokens = Some(tokens);

        self
    }

    /// Limits the run's wall-clock time.
    #[must_use]
    pub fn with_max_wall_time(mut self, wall_time: Duration) -> Self {
        self.max_wall_time = Some(wall_time);

        self
    }

    /// Returns whether a limit is set to a value no run could satisfy.
    pub(crate) fn is_unsatisfiable(self) -> bool {
        self.max_wall_time == Some(Duration::ZERO)
    }

    /// Checks whether a run that sent `requests` model requests using
    /// `tokens` may send another.
    ///
    /// # Errors
    /// Returns the limit the next request would exceed.
    pub(crate) fn check_request(self, requests: u64, tokens: u64) -> Result<(), LimitKind> {
        if self
            .max_model_requests
            .is_some_and(|max_requests| requests >= u64::from(max_requests.get()))
        {
            return Err(LimitKind::ModelRequests);
        }
        if self
            .max_total_tokens
            .is_some_and(|max_tokens| tokens >= max_tokens.get())
        {
            return Err(LimitKind::Tokens);
        }

        Ok(())
    }

    /// Checks whether a run whose model requested `requested` tool calls
    /// may request `more`.
    ///
    /// # Errors
    /// Returns [`LimitKind::ToolCalls`] when they would exceed the limit.
    pub(crate) fn check_tool_calls(self, requested: usize, more: usize) -> Result<(), LimitKind> {
        let allowed = self.max_tool_calls.map_or(usize::MAX, |max_calls| {
            usize::try_from(max_calls.get()).unwrap_or(usize::MAX)
        });
        if requested.saturating_add(more) > allowed {
            return Err(LimitKind::ToolCalls);
        }

        Ok(())
    }

    /// Returns when a run starting now reaches its wall-clock limit, if it
    /// has one that the clock can represent.
    pub(crate) fn deadline(self) -> Option<Instant> {
        self.max_wall_time
            .and_then(|wall_time| Instant::now().checked_add(wall_time))
    }
}

/// Which limit stopped a run: one of its [`RunLimits`], or a cap of the
/// budget ledger that refused its next model request.
///
/// It serializes as its `snake_case` name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LimitKind {
    /// [`RunLimits::max_model_requests`]: model requests.
    ModelRequests,
    /// [`RunLimits::max_tool_calls`]: tool calls.
    ToolCalls,
    /// [`RunLimits::max_total_tokens`] or a ledger's token cap: input plus
    /// output tokens.
    Tokens,
    /// [`RunLimits::max_wall_time`]: wall-clock time.
    WallTime,
    /// A ledger's cost cap: micro-USD.
    Cost,
}

impl LimitKind {
    /// Returns the stable `snake_case` name of this limit.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ModelRequests => "model_requests",
            Self::ToolCalls => "tool_calls",
            Self::Tokens => "tokens",
            Self::WallTime => "wall_time",
            Self::Cost => "cost",
        }
    }
}

impl fmt::Display for LimitKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
#[path = "limit_test.rs"]
mod tests;
