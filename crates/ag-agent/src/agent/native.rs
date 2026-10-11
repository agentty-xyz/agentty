//! In-process `ag-harness` transport for [`ag_session::AgentKind::Harness`].

mod activity;
mod config;
mod turn;

pub(crate) use activity::ActivityBridge;
pub use config::NativeHarnessConfig;
pub(crate) use config::{
    HarnessContext, ModelSelection, default_model, failure, output_schema, process_environment,
    repository, system_prompt, turn_options,
};
pub(crate) use turn::{
    NativePrompt, parse_outcome, reject_attachments, run_one_shot, settle, token_usage,
};
