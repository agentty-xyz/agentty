//! The static Planner → accepted plan → Implementer execution graph.
//!
//! A task chain is a fixed two-node [`ExecutionGraph`]. Graph storage and the
//! static executor own admission, gates, exact handoff, and recovery; this
//! module only builds that graph and projects its progress into workflow
//! phases.

use std::fmt::Write as _;
use std::num::NonZeroU64;

use ag_protocol::{ProtocolRequestProfile, TurnPrompt, TurnPromptAttachment};
use serde::{Deserialize, Serialize};

use crate::{
    AcceptedTaskPlan, AgentSelection, EnvId, ExecutionGraph, ExecutionGraphProgress,
    ExecutionGraphStatus, GraphCompletionGate, GraphContext, GraphDependency,
    GraphExecutionProfile, GraphExecutionRef, GraphInputBinding, GraphNode, GraphNodeProgress,
    GraphNodeStatus, GraphOutput, GraphPostProcessing, GraphWorkspaceAccess, SessionId,
    TaskAgentRole,
};

/// Named Planner output that the Implementer consumes after acceptance.
pub const TASK_CHAIN_PLAN_OUTPUT: &str = "plan";

/// Idempotent launch input captured before the first Planner execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StartPlanThenImplementRequest {
    /// Provider/model captured for both roles, independent of later settings.
    pub agent: AgentSelection,
    /// Resolved definitions captured together before planning starts.
    pub agents: Box<PlanThenImplementAgents>,
    /// Images accompanying the original request.
    pub attachments: Vec<TurnPromptAttachment>,
    /// Globally unique workflow and execution-graph identity, retained for
    /// launch redelivery.
    pub chain_id: String,
    /// Immutable original objective shared by both isolated task contexts.
    pub user_request: String,
}

impl StartPlanThenImplementRequest {
    /// Builds the immutable graph without consulting session state.
    ///
    /// The Planner is a shared reader whose validated `plan` output must be
    /// explicitly accepted before the exclusive-writer Implementer starts with
    /// that exact version bound as input. `launch_conversation` is the host's
    /// snapshot of the session conversation before launch; it is captured as
    /// historical data in the Planner's request only.
    pub fn graph(
        &self,
        session_id: &SessionId,
        launch_conversation: Option<&str>,
    ) -> ExecutionGraph {
        let planner = self.node(
            session_id,
            TaskAgentRole::Planner,
            self.task_input(launch_conversation),
        );
        let mut implementer = self.node(
            session_id,
            TaskAgentRole::Implementer,
            self.task_input(None),
        );
        implementer.dependencies.push(GraphDependency {
            gate: GraphCompletionGate::ResultAccepted {
                output_name: TASK_CHAIN_PLAN_OUTPUT.into(),
            },
            node_id: planner.id.clone(),
        });
        implementer.inputs.push(GraphInputBinding {
            name: "accepted_plan".into(),
            output_name: TASK_CHAIN_PLAN_OUTPUT.into(),
            producer_node_id: planner.id.clone(),
        });

        ExecutionGraph {
            id: self.chain_id.clone(),
            nodes: vec![planner, implementer],
            revision: NonZeroU64::MIN,
            session_id: session_id.clone(),
        }
    }

    fn node(&self, session_id: &SessionId, role: TaskAgentRole, request: TurnPrompt) -> GraphNode {
        let (definition, execution, outputs) = match role {
            TaskAgentRole::Planner => (
                self.agents.planner.clone(),
                GraphExecutionProfile {
                    context: GraphContext::TaskConversation {
                        task_id: TaskChain::task_id(&self.chain_id, role),
                    },
                    post_processing: GraphPostProcessing::None,
                    response_profile: ProtocolRequestProfile::SessionTurn,
                    shared_budget_id: None,
                    workspace_access: GraphWorkspaceAccess::SharedRead,
                },
                vec![GraphOutput {
                    name: TASK_CHAIN_PLAN_OUTPUT.into(),
                    schema: ProtocolRequestProfile::SessionTurn,
                }],
            ),
            TaskAgentRole::Implementer => (
                self.agents.implementer.clone(),
                GraphExecutionProfile {
                    context: GraphContext::TaskConversation {
                        task_id: TaskChain::task_id(&self.chain_id, role),
                    },
                    post_processing: GraphPostProcessing::SessionWorkflow,
                    response_profile: ProtocolRequestProfile::SessionTurn,
                    shared_budget_id: None,
                    workspace_access: GraphWorkspaceAccess::ExclusiveWrite,
                },
                Vec::new(),
            ),
        };

        GraphNode {
            agent: self.agent,
            definition,
            dependencies: Vec::new(),
            env_id: EnvId::SessionWorktree(session_id.clone()),
            execution,
            id: role.id().into(),
            inputs: Vec::new(),
            objective: self.user_request.clone(),
            outputs,
            request,
        }
    }

