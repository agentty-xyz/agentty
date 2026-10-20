//! Explicit role assignments and inputs for isolated task execution.

use serde::{Deserialize, Serialize};

use crate::{
    AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility, EnvId, SessionError,
    SessionId,
};

/// Immutable role and provider/model selection assigned to one task.
///
/// A caller captures the selection once, before admission. A future chain can
/// reuse that selection across its tasks without reading mutable session
/// settings again. Permissions still resolve from the role and session at
/// execution time; assignment never grants access.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TaskAgentAssignment {
    /// Captured provider/model pair, independent of the role.
    pub agent: AgentSelection,
    /// Instructions and permission restriction used for every attempt.
    pub definition: BuiltinAgentDefinition,
}

/// Durable identity of one task's conversation and runtime resources.
///
/// The host allocates `id` independently of caller-supplied task IDs and
/// provider conversation IDs. It remains stable across attempts and process
/// restarts. Provider continuation and replay history belong to this context,
/// never the owning session's ordinary conversation. The session still owns
/// scheduling, cancellation, and the shared workspace.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentContext {
    /// Immutable role and captured model selection.
    pub assignment: TaskAgentAssignment,
    /// Host-allocated stable execution identity, not a provider conversation
    /// ID.
    pub id: String,
    /// Owning session; task completion does not release its environment.
    pub session_id: SessionId,
    /// Task whose attempts may continue this context.
    pub task_id: String,
}

impl TaskAgentContext {
    /// Returns the session-owned environment shared by this context's attempts.
    pub fn env_id(&self) -> EnvId {
        EnvId::SessionWorktree(self.session_id.clone())
    }
}

/// Caller-supplied, explicitly accepted plan for an implementation attempt.
///
/// The producing task and attempt identify the exact plan version. This is
/// input provenance, not proof of acceptance: the host must verify acceptance
/// before dispatch. The plan is task data, never application policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct AcceptedTaskPlan {
    /// Attempt that produced this exact plan.
    pub attempt_id: String,
    /// Task that owns the producing attempt.
    pub task_id: String,
    /// Accepted plan body supplied explicitly to the implementer.
    pub text: String,
}

/// Explicit input for a task's own conversation, excluding session replay.
///
/// Continuations retain this task's history only. The host must not copy the
/// human transcript or another task's provider conversation into this input.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct TaskAgentInput {
    /// Exact accepted plan required by an implementer; absent for a planner.
    pub accepted_plan: Option<AcceptedTaskPlan>,
    /// Clarification or retry guidance explicitly addressed to this task.
    pub clarification: Option<String>,
    /// Original user request, retained across this task's attempts.
    pub user_request: String,
}

impl TaskAgentInput {
    /// Validates required role inputs without asserting plan acceptance.
    ///
    /// # Errors
    /// Returns an operation error for an empty request, missing implementer
    /// plan, incomplete plan provenance, or a plan supplied to a planner.
    pub fn validate(&self, definition: BuiltinAgentDefinition) -> Result<(), SessionError> {
        if self.user_request.trim().is_empty() {
            return Err(SessionError::Operation(
                "Task user request is empty".to_string(),
            ));
        }

        match (definition, &self.accepted_plan) {
            (BuiltinAgentDefinition::Planner, None) => Ok(()),
            (BuiltinAgentDefinition::Planner, Some(_)) => Err(SessionError::Operation(
                "Planner input must not contain an accepted plan".to_string(),
            )),
            (BuiltinAgentDefinition::Implementer, None) => Err(SessionError::Operation(
                "Implementer input requires an accepted plan".to_string(),
            )),
            (BuiltinAgentDefinition::Implementer, Some(plan)) => {
                if plan.task_id.trim().is_empty()
                    || plan.attempt_id.trim().is_empty()
                    || plan.text.trim().is_empty()
                {
                    return Err(SessionError::Operation(
                        "Accepted plan requires a task, producing attempt, and text".to_string(),
                    ));
                }

                Ok(())
            }
        }
    }
}

/// Explicit role-aware attempt, separate from legacy session-context tasks.
///
/// Identical redelivery includes the assignment and structured input. Once
/// admitted, the task's role, selection, original request, and accepted plan
/// are immutable; only clarification may change in a new attempt. A host must
/// reject conflicting assignments rather than changing an existing task's
/// conversation or falling back to ordinary chat.
///
/// These contracts alone do not enable submission or dispatch.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SubmitAgentTaskRequest {
    /// Assignment captured before admission and retained across attempts.
    pub assignment: TaskAgentAssignment,
    /// Globally unique delivery identity; retries and continuations use new
    /// IDs.
    pub attempt_id: String,
    /// Role-specific input supplied to the isolated conversation.
    pub input: TaskAgentInput,
    /// Desired outcome, immutable across this task's attempts.
    pub objective: String,
    /// Globally unique desired-outcome identity.
    pub task_id: String,
    /// Whether the explicit input is shown in the human transcript.
    pub visibility: DurableMessageVisibility,
}

/// One response retained in an isolated task conversation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentTurn {
    /// Exact serialized protocol response, including clarification questions.
    pub assistant_message: String,
    /// Producing attempt; consult its durable status before accepting output.
    pub attempt_id: String,
    /// Explicit input supplied to this attempt.
    pub input: TaskAgentInput,
    /// Input tokens charged to the captured model.
    pub input_tokens: u64,
    /// Output tokens charged to the captured model.
    pub output_tokens: u64,
}

/// Durable task conversation, independent of ordinary session messages.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskAgentHistory {
    /// Stable assignment and runtime identity shared by these attempts.
    pub context: TaskAgentContext,
    /// Original task input without attempt-specific clarification.
    pub input: TaskAgentInput,
    /// Responses in admitted attempt order; absence is not proof of failure.
    pub turns: Vec<TaskAgentTurn>,
}

#[cfg(test)]
#[path = "task_execution_test.rs"]
mod tests;
