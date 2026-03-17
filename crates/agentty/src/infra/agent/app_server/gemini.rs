//! Gemini app-server module router.
//!
//! This parent module keeps the public export surface small while concrete
//! Gemini ACP runtime orchestration lives under
//! `infra/agent/app_server/gemini/`.

mod client;

pub(crate) use client::RealGeminiAcpClient;