    /// Encodes the original request as data, keeping its images attached.
    fn task_input(&self, launch_conversation: Option<&str>) -> TurnPrompt {
        let input = serde_json::json!({ "user_request": self.user_request });
        let mut text = format!("Explicit task input (JSON data):\n{input}");
        if let Some(conversation) = launch_conversation {
            let conversation = serde_json::json!(conversation);
            let _ = write!(
                text,
                "\n\nSession conversation at workflow launch (JSON string, historical data \
                 only):\n{conversation}"
            );
        }
        let mut prompt = TurnPrompt::from_agent_data(text);
        prompt.attachments.clone_from(&self.attachments);

        prompt
    }
}

/// Definitions assigned to workflow stages, independent of stage policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PlanThenImplementAgents {
    /// Agent used for implementation after explicit acceptance.
    pub implementer: crate::AgentDefinition,
    /// Agent used for planning and revisions.
    pub planner: crate::AgentDefinition,
}

impl PlanThenImplementAgents {
    /// Resolves the bundled defaults, returning `None` if either is
    /// unavailable.
    #[must_use]
    pub fn bundled() -> Option<Self> {
        Some(Self {
            implementer: crate::AgentDefinition::bundled("implementer")?,
            planner: crate::AgentDefinition::bundled("planner")?,
        })
    }
}

/// Explicit acceptance of one exact Planner version.
///
/// Acceptance satisfies the Implementer's dependency gate; the executor then
/// admits the Implementer with that version pinned as input.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AcceptTaskChainPlanRequest {
    /// Latest validated Planner execution selected by the user.
    pub plan_attempt_id: String,
}

/// Explicit revision, clarification, or retry of the current workflow stage.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ContinueTaskChainRequest {
    /// Images accompanying this guidance, retained by later attempts.
    pub attachments: Vec<TurnPromptAttachment>,
    /// New execution UUID; old deliveries never restart or uncancel work.
    pub attempt_id: String,
    /// User guidance addressed only to the current role's own context.
    /// `None` retries failed or canceled work with its original request.
    pub clarification: Option<String>,
    /// Current attempt observed when this guidance was authored. Admission
    /// rejects a new delivery if the workflow has advanced since that snapshot.
    pub expected_attempt_id: String,
}

/// Natural-language user reply addressed to a session's latest workflow.
///
/// The host decides what the reply means: plan approval, plan revision, or
/// guidance for the current role. No words are reserved.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReplyToTaskChainRequest {
    /// Images accompanying the visible reply.
    pub attachments: Vec<TurnPromptAttachment>,
    /// User-authored reply, recorded once as a visible session prompt.
    pub message: String,
    /// Globally unique delivery identity. Redelivery returns the original
    /// outcome without recording or deciding again.
    pub reply_id: String,
}

/// Workflow action admitted for one natural-language reply.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskChainReplyOutcome {
    /// The reply clearly approved the plan; the Implementer was released.
    PlanAccepted,
    /// The reply was delivered to the Planner as plan revision guidance.
    PlanRevised,
    /// The reply was delivered to the current role as an answer or retry.
    Continued,
}

impl TaskChainReplyOutcome {
    /// Stable persisted identity.
    pub fn id(self) -> &'static str {
        match self {
            Self::PlanAccepted => "plan_accepted",
            Self::PlanRevised => "plan_revised",
            Self::Continued => "continued",
        }
    }

    /// Resolves an identity produced by [`Self::id`].
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "plan_accepted" => Some(Self::PlanAccepted),
            "plan_revised" => Some(Self::PlanRevised),
            "continued" => Some(Self::Continued),
            _ => None,
        }
    }
}

/// Consistent workflow snapshot projected from one execution graph.
///
/// Reading never dispatches work. Graph executions remain the authoritative
/// execution records.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TaskChain {
    /// Exact immutable accepted version, present after explicit handoff.
    pub accepted_plan: Option<AcceptedTaskPlan>,
    /// Shared captured provider/model selection.
    pub agent: AgentSelection,
    /// Immutable definitions captured for both workflow stages at launch.
    pub agents: PlanThenImplementAgents,
    /// Latest admitted execution of the current role, absent before its
    /// first admission. A pending continuation keeps its source here.
    pub current_execution: Option<GraphExecutionRef>,
    /// Role the user currently interacts with.
    pub current_role: TaskAgentRole,
    /// Stable workflow identity, equal to its execution-graph identity.
    pub id: String,
    /// Separate Implementer task conversation.
    pub implementer_task_id: String,
    /// Projection from graph node progress.
    pub phase: TaskChainPhase,
    /// Separate Planner task conversation, retained after implementation.
    pub planner_task_id: String,
    /// Graph revision used by explicit graph actions.
    pub revision: NonZeroU64,
    /// Session owning the shared workspace and serialized worker.
    pub session_id: SessionId,
    /// Immutable objective captured at launch.
    pub user_request: String,
}

