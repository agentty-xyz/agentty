//! Transport-independent contracts for session and isolated agent execution.
//! Implementations own provider routing and resource cleanup; hosts own
//! scheduling.

mod activity;
mod contract;
mod execution_policy;
mod input_size;
mod notice;
mod one_shot;
mod output;
mod permission;
mod provider_call_budget;
mod reasoning;
mod session;
/// Shared usage fixtures for deterministic consumer tests.
#[cfg(any(test, feature = "test-utils"))]
#[path = "test_support_test.rs"]
pub mod test_support;
mod usage;

pub use activity::{
    ActivityEvent, ActivityFacts, ActivityFailure, ActivityKind, ActivityStatus, DiffStat,
    LineRange,
};
#[cfg(any(test, feature = "test-utils"))]
pub use contract::MockAgentChannel;
pub use contract::{
    AgentChannel, AgentDefinitionPrompt, AgentDefinitionPromptUpdate, AgentError, AgentFuture,
    AgentRequestKind, ContextCompaction, LiveTranscript, SessionRef, StartSessionRequest,
    TurnContinuation, TurnContinuationParts, TurnEvent, TurnRequest, TurnResult,
    normalize_instruction_conversation_id,
};
pub use execution_policy::{ExecutionPolicy, McpPolicy, ToolPolicy};
pub use input_size::is_input_size_error;
pub use notice::TurnNotice;
#[cfg(any(test, feature = "test-utils"))]
pub use one_shot::MockOneShotClient;
pub use one_shot::{OneShotClient, OneShotError, OneShotRequest, OneShotSubmission};
pub use output::{OutputEvent, OutputKind};
pub use permission::PermissionMode;
pub use provider_call_budget::{ProviderCallBudget, ProviderCallLedger};
pub use reasoning::ReasoningLevel;
pub use session::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};
pub use usage::{
    ApiEquivalentCost, ContextFill, CostBasis, CostEstimate, ModelUsage, PlanLogin, PricedCost,
    TurnUsage, UsageCheckpoint, UsagePurpose,
};
