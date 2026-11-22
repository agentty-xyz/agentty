//! In-process `ag-harness` transport for [`ag_session::AgentKind::Harness`].

mod activity;
mod config;
mod output;
mod sign_in;
mod turn;
mod turn_error;

pub(crate) use activity::ActivityBridge;
pub use config::NativeHarnessConfig;
pub(crate) use config::{
    HarnessContext, ModelSelection, RunOptions, failure, harness_models, output_schema,
    process_environment, repository, running_context_window, system_prompt,
};
pub(crate) use output::{OutputProjector, turn_notice};
pub(crate) use sign_in::sign_in_chatgpt;
pub(crate) use turn::{
    NativePrompt, cleanup_failure, parse_output, reject_attachments, run_one_shot,
};
pub(crate) use turn_error::NativeTurnError;
