//! Structured LLM turns with deny-by-default tools and durable sessions.
//!
//! Every turn ends in JSON that is validated locally against a caller-supplied
//! [`OutputSchema`]. Tools stay off until the host allows them, and the session
//! store, not the provider, is the source of truth for conversation state.
//!
//! # Quickstart
//!
//! ```no_run
//! use ag_harness::provider::{MUSE_SPARK_1_3, Muse};
//! use ag_harness::{Harness, OutputSchema};
//! use serde_json::json;
//!
//! # async fn example() -> Result<(), Box<dyn std::error::Error>> {
//! let schema = OutputSchema::new(json!({
//!     "type": "object",
//!     "properties": { "summary": { "type": "string" } },
//!     "required": ["summary"],
//! }))?;
//! let harness = Harness::new(Muse::from_env(MUSE_SPARK_1_3)?);
//!
//! let outcome = harness.run_once("Summarize Cargo.toml", schema).await?;
//! println!("{}", outcome.output()["summary"]);
//! # Ok(())
//! # }
//! ```
//!
//! # Concepts
//!
//! - [`Harness`] holds the model, tool defaults, repository, and store.
//! - [`Session`] is a durable conversation: `harness.session(id, schema)`
//!   creates one and `harness.resume(id)` reopens it in any process.
//! - A turn is one prompt run to a schema-valid answer. `run_once` and
//!   `Session::send` cover the common case; `Harness::turn` and `Session::turn`
//!   return a builder for per-turn [`TurnOptions`], host request IDs, and
//!   cancellation through [`TurnControl`].
//! - [`Tool`]s are denied until allowed with [`Harness::allow`] or a
//!   [`ToolPolicy`].
//!
//! # Modules
//!
//! The crate root holds everything a typical host needs. Extension points and
//! detailed records live in modules:
//!
//! | Module        | Contents                                                   |
//! | ------------- | ---------------------------------------------------------- |
//! | [`provider`]  | Built-in Muse, Kimi, and Qwen clients                      |
//! | [`model`]     | The [`Model`] trait, requests, registry, context budgets   |
//! | [`tool`]      | Tool arguments, results, and the [`tool::FileSystem`] seam |
//! | [`bash`]      | Sandboxed Bash configuration, executors, command records   |
//! | [`turn`]      | Turn builders, reports, activity, and settlement errors    |
//! | [`store`]     | Session stores and durable write and checkpoint records    |
//! | [`recovery`]  | Execution identities and idempotent host request records   |
//! | [`lifecycle`] | Content-free events and OpenTelemetry observers            |

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
mod gated_store_test;

#[cfg(test)]
#[path = "../tests/support/repository.rs"]
mod repository_fixture;

pub mod bash;
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
pub mod lifecycle;
mod memory_store;
pub mod model;
mod model_registry;
#[cfg(test)]
#[path = "../tests/support/model_registry.rs"]
mod model_registry_test;
mod policy;
pub mod provider;
mod read;
pub mod recovery;
mod repository;
mod reservation;
mod schema_contract;
mod session;
mod session_model;
pub mod store;
mod telemetry;
pub mod tool;
mod trace;
pub mod turn;
mod turn_options_snapshot;
mod write;
mod write_journal;

pub use comparison::{ComparisonBase, ComparisonBaseError};
pub use harness::{Harness, Session, SessionBuilder};
pub use input::{ImageContent, ImageMediaType, InputBlock, TurnInput, TurnInputError};
pub use model::{Model, ModelError};
pub use policy::ToolPolicy;
pub use repository::{Repository, RepositoryError};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use session::SessionError;
pub use tool::Tool;
pub use turn::{TurnControl, TurnError, TurnLimits, TurnOptions, TurnOutcome};

/// Entry point for the matching trusted `ag-harness-sandbox` executable.
/// Run only in a dedicated process, before creating any runtime or threads.
/// Host applications should launch the executable through Bash turn options.
#[doc(hidden)]
pub fn run_sandbox_launcher() -> std::process::ExitCode {
    execution::run_launcher()
}
