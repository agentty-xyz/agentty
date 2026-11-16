//! Per-run limits that stop a run before it spends more model requests,
//! tool calls, tokens, or wall-clock time than its host allows.

use std::fmt;
use std::time::Duration;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

/// Bounds on what one run may spend; every unset limit is unlimited.
///
/// Limits apply to the run they are set on only and are never stored as a
/// session default. A run that reaches one fails with
/// [`crate::ErrorKind::BudgetExhausted`] naming the [`LimitKind`]; its turn
/// is recorded as failed like any other failure, its outcome keeps the usage
/// of every completed model request, and the session accepts the next run.
/// A limit of zero stops the run before its first model request or tool call.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[non_exhaustive]
pub struct RunLimits {
    /// Model requests the run may send, counting re-sent requests and
    /// context summaries. Checked before each request of the run's
    /// conversation, so a context summary started below the limit may still
    /// re-request an invalid summary.
    pub max_rounds: Option<u32>,
    /// Tool calls the run may execute. A response whose calls would exceed
    /// the limit stops the run before any of them runs.
    pub max_tool_calls: Option<u32>,
    /// Input plus output tokens the provider reported for the run's
    /// completed model requests. Checked before each request against the
    /// usage already known, so the last request may overshoot the limit.
    pub max_total_tokens: Option<u64>,
    /// Wall-clock time from the start of the run's turn, after admission.
    /// Reaching it stops the run at once, also while it awaits a model
    /// request or a tool call, which are then dropped as on cancellation.
    pub max_wall_time: Option<Duration>,
}

impl RunLimits {
    /// Limits the model requests the run may send.
    #[must_use]
    pub fn with_max_rounds(mut self, rounds: u32) -> Self {
        self.max_rounds = Some(rounds);

        self
    }

    /// Limits the tool calls the run may execute.
    #[must_use]
    pub fn with_max_tool_calls(mut self, tool_calls: u32) -> Self {
        self.max_tool_calls = Some(tool_calls);

        self
    }

    /// Limits the input plus output tokens the run's model requests may use.
    #[must_use]
    pub fn with_max_total_tokens(mut self, tokens: u64) -> Self {
        self.max_total_tokens = Some(tokens);

        self
    }

    /// Limits the run's wall-clock time.
    #[must_use]
    pub fn with_max_wall_time(mut self, wall_time: Duration) -> Self {
        self.max_wall_time = Some(wall_time);

        self
    }

    /// Checks whether a run that sent `rounds` model requests using `tokens`
    /// may send another.
    ///
    /// # Errors
    /// Returns the limit the next request would exceed.
    pub(crate) fn check_request(self, rounds: u64, tokens: u64) -> Result<(), LimitKind> {
        if self
            .max_rounds
            .is_some_and(|max_rounds| rounds >= u64::from(max_rounds))
        {
            return Err(LimitKind::Rounds);
        }
        if self
            .max_total_tokens
            .is_some_and(|max_tokens| tokens >= max_tokens)
        {
            return Err(LimitKind::Tokens);
        }

        Ok(())
    }

    /// Checks whether a run that executed `executed` tool calls may execute
    /// `requested` more.
    ///
    /// # Errors
    /// Returns [`LimitKind::ToolCalls`] when they would exceed the limit.
    pub(crate) fn check_tool_calls(
        self,
        executed: usize,
        requested: usize,
    ) -> Result<(), LimitKind> {
        let allowed = self.max_tool_calls.map_or(usize::MAX, |max_calls| {
            usize::try_from(max_calls).unwrap_or(usize::MAX)
        });
        if executed.saturating_add(requested) > allowed {
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

/// Which [`RunLimits`] limit stopped a run.
///
/// It serializes as its `snake_case` name.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum LimitKind {
    /// [`RunLimits::max_rounds`]: model requests.
    Rounds,
    /// [`RunLimits::max_tool_calls`]: tool calls.
    ToolCalls,
    /// [`RunLimits::max_total_tokens`]: input plus output tokens.
    Tokens,
    /// [`RunLimits::max_wall_time`]: wall-clock time.
    WallTime,
}

impl LimitKind {
    /// Returns the stable `snake_case` name of this limit.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rounds => "rounds",
            Self::ToolCalls => "tool_calls",
            Self::Tokens => "tokens",
            Self::WallTime => "wall_time",
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
