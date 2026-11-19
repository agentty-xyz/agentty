//! Unstable JSON-RPC 2.0 protocol that `ag-harness serve` speaks over
//! standard input and output, one JSON message per line.
//!
//! The wire types may change in any release until the protocol is
//! stabilized; `crates/ag-harness-cli/schema/serve_protocol.json`, which
//! `serve --print-schema` prints, describes the current shape.

mod approval;
mod protocol;
mod server;

pub(crate) use protocol::PROTOCOL_SCHEMA;
pub(crate) use server::{Settings, serve};
