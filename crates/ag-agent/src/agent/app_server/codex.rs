//! Codex app-server module router.
//!
//! This parent module keeps the public export surface small while concrete
//! Codex runtime orchestration lives under `infra/agent/app_server/codex/`.

mod client;
mod lifecycle;
mod policy;
mod stream_parser;
mod usage;

pub(crate) use client::RealCodexAppServerClient;
