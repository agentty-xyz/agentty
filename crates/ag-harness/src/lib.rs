//! Structured LLM turns with deny-by-default tools and durable sessions.
//!
//! Every turn ends in JSON that is validated locally against a caller-supplied
//! [`OutputSchema`]. Tools stay off until the host allows them, and the session
//! store, not the provider, is the source of truth for conversation state.
//!
//! # Quickstart
//!
//! ```no_run
//! use std::env;
//! use std::sync::Arc;
//!
//! use ag_harness::provider::{GPT_6_LUNA, ModelConfiguration, ModelProvider};
//! use ag_harness::{Harness, OutputSchema};
//! use serde_json::json;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let schema = OutputSchema::new(json!({
//!     "type": "object",
//!     "properties": { "summary": { "type": "string" } },
//!     "required": ["summary"],
//! }))?;
//! let model = ModelConfiguration::new(ModelProvider::OpenAi, GPT_6_LUNA)
//!     .client_from_environment(|name| env::var(name))?;
//! let harness = Harness::builder().model(Arc::new(model)).build()?;
//!
//! let outcome = harness.run_once("Summarize Cargo.toml", schema).await?;
//! println!("{}", outcome.output()["summary"]);
//! # Ok(())
//! # }
//! ```
//!
//! # Concepts
//!
//! - [`Harness`] holds the model, its required context budget, tool defaults,
//!   repository, and store. It is a cheap `Clone + Send + Sync` handle built
//!   with [`Harness::builder`].
//! - [`Session`] is a durable conversation: `harness.session(id, schema)`
//!   creates one and `harness.resume(id)` reopens it in any process.
//! - A turn is one prompt run to a schema-valid answer. `run_once` and
//!   `Session::send` cover the common case; `Harness::turn` and `Session::turn`
//!   return a builder for per-turn [`TurnOptions`], host request IDs, and
//!   cancellation through [`TurnControl`].
//! - A [`Run`] is a turn started from a [`RunRequest`] with [`Session::run`] or
//!   [`Harness::run`]. Awaiting it yields a settled [`RunOutcome`] with its
//!   [`RunOutput`], aggregated [`Usage`] and its [`Cost`] in micro-USD when the
//!   model has prices, the run's [`FileChange`]s and [`CommandRecord`]s, and
//!   its [`TurnReport`]; a [`CancelToken`] cancels it. [`Run::into_parts`] also
//!   streams its ordered [`RunEvent`]s through an [`EventStream`], ending with
//!   [`RunEventKind::Completed`].
//! - `Session::transcript` reads settled turns as [`TurnRecord`]s for a
//!   [`TurnRange`], and `Session::usage` sums their recorded tokens and cost as
//!   [`UsageTotals`]; `Harness::fork` copies a session's turns through one
//!   index into a new session with its own prompt-cache key.
//! - A [`BudgetLedger`], such as a [`SharedBudget`] capping total micro-USD or
//!   tokens, reserves every model request of the runs it is given, so parallel
//!   runs share one budget.
//! - A turn offers exactly the tools of its [`ToolSet`]: host [`ToolHandler`]s
//!   and the built-in `read`, `write`, and `bash` handlers. A harness offers no
//!   tools until [`HarnessBuilder::tools`], [`RunRequest::tools`], or
//!   [`TurnOptions::with_tools`] gives it a set.
//! - [`RunError`] and [`HarnessError`] classify any [`TurnError`] or
//!   [`SessionError`] by [`ErrorKind`] and whether a retry may succeed.
//!
//! # Embedding
//!
//! The crate's `examples/embed.rs` is a complete host: a model through
//! `ModelClient::from_spec`, a host tool behind an [`ApprovalHandler`], and
//! the run's event stream. The native Bash sandbox runs commands through a
//! helper executable: call `sandbox::dispatch` first in `main` so the
//! host's own executable is the helper, or install the standalone one with
//! `cargo install ag-harness --version <version> --features native-sandbox
//! --bin ag-harness-sandbox` and pass its path to `BashConfig::new`. Where the
//! sandbox cannot run, commands run unsandboxed with a warning unless the host
//! chooses otherwise with `sandbox::Sandbox`; the [`sandbox`] module describes
//! both.
//!
//! # Modules
//!
//! The crate root holds everything a typical host needs. Extension points and
//! detailed records live in modules:
//!
//! | Module         | Contents                                                   |
//! | -------------- | ---------------------------------------------------------- |
//! | [`provider`]   | Built-in Muse, Kimi, and Qwen clients                      |
//! | [`catalog`]    | Model specs, roles, capabilities, pricing, and selection   |
//! | [`budget`]     | Budget ledgers, estimates, and reservations                |
//! | [`model`]      | The [`Model`] trait, requests, registry, context profiles  |
//! | [`tool`]       | Tool handlers and sets, built-in arguments, file system    |
//! | [`bash`]       | Bash configuration, executors, enforcement, records        |
//! | [`sandbox`]    | Native sandbox choice, helper entry point, warnings        |
//! | [`turn`]       | Turn builders, reports, activity, and settlement errors    |
//! | [`store`]      | Session stores and durable write and checkpoint records    |
//! | [`recovery`]   | Execution identities and idempotent host request records   |
//! | [`event`]      | Run events with items, usage, and their wire shape         |
//! | [`transcript`] | Settled-turn transcripts and their wire shape              |
//! | [`lifecycle`]  | Content-free events and OpenTelemetry observers            |
//!
//! # Features
//!
//! The default features keep every built-in backend: `sqlite`,
//! `native-sandbox`, `otel`, `providers-openai`, `providers-compat`, and
//! `providers-anthropic`.
//! Disable them to build only the core with [`store::MemoryStore`],
//! host-implemented [`Model`]s, and host-selected Bash executors. The crate
//! README's feature table lists what each feature enables, and each gated
//! item's documentation names the feature it requires.

