//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model loop, normalized completion
//! metadata, validated structured output, and deny-by-default repository
//! inspection and patch tools. Provider and local filesystem implementations
//! remain behind injectable boundaries.

#[cfg(test)]
#[path = "../tests/support/model_switch.rs"]
mod model_switch_test;

#[cfg(test)]
#[path = "../tests/support/recovery.rs"]
mod recovery_test;

#[cfg(test)]
extern crate self as ag_harness;

#[cfg(test)]
#[path = "../tests/support/store_conformance.rs"]
mod store_conformance_test;

#[cfg(test)]
#[path = "../tests/support/repository.rs"]
mod repository_fixture;

mod bash;
mod cancellation;
#[cfg(test)]
#[path = "../tests/support/cancellation.rs"]
mod cancellation_test;

mod chat_completion;
mod command_journal;
mod command_settlement;
mod compaction;
#[cfg(test)]
#[path = "../tests/support/compaction.rs"]
mod compaction_projection_test;
mod comparison;
mod context;
#[cfg(test)]
#[path = "../tests/support/context_projection.rs"]
mod context_projection_test;
mod effect;
mod engine;
mod execution;
mod file_system;
mod harness;
mod input;
mod lifecycle;
mod memory_store;
mod model;
mod model_registry;
#[cfg(test)]
#[path = "../tests/support/model_registry.rs"]
mod model_registry_test;
mod policy;
mod provider;
mod read;
mod recovery;
mod repository;
mod schema_contract;
mod session;
mod session_model;
mod store;
mod store_coordinator;
mod telemetry;
mod tool;
mod trace;
mod turn;
mod turn_options_snapshot;
mod write;
mod write_journal;

pub use bash::{BashArguments, BashConfig, BashError};
pub use cancellation::{ControlledTurn, SettlementError, TurnControl};
pub use command_journal::{
    CommandCleanupScope, CommandIntent, CommandOutcome, CommandRecord, CommandTermination,
};
pub use command_settlement::CommandSettlementError;
pub use compaction::{CheckpointError, MAX_SUMMARY_BYTES, SessionCheckpoint};
pub use comparison::{ComparisonBase, ComparisonBaseError};
pub use context::{ContextBudget, ContextBudgetError, ContextEstimator, HeuristicContextEstimator};
pub use effect::EffectSettlementError;
pub use execution::{
    BashExecutor, BashProcess, ExecutionAccess, ExecutionCommand, ExecutionError, ExecutionPolicy,
    MainExit, OutputStream, ProcessEvent, UnsandboxedExecutor,
};
pub use file_system::{FileSystem, LocalFileSystem};
pub use harness::{Harness, Session, SessionBuilder};
pub use input::{ImageContent, ImageMediaType, InputBlock, TurnInput, TurnInputError};
pub use lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, LifecycleObserverSet,
    LifecycleOperationGuard, ModelResponseType, ToolErrorType, TurnErrorType,
};
pub use memory_store::MemoryStore;
pub use model::{
    CompletionMetadata, CompletionUsage, Model, ModelClient, ModelCompletion, ModelError,
    ModelErrorType, ModelMessage, ModelMetadata, ModelMetadataError, ModelRequest, ModelResponse,
    ReasoningEffort,
};
pub use model_registry::{ModelCapabilities, ModelRegistration, ModelRegistry, ModelRegistryError};
pub use policy::ToolPolicy;
pub use provider::{
    KIMI_K2_6, KimiConfig, MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, ModelConfiguration,
    ModelConfigurationError, ModelProvider, ModelProviderParseError, Muse, MuseConfig, QWEN_PLUS,
    QwenConfig,
};
pub use read::{ReadError, ReadOutput};
pub use recovery::{
    ExecutionIdentity, HostRequest, HostTurnAcquisition, HostTurnRecord, HostTurnStatus,
};
pub use repository::{Repository, RepositoryError};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use session::{
    AcquiredTurn, Database as SqliteStore, LoadedSession, NewSession, SessionError, SessionInfo,
    StoreIdentity, TurnOwner,
};
pub use session_model::RecordedModel;
pub use store::SessionStore;
pub use telemetry::LifecycleMetrics;
pub use tool::{
    ReadAction, ReadArguments, ReadSide, Tool, ToolCall, ToolCallArguments, ToolDefinition,
    WriteArguments,
};
pub use trace::LifecycleTraceObserver;
pub use turn::{
    ModelRequestActivity, ToolActivity, TurnError, TurnLimits, TurnOptions, TurnOutcome, TurnReport,
};
pub use turn_options_snapshot::{StoredTurnOptions, StoredTurnOptionsError};
pub use write::{WriteError, WriteOutput};
pub use write_journal::{WriteRecord, WriteStatus};

/// Entry point for the matching trusted `ag-harness-sandbox` executable.
/// Run only in a dedicated process, before creating any runtime or threads.
/// Host applications should launch the executable through Bash turn options.
#[doc(hidden)]
pub fn run_sandbox_launcher() -> std::process::ExitCode {
    execution::run_launcher()
}