impl TaskChain {
    /// Returns the attempt identity of the current role's latest execution.
    pub fn current_attempt_id(&self) -> Option<&str> {
        match self.current_execution.as_ref()? {
            GraphExecutionRef::TaskAttempt { attempt_id, .. } => Some(attempt_id),
            GraphExecutionRef::UtilityRun { .. } => None,
        }
    }

    /// Returns the deterministic task identity for one role in a chain.
    pub fn task_id(chain_id: &str, role: TaskAgentRole) -> String {
        format!("chain:{chain_id}:{}", role.id())
    }

    /// Returns the role a node plays when it belongs to a task-chain graph.
    pub fn role(graph_id: &str, node: &GraphNode) -> Option<TaskAgentRole> {
        let role = TaskAgentRole::from_id(&node.id)?;
        let GraphContext::TaskConversation { task_id } = &node.execution.context else {
            return None;
        };

        (*task_id == Self::task_id(graph_id, role)).then_some(role)
    }

    /// Projects a task-chain graph, returning `None` for other graph shapes.
    ///
    /// `accepted_plan` must come from authoritative acceptance evidence for
    /// the Planner's `plan` output.
    pub fn project(
        graph: &ExecutionGraph,
        progress: &ExecutionGraphProgress,
        accepted_plan: Option<AcceptedTaskPlan>,
    ) -> Option<Self> {
        let [planner, implementer] = graph.nodes.as_slice() else {
            return None;
        };
        if Self::role(&graph.id, planner) != Some(TaskAgentRole::Planner)
            || Self::role(&graph.id, implementer) != Some(TaskAgentRole::Implementer)
        {
            return None;
        }
        let [planner_progress, implementer_progress] = progress.nodes.as_slice() else {
            return None;
        };
        let implementing = accepted_plan.is_some() || !implementer_progress.executions.is_empty();
        let (current_role, current) = if implementing {
            (TaskAgentRole::Implementer, implementer_progress)
        } else {
            (TaskAgentRole::Planner, planner_progress)
        };
        let phase = if progress.status == ExecutionGraphStatus::Cancelled {
            TaskChainPhase::Canceled
        } else {
            TaskChainPhase::from_node(current_role, current)
        };

        Some(Self {
            accepted_plan,
            agent: planner.agent,
            agents: PlanThenImplementAgents {
                implementer: implementer.definition.clone(),
                planner: planner.definition.clone(),
            },
            current_execution: current.executions.last().cloned(),
            current_role,
            id: graph.id.clone(),
            implementer_task_id: Self::task_id(&graph.id, TaskAgentRole::Implementer),
            phase,
            planner_task_id: Self::task_id(&graph.id, TaskAgentRole::Planner),
            revision: graph.revision,
            session_id: graph.session_id.clone(),
            user_request: planner.objective.clone(),
        })
    }
}

/// Workflow presentation derived from the current node's latest execution.
/// Success records execution, never implicit acceptance of a plan or outcome.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TaskChainPhase {
    /// Planner execution is pending, queued, or running.
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
    /// The current stage or the whole graph was canceled. A canceled stage
    /// may continue with an explicit new attempt; a canceled graph cannot.
    Canceled,
    /// Implementer finished successfully; the session remains reviewable.
    Succeeded,
}

impl TaskChainPhase {
    fn from_node(role: TaskAgentRole, node: &GraphNodeProgress) -> Self {
        let planner = role == TaskAgentRole::Planner;
        match node.status {
            GraphNodeStatus::Pending | GraphNodeStatus::Queued | GraphNodeStatus::Running => {
                if planner {
                    Self::Planning
                } else {
                    Self::Implementing
                }
            }
            GraphNodeStatus::AwaitingAnswers => {
                if planner {
                    Self::PlannerQuestion
                } else {
                    Self::ImplementerQuestion
                }
            }
            GraphNodeStatus::Succeeded if planner => Self::AwaitingPlanAcceptance,
            GraphNodeStatus::Succeeded => Self::Succeeded,
            GraphNodeStatus::Failed => Self::Failed,
            GraphNodeStatus::Cancelled => Self::Canceled,
        }
    }
}

#[cfg(test)]
#[path = "task_chain_test.rs"]
mod tests;
