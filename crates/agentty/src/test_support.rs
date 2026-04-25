//! Hidden support APIs for integration tests.
//!
//! These helpers intentionally live outside the production-facing module
//! surface so tests can share canonical naming rules without widening app APIs.

use std::path::{Path, PathBuf};

use crate::app;

/// Returns the canonical session folder path for integration-test fixtures.
pub fn session_folder(base: &Path, session_id: &str) -> PathBuf {
    app::session::session_folder(base, session_id)
}
