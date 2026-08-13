//! Timestamp boundary for persistence writes.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Supplies Unix timestamps for rows written by persistence adapters.
pub trait TimestampSource: Send + Sync {
    /// Returns the current Unix timestamp in whole seconds.
    fn now_timestamp_seconds(&self) -> i64;
}

impl<TimestampFn> TimestampSource for TimestampFn
where
    TimestampFn: Fn() -> i64 + Send + Sync,
{
    fn now_timestamp_seconds(&self) -> i64 {
        self()
    }
}

/// Returns a timestamp source backed by the host system clock.
pub(crate) fn system_timestamp_source() -> Arc<dyn TimestampSource> {
    Arc::new(system_timestamp_seconds)
}

/// Converts the host system clock to a Unix timestamp in whole seconds.
fn system_timestamp_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn closure_timestamp_source_returns_injected_value() {
        // Arrange
        let timestamp_source = || 123;

        // Act
        let timestamp = timestamp_source.now_timestamp_seconds();

        // Assert
        assert_eq!(timestamp, 123);
    }

    #[test]
    fn system_timestamp_source_returns_a_post_epoch_value() {
        // Arrange
        let timestamp_source = system_timestamp_source();

        // Act
        let timestamp = timestamp_source.now_timestamp_seconds();

        // Assert
        assert!(timestamp > 0);
    }
}
