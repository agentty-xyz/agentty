//! Store and schema fixtures built from the public API alone, shared by
//! external-consumer suites and crate tests.

#[cfg(feature = "sqlite")]
use std::path::Path;
use std::sync::Arc;

#[cfg(feature = "sqlite")]
use ag_harness::store::SqliteStore;
use ag_harness::store::{MemoryStore, SessionStore};
use ag_harness::{OutputSchema, TurnOptions};
use serde_json::json;

pub(crate) fn schema() -> OutputSchema {
    OutputSchema::new(json!({"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false})).expect("schema")
}

pub(crate) fn options() -> TurnOptions {
    TurnOptions::from_output(schema())
}

/// Returns one empty store of each public backend this build includes.
#[cfg(feature = "sqlite")]
pub(crate) async fn stores() -> Vec<Arc<dyn SessionStore>> {
    vec![
        Arc::new(MemoryStore::new()),
        Arc::new(
            SqliteStore::open(Path::new(":memory:"))
                .await
                .expect("sqlite"),
        ),
    ]
}

/// Returns one empty store of each public backend this build includes.
#[cfg(not(feature = "sqlite"))]
pub(crate) fn stores() -> std::future::Ready<Vec<Arc<dyn SessionStore>>> {
    std::future::ready(vec![Arc::new(MemoryStore::new())])
}
