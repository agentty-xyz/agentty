//! Resource limits and the per-stream byte budget.

use std::time::Duration;

use tokio::time::Instant;

/// Length of the window a [`ByteBudget`] counts bytes over.
const BUDGET_WINDOW: Duration = Duration::from_mins(1);

/// Resources one relay lets clients hold.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RelayLimits {
    /// Time an instance has to answer a device's stream.
    pub accept_timeout: Duration,
    /// Time an instance has to finish its registration handshake.
    pub handshake_timeout: Duration,
    /// Bytes one stream may carry per minute, both directions combined.
    pub max_bytes_per_minute: usize,
    /// Registration handshakes that may be in progress at once.
    pub max_handshakes: usize,
    /// Instances that may be registered at once.
    pub max_instances: usize,
    /// Instances one client address may register at once.
    pub max_instances_per_address: usize,
    /// Streams one instance may have open or waiting at once.
    pub max_streams_per_instance: usize,
    /// Interval between keepalive pings on every connection.
    pub ping_interval: Duration,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            accept_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            max_bytes_per_minute: 16 * 1024 * 1024,
            max_handshakes: 64,
            max_instances: 10_000,
            max_instances_per_address: 8,
            max_streams_per_instance: 8,
            ping_interval: Duration::from_secs(30),
        }
    }
}

/// Counts the bytes one stream carries in the current one-minute window.
pub(crate) struct ByteBudget {
    limit: usize,
    spent: usize,
    window_start: Instant,
}

impl ByteBudget {
    /// Starts a window that allows `limit` bytes.
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            limit,
            spent: 0,
            window_start: Instant::now(),
        }
    }

    /// Records `bytes` and returns whether the stream is still within its
    /// budget. A new window starts once the current one is a minute old.
    pub(crate) fn spend(&mut self, bytes: usize) -> bool {
        let now = Instant::now();
        if now.duration_since(self.window_start) >= BUDGET_WINDOW {
            self.window_start = now;
            self.spent = 0;
        }
        self.spent = self.spent.saturating_add(bytes);

        self.spent <= self.limit
    }
}

#[cfg(test)]
#[path = "limit_test.rs"]
mod tests;
