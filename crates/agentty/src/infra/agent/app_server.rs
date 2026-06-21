//! Provider-specific app-server clients hidden under the agent module.
//!
//! This router keeps concrete app-server runtime integrations grouped with
//! their matching backend implementations instead of exposing them as
//! top-level `infra/` modules.

mod codex;

pub(crate) use codex::RealCodexAppServerClient;
