//! Lightweight, Rust-native LLM harness for application-facing agent workflows.
//!
//! The crate provides a provider-neutral model loop, normalized completion
//! metadata, validated structured output, and deny-by-default repository
//! inspection and patch tools. Provider and local filesystem implementations
//! remain behind injectable boundaries.

#[cfg(test)]
extern crate self as ag_harness;

#[cfg(test)]
#[path = "../tests/support/store_conformance.rs"]
mod store_conformance_test;

#[cfg(test)]
#[path = "../tests/support/repository.rs"]
mod repository_fixture;

mod cancellation;
#[cfg(test)]
#[path = "../tests/support/cancellation.rs"]
mod cancellation_test;

mod chat_completion;
mod comparison;
mod engine;
mod execution;
mod file_system;
mod harness;
mod lifecycle;
mod model;
mod policy;
mod provider;
mod read;
mod repository;
mod schema_contract;
mod session;
mod store;
mod store_coordinator;
mod telemetry;
mod tool;
mod trace;
mod turn;
mod turn_options_snapshot;
mod write;
mod write_journal;

pub use cancellation::{ControlledTurn, SettlementError, TurnControl};
pub use comparison::{ComparisonBase, ComparisonBaseError};
pub use file_system::{FileSystem, LocalFileSystem};
pub use harness::{Harness, Session, SessionBuilder};
pub use lifecycle::{
    LifecycleEvent, LifecycleEventKind, LifecycleId, LifecycleObserver, LifecycleObserverSet,
    LifecycleOperationGuard, ModelResponseType, ToolErrorType, TurnErrorType,
};
pub use model::{
    CompletionMetadata, CompletionUsage, Model, ModelClient, ModelCompletion, ModelError,
    ModelErrorType, ModelMessage, ModelMetadata, ModelMetadataError, ModelRequest, ModelResponse,
    ReasoningEffort,
};
pub use policy::ToolPolicy;
pub use provider::{
    KIMI_K2_6, KimiConfig, MUSE_SPARK_1_3, MUSE_SPARK_1_3_CONTRIBUTOR, ModelConfiguration,
    ModelConfigurationError, ModelProvider, ModelProviderParseError, Muse, MuseConfig, QWEN_PLUS,
    QwenConfig,
};
pub use read::{ReadError, ReadOutput};
pub use repository::{Repository, RepositoryError};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use session::{
    AcquiredTurn, Database as SqliteStore, LoadedSession, NewSession, SessionError, SessionInfo,
    StoreIdentity, TurnOwner,
};
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
