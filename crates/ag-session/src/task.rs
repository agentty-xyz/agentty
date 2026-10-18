//! Desired outcomes and explicit, single-turn attempts to advance them.

use crate::{DurableMessageVisibility, SessionId};

/// A desired outcome to persist without admitting an attempt or starting work.
///
/// Repeating the same identity, session, and objective is idempotent, even
/// after completion. The owning session and objective cannot be changed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CreateTaskRequest {
    /// Nonempty desired outcome, immutable across attempts.
    pub objective: String,
    /// Globally unique, nonempty identity of the desired outcome.
    pub task_id: String,
}

/// An explicit attempt to advance a task in an existing idle session.
///
/// Re-deliver the identical request with the same `attempt_id` to acknowledge
/// an existing attempt, even after failure. Use a new ID to retry or continue
/// after clarification. Provider retries stay inside the same attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitTaskRequest {
    /// Globally unique delivery identity for this attempt.
    pub attempt_id: String,
    /// Prompt for this attempt, including any clarification or retry context.
    pub message: String,
    /// Desired outcome, immutable across attempts of the same task.
    pub objective: String,
    /// Stable identity of the desired outcome.
    pub task_id: String,
    /// Whether the attempt prompt appears in the human transcript.
    pub visibility: DurableMessageVisibility,
}

impl SubmitTaskRequest {
    /// Operation identity reserved for this attempt's ordered workflow.
    pub fn operation_id(&self) -> String {
        format!("task-attempt:{}", self.attempt_id)
    }
}

/// A desired outcome with retained attempt history, scoped to one session.
///
/// Successful execution does not establish that the objective is satisfied.
/// Only explicit completion sets `completed_at`. Clarification, cancellation,
/// and failed or interrupted attempts leave the task open.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Task {
    /// Attempts in admission order; execution status comes from operations.
    pub attempts: Vec<TaskAttempt>,
    /// Time the caller explicitly accepted the outcome, if complete.
    pub completed_at: Option<i64>,
    /// Time the task was created, possibly before its first attempt.
    pub created_at: i64,
    /// Stable task identity.
    pub id: String,
    /// Desired outcome, separate from an individual attempt's prompt.
    pub objective: String,
    /// Conversation and workspace used by every attempt.
    pub session_id: SessionId,
}

/// One turn and its ordered post-processing, backed by a unique operation.
///
/// A clarification response finishes the attempt normally and leaves the
/// session waiting for input. It does not keep a worker slot occupied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAttempt {
    /// Time execution and post-processing settled, if terminal.
    pub finished_at: Option<i64>,
    /// Stable attempt delivery identity.
    pub id: String,
    /// Execution or recovery failure, if any.
    pub last_error: Option<String>,
    /// Prompt admitted for this attempt.
    pub message: String,
    /// Unique operation; never reused for another attempt.
    pub operation_id: String,
    /// Time the attempt was durably admitted.
    pub queued_at: i64,
    /// Time the worker started execution, if started.
    pub started_at: Option<i64>,
    /// Execution state projected from the operation, not task completion.
    pub status: TaskAttemptStatus,
    /// Transcript presentation captured at admission.
    pub visibility: DurableMessageVisibility,
}

/// Operation lifecycle exposed by the task read model.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskAttemptStatus {
    /// Durably admitted, awaiting worker execution.
    Queued,
    /// Executing a turn or its ordered post-processing.
    Running,
    /// Workflow finished successfully, possibly requesting clarification.
    Succeeded,
    /// Workflow failed, including interruption by application restart.
    Failed,
    /// Workflow was canceled before or during execution.
    Canceled,
}
