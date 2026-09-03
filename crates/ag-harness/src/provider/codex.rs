mod auth;
mod client;
mod error;
mod model;
mod sse;

pub(crate) use model::CodexBackend;
pub use model::{Codex, CodexConfig};

#[cfg(test)]
#[path = "codex_test_support.rs"]
mod test_support;
