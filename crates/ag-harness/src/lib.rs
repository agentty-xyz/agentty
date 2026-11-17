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
//! - [`Tool`]s are denied until allowed with [`Harness::allow`] or a
//!   [`ToolPolicy`].
//! - [`RunError`] and [`HarnessError`] classify any [`TurnError`] or
//!   [`SessionError`] by [`ErrorKind`] and whether a retry may succeed.
//!
//! # Modules
//!
//! The crate root holds everything a typical host needs. Extension points and
//! detailed records live in modules:
//!
//! | Module        | Contents                                                   |
//! | ------------- | ---------------------------------------------------------- |
//! | [`provider`]  | Built-in Muse, Kimi, and Qwen clients                      |
//! | [`catalog`]   | Model specs, roles, capabilities, pricing, and selection   |
//! | [`model`]     | The [`Model`] trait, requests, registry, context profiles  |
//! | [`tool`]      | Tool arguments, results, and the [`tool::FileSystem`] seam |
//! | [`bash`]      | Bash configuration, executors, enforcement, records        |
//! | [`turn`]      | Turn builders, reports, activity, and settlement errors    |
//! | [`store`]     | Session stores and durable write and checkpoint records    |
//! | [`recovery`]  | Execution identities and idempotent host request records   |
//! | [`lifecycle`] | Content-free events and OpenTelemetry observers            |

#[cfg(test)]
extern crate self as ag_harness;

#[cfg(test)]
#[path = "../tests/support/store_fixture.rs"]
mod store_fixture;

#[cfg(test)]
mod gated_store_test;

#[cfg(test)]
#[path = "../tests/support/chatgpt_sign_in.rs"]
mod chatgpt_sign_in_fixture;

#[cfg(test)]
#[path = "../tests/support/context_budget.rs"]
mod context_budget_fixture;

#[cfg(test)]
#[path = "../tests/support/model_profile.rs"]
mod model_profile_fixture;

mod admission;
pub mod bash;
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
mod execution;
mod file_system;
mod harness;
mod input;
pub mod lifecycle;
mod memory_store;
pub mod model;
mod model_profile;
mod model_registry;
mod policy;
pub mod provider;
mod read;
pub mod recovery;
mod repetition;
mod repository;
mod reservation;
mod round_summary;
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

pub use catalog::{ModelCatalog, ModelSelector, Role};
pub use comparison::{ComparisonBase, ComparisonBaseError};
pub use error::{ErrorKind, HarnessError, RunError};
pub use harness::{Harness, HarnessBuildError, HarnessBuilder, Session, SessionBuilder};
pub use input::{ImageContent, ImageMediaType, InputBlock, TurnInput, TurnInputError};
pub use model::{Model, ModelError};
pub use policy::ToolPolicy;
pub use repository::{Repository, RepositoryError};
pub use schema_contract::{OutputSchema, OutputSchemaError};
pub use session::SessionError;
pub use tool::Tool;
pub use turn::{TurnControl, TurnError, TurnOptions, TurnOutcome};

/// Entry point for the matching trusted `ag-harness-sandbox` executable.
/// Run only in a dedicated process, before creating any runtime or threads.
/// Host applications should launch the executable through Bash turn options.
#[doc(hidden)]
pub fn run_sandbox_launcher() -> std::process::ExitCode {
    execution::run_launcher()
}