#![cfg_attr(docsrs, feature(doc_cfg))]

#[cfg(test)]
extern crate self as ag_harness;

#[cfg(test)]
#[path = "../tests/support/store_fixture.rs"]
mod store_fixture;

#[cfg(test)]
mod gated_store_test;

#[cfg(test)]
mod fault_store_test;

#[cfg(test)]
#[cfg(all(feature = "providers-compat", feature = "providers-openai"))]
#[path = "../tests/support/chatgpt_sign_in.rs"]
mod chatgpt_sign_in_fixture;

#[cfg(test)]
#[path = "../tests/support/context_budget.rs"]
mod context_budget_fixture;

#[cfg(test)]
#[path = "../tests/support/model_profile.rs"]
mod model_profile_fixture;

#[cfg(test)]
#[path = "../tests/support/repository.rs"]
mod repository_fixture;

#[cfg(test)]
#[path = "../tests/support/store_gate.rs"]
mod store_gate;

// Suites that inject stores of their own; the sealed `SessionStore` admits
// only in-crate implementations.
#[cfg(test)]
#[path = "../tests/support/cancellation.rs"]
mod cancellation_contract_test;

#[cfg(test)]
#[path = "../tests/support/compaction.rs"]
mod compaction_contract_test;

#[cfg(test)]
#[path = "../tests/support/catalog_registration.rs"]
mod catalog_registration_contract_test;

#[cfg(test)]
#[path = "../tests/support/model_registry.rs"]
mod model_registry_contract_test;

#[cfg(test)]
#[path = "../tests/support/model_switch.rs"]
mod model_switch_contract_test;

#[cfg(test)]
#[path = "../tests/support/recovery.rs"]
mod recovery_contract_test;

#[cfg(test)]
#[path = "../tests/support/run_cost.rs"]
mod run_cost_contract_test;

#[cfg(test)]
#[path = "../tests/support/store_conformance.rs"]
mod store_conformance_test;

#[cfg(test)]
#[path = "../tests/support/transcript.rs"]
mod transcript_contract_test;

mod admission;
pub mod approval;
pub mod bash;
pub mod budget;
mod builtin_tool;
mod calibration;
mod cancellation;
pub mod catalog;

mod command_journal;
mod command_settlement;
mod compaction;
mod comparison;
mod context;
#[cfg(test)]
#[path = "../tests/support/context_projection.rs"]
mod context_projection_test;
mod effect;
mod engine;
mod error;
pub mod event;
mod execution;
mod file_system;
mod harness;
mod input;
pub mod lifecycle;
mod limit;
mod memory_store;
pub mod model;
mod model_profile;
mod model_registry;
#[cfg(any(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
))]
pub mod provider;
mod read;
pub mod recovery;
mod repetition;
mod repository;
mod request_option;
mod reservation;
mod round_summary;
mod run;
pub mod sandbox;
mod schema_contract;
mod session;
mod session_model;
#[cfg(feature = "sqlite")]
mod sqlite_store;
pub mod store;
mod summary_model;
mod telemetry;
pub mod tool;
mod tool_handler;
#[cfg(feature = "otel")]
mod trace;
pub mod transcript;
pub mod turn;
mod turn_options_snapshot;
mod usage;
mod write;
mod write_journal;

pub use approval::{ApprovalHandler, ApprovalRequest, Decision};
pub use budget::{BudgetLedger, SharedBudget};
pub use catalog::{ModelCatalog, ModelSelector, Role};
pub use command_journal::CommandRecord;
pub use comparison::{ComparisonBase, ComparisonBaseError};
pub use error::{ErrorKind, HarnessError, RunError};
pub use event::{EventStream, RunEvent, RunEventKind};
pub use harness::{Harness, HarnessBuildError, HarnessBuilder, Session, SessionBuilder};
pub use input::{ImageContent, ImageMediaType, InputBlock, TurnInput, TurnInputError};
pub use limit::{LimitKind, RunLimits};
pub use model::{Model, ModelError};
pub use repository::{Repository, RepositoryError};
pub use request_option::{GenerationParams, OutputMode};
pub use run::{
    CancelToken, FileChange, Run, RunHandle, RunOutcome, RunOutput, RunRequest, Unsettled, Usage,
};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use session::{ModelSelectionError, SessionError};
pub use tool::{ToolHandler, ToolSet};
pub use transcript::{TurnRange, TurnRecord};
pub use turn::{TurnControl, TurnError, TurnOptions, TurnOutcome, TurnReport};
pub use usage::{Cost, UsageTotals};
pub use write_journal::WriteStatus;

/// Compiles the README's Rust examples as doctests.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
struct ReadmeDoctests;
