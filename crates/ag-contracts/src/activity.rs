//! Provider-observed activity for display, independent of telemetry capture.

use std::time::SystemTime;

/// A bounded display snapshot of one observed provider operation.
///
/// Match updates by `(attempt_id, id)`. Retries use new attempt identities.
/// Names describe tools or explicitly invoked skills, never inferred file
/// reads. Optional input and output previews are display-only: they must not
/// be replayed to providers or implicitly exported as telemetry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityEvent {
    /// Unique provider attempt identity, including retries and repairs.
    pub attempt_id: String,
    /// Process exit code when the provider reports one.
    pub exit_code: Option<i64>,
    /// Provider operation identity within the attempt.
    pub id: String,
    /// Bounded command or argument preview, when reported by the provider.
    pub input: Option<String>,
    /// Provider-independent operation category.
    pub kind: ActivityKind,
    /// Bounded tool or skill identity suitable for display.
    pub name: String,
    /// Time this snapshot was observed by the adapter.
    pub observed_at: SystemTime,
    /// Bounded result preview, updated as output arrives when supported.
    pub output: Option<String>,
    /// Provider parent operation identity, scoped to the same attempt.
    pub parent_id: Option<String>,
    /// Latest observed lifecycle state.
    pub status: ActivityStatus,
}

/// Category of explicitly observed activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityKind {
    /// Command execution.
    Command,
    /// File modification.
    FileChange,
    /// Named skill invocation.
    Skill,
    /// Built-in, dynamic, MCP, or subagent tool invocation.
    Tool,
    /// Provider web search.
    WebSearch,
}

/// Lifecycle state for an observed operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityStatus {
    /// Provider reported that the operation started.
    Running,
    /// Provider reported successful completion.
    Completed,
    /// Provider reported failure.
    Failed,
    /// Attempt ended without an observed terminal result, including
    /// cancellation.
    Interrupted,
}
