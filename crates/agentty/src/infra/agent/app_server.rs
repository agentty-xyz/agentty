//! Provider-specific app-server clients hidden under the agent module.
//!
//! This router keeps concrete Codex and Gemini runtime integrations grouped
//! with their matching backend implementations instead of exposing them as
//! top-level `infra/` modules.

mod codex;
mod gemini;

pub(crate) use codex::RealCodexAppServerClient;
pub(crate) use gemini::RealGeminiAcpClient;
