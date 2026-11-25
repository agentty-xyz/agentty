//! In-process `ag-harness` transport for [`ag_session::AgentKind::Harness`].

mod activity;
mod api_key;
mod cargo_cache;
mod cargo_lockfile;
mod command_exit;
mod config;
mod output;
mod run_projector;
mod sandbox;
mod sign_in;
mod tool_description;
mod turn;
mod turn_error;
mod usage;

pub(crate) use activity::ActivityBridge;
pub(crate) use api_key::{save_api_key, with_stored_api_keys};
pub use cargo_cache::TrustedCargoCache;
pub use config::NativeHarnessConfig;
pub(crate) use config::{
    HarnessContext, ModelSelection, RunOptions, failure, harness_models, output_schema,
    process_environment, repository, running_context_window, system_prompt,
};
pub(crate) use run_projector::RunProjector;
pub(crate) use sandbox::SESSION_SANDBOX;
pub use sandbox::dispatch_sandbox_helper;
pub(crate) use sign_in::sign_in_chatgpt;
pub(crate) use turn::{
    NativePrompt, charge_run, cleanup_failure, parse_output, reject_attachments, run_one_shot,
};
pub(crate) use turn_error::NativeTurnError;
pub(crate) use usage::UsageMeter;
