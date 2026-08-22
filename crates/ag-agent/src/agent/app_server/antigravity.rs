//! Antigravity persistent-runtime module router.
//!
//! Concrete NDJSON session orchestration lives in the child modules while
//! this router exposes only the production client to the provider registry.

mod client;
mod lifecycle;
mod stream_parser;
mod usage;

pub(crate) use client::RealAntigravityClient;
