//! Frontend-neutral session management, selection catalog, and session models.
//!
//! Host applications implement [`SessionBackend`] to connect the stable
//! programmatic API to their persistence, agent, Git, and forge workflows.
//! Callers such as background callers use [`SessionService`] without
//! depending on terminal UI state.

mod agent;
mod agent_profile;
mod discovery;
mod env;
mod error;
mod execution_graph;
mod message;
mod model;
mod project;
mod question;
mod review;
mod service;
mod setting;
mod task;
mod task_chain;
mod task_execution;
mod task_plan;
mod task_role;
mod transcript_notice;

/// Shared explicit model selections for deterministic tests and demos.
#[cfg(any(test, feature = "test-utils"))]
#[path = "test_support_test.rs"]
pub mod test_support;

pub use ag_contracts::{ReasoningLevel, SessionDiffState, SessionStats};
pub use agent::{
    AgentKind, AgentModel, AgentSelection, AgentSelectionMetadata,
    parse_persisted_session_agent_model, resolve_agent_kind_for_model,
    resolve_agent_selection_for_model, resolve_model_for_available_agent_kinds,
    resolve_prompt_model_agent_kind, selectable_models_for_agent_kinds,
};
pub use agent_profile::{
    AGENT_PROFILE_PROMPT_MAX_BYTES, AgentDefinition, AgentDefinitionParseError,
    AgentDefinitionSource, AgentDefinitionSummary, agent_definition_identity,
    bundled_agent_definitions, parse_agent_definition, parse_agent_summary,
};
pub use discovery::{
    AgentAvailabilityProbe, AgentCliInfo, AgentCliVersion, StaticAgentAvailabilityProbe,
};
pub use env::{EnvId, ResolvedEnv};
pub use error::SessionError;
pub use execution_graph::{
    ExecutionGraph, GraphCompletionGate, GraphContext, GraphContractError, GraphDependency,
    GraphExecutionProfile, GraphExecutionRef, GraphInputBinding, GraphNode, GraphOutput,
    GraphPostProcessing, GraphResolvedInput, GraphResult, GraphResultRef, GraphResultState,
    GraphWorkspaceAccess,
};
pub use message::{
    AgentAnswerContent, SessionMessage, SessionMessageKind, SessionMessageKindParseError,
    SessionTranscript, normalized_message_content, stored_message_content,
};
pub use model::{
    ForgeKind, PermissionMode, ResponseStyle, ReviewRequest, ReviewRequestState,
    ReviewRequestSummary, Session, SessionId, SessionSettings, SessionStatus, SpeedMode,
    activity_day_key_with_offset, session_branch,
};
pub use project::{
    Project, ProjectListItem, mru_project_order, ordered_project_items, project_name_from_path,
};
pub use question::{QuestionItem, default_option_index};
pub use review::{
    FocusedReviewStatus, build_apply_review_prompt, has_actionable_review_suggestions,
    review_suggestions,
};
pub use service::{
    AnswerQuestionsRequest, CreateSessionMode, CreateSessionRequest, DurableMessageRequest,
    DurableMessageVisibility, QuestionAnswer, SessionBackend, SessionService,
    StartExecutionGraphRequest,
};
pub use setting::SettingName;
pub use task::{CreateTaskRequest, SubmitTaskRequest, Task, TaskAttempt, TaskAttemptStatus};
pub use task_chain::{
    AcceptTaskChainPlanRequest, ContinueTaskChainRequest, PlanThenImplementAgents,
    ReplyToTaskChainRequest, StartPlanThenImplementRequest, TaskChain, TaskChainPhase,
    TaskChainReplyOutcome,
};
pub use task_execution::{
    AcceptedTaskPlan, SubmitAgentTaskRequest, TaskAgentAssignment, TaskAgentContext,
    TaskAgentHistory, TaskAgentInput, TaskAgentTurn,
};
pub use task_plan::AcceptTaskPlanRequest;
pub use task_role::{AgentContextRequirement, AgentOutputKind, TaskAgentRole};
pub use transcript_notice::TranscriptNotice;
