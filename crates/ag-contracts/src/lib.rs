//! Transport-independent contracts for session and isolated agent execution.
//! Implementations own provider routing and resource cleanup; hosts own
//! scheduling.

mod contract;
mod input_size;
mod one_shot;
mod permission;
mod provider_call_budget;
mod reasoning;
mod session;

#[cfg(any(test, feature = "test-utils"))]
pub use contract::MockAgentChannel;
pub use contract::{
    AgentChannel, AgentError, AgentFuture, AgentRequestKind, LiveTranscript, PersonalityPrompt,
    PersonalityPromptUpdate, SessionRef, StartSessionRequest, TurnContinuation,
    TurnContinuationParts, TurnEvent, TurnRequest, TurnResult,
    normalize_instruction_conversation_id,
};
pub use input_size::is_input_size_error;
#[cfg(any(test, feature = "test-utils"))]
pub use one_shot::MockOneShotClient;
pub use one_shot::{OneShotClient, OneShotError, OneShotRequest, OneShotSubmission};
pub use permission::PermissionMode;
pub use provider_call_budget::ProviderCallBudget;
pub use reasoning::ReasoningLevel;
pub use session::{ResponseStyle, SessionDiffState, SessionStats, SpeedMode};
