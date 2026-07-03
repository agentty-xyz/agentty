//! Provider-specific app-server clients hidden under the agent module.
//!
//! This router keeps concrete app-server runtime integrations grouped with
//! their matching backend implementations instead of exposing them as
//! top-level `infra/` modules.

mod codex;
mod command;
mod gemini;
mod stdio_transport;

pub(crate) use codex::RealCodexAppServerClient;
pub(crate) use command::{build_codex_app_server_command, build_gemini_acp_command};
pub(crate) use gemini::RealGeminiAcpClient;
