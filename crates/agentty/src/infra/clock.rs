//! Wall-clock boundary for monotonic and system time access.
//!
//! Production adapters that read the host clock live here so app and
//! runtime orchestration can stay free of direct `Instant::now()` and
//! `SystemTime::now()` calls. Use `RealClock` to wire production code and
//! mock the [`Clock`] trait in tests for deterministic time control.

use std::time::{Instant, SystemTime};

/// Provides monotonic and system-time values used by session refresh logic,
/// runtime render-throttle accounting, and clipboard image timestamps.
pub(crate) trait Clock: Send + Sync {
    /// Returns the current monotonic instant.
    fn now_instant(&self) -> Instant;

    /// Returns the current wall-clock system time.
    fn now_system_time(&self) -> SystemTime;
}

/// Production clock backed by `std::time`.
pub(crate) struct RealClock;

impl Clock for RealClock {
    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        SystemTime::now()
    }
}
