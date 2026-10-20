//! A static Planner, explicit acceptance, and Implementer workflow.

use crate::{
    AcceptedTaskPlan, AgentSelection, BuiltinAgentDefinition, DurableMessageVisibility, SessionId,
    SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentInput,
};

/// Idempotent launch input captured before the first Planner attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartPlanThenImplementRequest {
    /// Provider/model captured for both roles, independent of later settings.
    pub agent: AgentSelection,
    /// Globally unique workflow identity, retained for launch redelivery.
    pub chain_id: String,
    /// Globally unique initial Planner delivery identity.
    pub planner_attempt_id: String,
    /// Immutable original objective shared by both isolated task contexts.
    pub user_request: String,
}

impl StartPlanThenImplementRequest {
    /// Builds the exact first role request without consulting session state.
    pub fn planner_request(&self) -> SubmitAgentTaskRequest {
        SubmitAgentTaskRequest {
            assignment: TaskAgentAssignment {
                agent: self.agent,
                definition: BuiltinAgentDefinition::Planner,
            },
            attempt_id: self.planner_attempt_id.clone(),
            input: TaskAgentInput {
                accepted_plan: None,
                clarification: None,
                user_request: self.user_request.clone(),
            },
            objective: self.user_request.clone(),
            task_id: format!("chain:{}:planner", self.chain_id),
            visibility: DurableMessageVisibility::Hidden,
        }
    }

    /// Identifies the separate Implementer task reserved by this workflow.
    pub fn implementer_task_id(&self) -> String {
        format!("chain:{}:implementer", self.chain_id)
    }
}

/// Explicit acceptance and initial implementation admission in one action.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptTaskChainPlanRequest {
    /// New delivery identity for the first Implementer attempt.
    pub attempt_id: String,
    /// Exact latest Planner version selected by the user.
    pub plan_attempt_id: String,
}

/// Explicit revision, clarification, or retry of the current workflow stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinueTaskChainRequest {
    /// New delivery identity; old deliveries never restart or uncancel work.
    pub attempt_id: String,
    /// User guidance addressed only to the current role's own context.
    pub clarification: Option<String>,
    /// Current attempt observed when this guidance was authored. Admission
    /// rejects a new delivery if the workflow has advanced since that snapshot.
    /// Identical redelivery retains its original anchor after later progress.
    pub expected_attempt_id: String,
}

/// Natural-language user reply addressed to a session's latest workflow.
///
/// The host decides what the reply means: plan approval, plan revision, or
/// guidance for the current role. No words are reserved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplyToTaskChainRequest {
    /// User-authored reply, recorded once as a visible session prompt.
    pub message: String,
    /// Globally unique delivery identity. Redelivery returns the original
    /// outcome without recording, deciding, or admitting work again.
    pub reply_id: String,
}

/// Workflow action admitted for one natural-language reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskChainReplyOutcome {
    /// The reply clearly approved the plan; the Implementer was admitted.
    PlanAccepted,
    /// The reply was delivered to the Planner as plan revision guidance.
    PlanRevised,
    /// The reply was delivered to the current role as an answer or retry.
    Continued,
}

impl TaskChainReplyOutcome {
    /// Every outcome, in the order hosts look up delivery receipts.
    pub const ALL: [Self; 3] = [Self::PlanAccepted, Self::PlanRevised, Self::Continued];

    /// Derives the attempt identity admitted when one reply has this outcome.
    pub fn attempt_id(self, reply_id: &str) -> String {
        let action = match self {
            Self::PlanAccepted => "implement",
            Self::PlanRevised => "revise",
            Self::Continued => "continue",
        };

        format!("reply:{reply_id}:{action}")
    }
}

/// Consistent durable workflow snapshot; reading never dispatches work.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskChain {
    /// Exact immutable accepted version, present after explicit handoff.
    pub accepted_plan: Option<AcceptedTaskPlan>,
    /// Shared captured provider/model selection.
    pub agent: AgentSelection,
    /// Exact latest admitted request for the current stage, including IDs.
    pub current_request: SubmitAgentTaskRequest,
    /// Stable workflow identity.
    pub id: String,
    /// Separate Implementer task, reserved at launch.
    pub implementer_task_id: String,
    /// Projection from durable stage selection and operation-backed attempts.
    pub phase: TaskChainPhase,
    /// Separate Planner task, retained after implementation begins.
    pub planner_task_id: String,
    /// Session owning the shared workspace and serialized worker.
    pub session_id: SessionId,
    /// Immutable objective captured at launch.
    pub user_request: String,
}

impl TaskChain {
    /// Builds a new delivery to the current role while retaining its inputs.
    pub fn continuation_request(
        &self,
        request: &ContinueTaskChainRequest,
    ) -> SubmitAgentTaskRequest {
        SubmitAgentTaskRequest {
            attempt_id: request.attempt_id.clone(),
            input: TaskAgentInput {
                clarification: request.clarification.clone(),
                ..self.current_request.input.clone()
            },
            ..self.current_request.clone()
        }
    }
}

/// Workflow presentation derived from the current attempt and saved response.
/// Success records execution, never implicit acceptance of a plan or outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskChainPhase {
    /// Planner attempt is queued or executing, including post-processing.
    Planning,
    /// Latest successful Planner response needs explicit user acceptance.
    AwaitingPlanAcceptance,
    /// Planner has questions that need an explicit continuation.
    PlannerQuestion,
    /// Accepted plan is being implemented, including post-processing.
    Implementing,
    /// Implementer has questions that need an explicit continuation.
    ImplementerQuestion,
    /// Current stage failed or was interrupted; retry is always explicit.
    Failed,
    /// Cancellation requested; an explicit new attempt may retry the stage.
    Canceled,
    /// Implementer finished successfully; the session remains reviewable.
    Succeeded,
}

#[cfg(test)]
#[path = "task_chain_test.rs"]
mod tests;
