//! Wall-clock boundary for monotonic and system time access.
//!
//! Production adapters that read the host clock live here so app and
//! runtime orchestration can stay free of direct `Instant::now()` and
//! `SystemTime::now()` calls. Use [`from_environment`] to wire production
//! code and mock the [`Clock`] trait in tests for deterministic time control.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

/// Environment variable that pins wall-clock time to a fixed Unix timestamp.
///
/// End-to-end feature recordings hash the captured frames, so every
/// wall-clock-derived value on screen — session timers, rotating status-bar
/// hints — must be identical between runs or the same UI hashes differently
/// every minute. Feature tests set this; normal runs leave it unset.
pub(crate) const CLOCK_UNIX_ENV_VAR: &str = "AGENTTY_CLOCK_UNIX";

/// Provides monotonic and system-time values used by session refresh logic,
/// runtime render-throttle accounting, and clipboard image timestamps.
pub(crate) trait Clock: Send + Sync {
    /// Returns the current monotonic instant.
    fn now_instant(&self) -> Instant;

    /// Returns the current wall-clock system time.
    fn now_system_time(&self) -> SystemTime;
}

/// Returns the clock selected by [`CLOCK_UNIX_ENV_VAR`].
///
/// Falls back to [`RealClock`] when the variable is unset or does not parse
/// as a Unix second count, so a malformed value can never silently freeze a
/// user's clock.
pub(crate) fn from_environment() -> Arc<dyn Clock> {
    match pinned_system_time() {
        Some(system_time) => Arc::new(FixedClock { system_time }),
        None => Arc::new(RealClock),
    }
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

/// Clock whose wall-clock reads are frozen at a fixed point in time.
///
/// Monotonic time keeps advancing so render throttling and elapsed-duration
/// bookkeeping still work; only the wall clock is pinned.
struct FixedClock {
    /// Wall-clock instant returned by every [`Clock::now_system_time`] call.
    system_time: SystemTime,
}

impl Clock for FixedClock {
    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        self.system_time
    }
}

/// Parses the pinned wall-clock time from [`CLOCK_UNIX_ENV_VAR`].
fn pinned_system_time() -> Option<SystemTime> {
    let raw = std::env::var(CLOCK_UNIX_ENV_VAR).ok()?;
    let unix_seconds = raw.trim().parse::<u64>().ok()?;

    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_seconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_clock_freezes_wall_clock_time() {
        // Arrange
        let system_time = SystemTime::UNIX_EPOCH + Duration::from_secs(90);
        let clock = FixedClock { system_time };

        // Act
        let first_read = clock.now_system_time();
        let second_read = clock.now_system_time();

        // Assert
        assert_eq!(first_read, system_time);
        assert_eq!(second_read, system_time);
    }

    #[test]
    fn fixed_clock_keeps_monotonic_time_advancing() {
        // Arrange
        let clock = FixedClock {
            system_time: SystemTime::UNIX_EPOCH,
        };

        // Act
        let earlier = clock.now_instant();
        let later = clock.now_instant();

        // Assert
        assert!(later >= earlier);
    }

    #[test]
    fn real_clock_advances_wall_clock_time() {
        // Arrange
        let clock = RealClock;

        // Act
        let now = clock.now_system_time();

        // Assert
        assert!(now > SystemTime::UNIX_EPOCH);
    }
}
