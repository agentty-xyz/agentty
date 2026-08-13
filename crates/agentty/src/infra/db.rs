//! Agentty persistence facade and database-location policy.

use std::sync::Arc;

pub use ag_store::*;

use crate::infra::clock;

/// Subdirectory under the Agentty home where the database file is stored.
pub const DB_DIR: &str = "db";

/// Default Agentty database filename.
pub const DB_FILE: &str = "agentty.db";

/// Returns a store timestamp source backed by Agentty's environment-selected
/// clock.
///
/// Feature tests pin that clock so database ordering and activity timestamps
/// remain deterministic alongside rendered frame time.
pub fn timestamp_source_from_environment() -> Arc<dyn TimestampSource> {
    let clock = clock::from_environment();

    Arc::new(move || clock::unix_timestamp_seconds(clock.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_timestamp_source_returns_a_unix_timestamp() {
        // Arrange
        let timestamp_source = timestamp_source_from_environment();

        // Act
        let timestamp = timestamp_source.now_timestamp_seconds();

        // Assert
        assert!(timestamp > 0);
    }
}
