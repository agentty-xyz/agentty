//! Resource limits, the handshake limiter, and the per-stream byte budget.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
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
    /// Registration handshakes one client address may have in progress at
    /// once, so one client cannot take every handshake.
    pub max_handshakes_per_address: usize,
    /// Instances that may be registered at once.
    pub max_instances: usize,
    /// Instances one client address may register at once.
    pub max_instances_per_address: usize,
    /// Streams one instance may have open or waiting at once.
    pub max_streams_per_instance: usize,
    /// Interval between keepalive pings on every connection.
    pub ping_interval: Duration,
    /// Time [`crate::RelayHandle::shutdown`] waits for requests that have not
    /// become connections before it drops them.
    pub shutdown_timeout: Duration,
    /// Time a peer has to take one message before the relay treats it as
    /// gone. Bounds every send, so a peer that stops reading cannot hold a
    /// connection open.
    pub write_timeout: Duration,
}

impl Default for RelayLimits {
    fn default() -> Self {
        Self {
            accept_timeout: Duration::from_secs(10),
            handshake_timeout: Duration::from_secs(10),
            max_bytes_per_minute: 16 * 1024 * 1024,
            max_handshakes: 64,
            max_handshakes_per_address: 4,
            max_instances: 10_000,
            max_instances_per_address: 8,
            max_streams_per_instance: 8,
            ping_interval: Duration::from_secs(30),
            shutdown_timeout: Duration::from_secs(5),
            write_timeout: Duration::from_secs(10),
        }
    }
}

/// Counts registration handshakes in progress, in total and per client
/// address.
pub(crate) struct HandshakeLimiter {
    in_progress: Mutex<HashMap<IpAddr, usize>>,
    max_per_address: usize,
    max_total: usize,
}

impl HandshakeLimiter {
    /// Creates a limiter with the handshake limits of `limits`.
    pub(crate) fn new(limits: &RelayLimits) -> Self {
        Self {
            in_progress: Mutex::new(HashMap::new()),
            max_per_address: limits.max_handshakes_per_address,
            max_total: limits.max_handshakes,
        }
    }

    /// Reserves one handshake for `address` until the guard drops; `None`
    /// when the address or the relay already has as many as allowed.
    pub(crate) fn reserve(self: &Arc<Self>, address: IpAddr) -> Option<HandshakeGuard> {
        let mut in_progress = self.lock();
        let total: usize = in_progress.values().sum();
        let from_address = in_progress.get(&address).copied().unwrap_or(0);
        if total >= self.max_total || from_address >= self.max_per_address {
            return None;
        }
        in_progress.insert(address, from_address + 1);

        Some(HandshakeGuard {
            address,
            limiter: Arc::clone(self),
        })
    }

    /// Locks the counts; a poisoned lock still holds consistent counts.
    fn lock(&self) -> MutexGuard<'_, HashMap<IpAddr, usize>> {
        self.in_progress
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }
}

/// One reserved registration handshake.
pub(crate) struct HandshakeGuard {
    address: IpAddr,
    limiter: Arc<HandshakeLimiter>,
}

impl Drop for HandshakeGuard {
    fn drop(&mut self) {
        let mut in_progress = self.limiter.lock();
        let remaining = in_progress
            .get(&self.address)
            .copied()
            .unwrap_or(0)
            .saturating_sub(1);
        if remaining == 0 {
            in_progress.remove(&self.address);
        } else {
            in_progress.insert(self.address, remaining);
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
