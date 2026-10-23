//! Durable immutable graph snapshots, execution links, and exact result
//! evidence.

mod execution;
mod model;
mod repository;
mod result;

pub use model::{GraphEvidence, GraphExecutionRecord, StoredGraphEvidence, StoredGraphResult};
pub use repository::GraphRepository;
#[cfg(any(test, feature = "test-utils"))]
pub use repository::MockGraphRepository;
pub(crate) use repository::SqliteGraphRepository;

#[cfg(test)]
#[path = "execution_graph/store_test.rs"]
mod tests;
