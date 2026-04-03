//! Gemini app-server module router.
//!
//! This parent module keeps the public export surface small while concrete
//! Gemini ACP runtime orchestration lives under
//! `infra/agent/app_server/gemini/`.

mod client;
mod lifecycle;
mod policy;
mod stream_parser;
mod transport;
mod usage;

pub(crate) use client::RealGeminiAcpClient;
#[cfg(test)]
pub(crate) use transport::MockGeminiRuntimeTransport;
