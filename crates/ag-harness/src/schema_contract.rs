//! Shared schema contract and bounded diagnostics for the harness.
//!
//! Schema compilation and output validation live in `ag-router` so provider
//! clients and the harness use one validated contract.

#[cfg(test)]
pub(crate) use ag_router::RESPONSE_CONTENT_LIMIT_BYTES;
pub use ag_router::{OutputSchema, OutputSchemaError};
pub(crate) use ag_router::{OutputValidationError, bounded_diagnostic, ensure_content_size};
