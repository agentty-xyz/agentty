//! Wall-clock boundary for monotonic and system time access.
//!
//! Production adapters that read the host clock live here so app and
//! runtime orchestration can stay free of direct `Instant::now()` and
//! `SystemTime::now()` calls. Use [`from_environment`] to wire production
//! code and mock the [`Clock`] trait in tests for deterministic time control.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use time::{OffsetDateTime, UtcOffset};

/// Environment variable that pins wall-clock time to a fixed Unix timestamp.
///
/// End-to-end feature recordings hash the captured frames, so every
/// wall-clock-derived value on screen — session timers, rotating status-bar
/// hints — must be identical between runs or the same UI hashes differently
/// every minute. Feature tests set this; normal runs leave it unset.
pub(crate) const CLOCK_UNIX_ENV_VAR: &str = "AGENTTY_CLOCK_UNIX";

/// Environment variable that pins the UTC offset used with a fixed wall clock.
///
/// The value is expressed in seconds and only applies when
/// [`CLOCK_UNIX_ENV_VAR`] selects a fixed clock. Fixed clocks default to UTC
/// when this variable is unset or invalid.
pub(crate) const CLOCK_UTC_OFFSET_SECONDS_ENV_VAR: &str = "AGENTTY_CLOCK_UTC_OFFSET_SECONDS";

/// Provides monotonic and system-time values used by runtime and persistence
/// adapters.
pub(crate) trait Clock: Send + Sync {
    /// Returns the UTC offset associated with one Unix timestamp.
    ///
    /// Test clocks default to UTC. Production clocks override this method to
    /// resolve the host offset for the requested timestamp.
    fn local_utc_offset_seconds(&self, _timestamp_seconds: i64) -> i64 {
        0
    }

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
    let unix_seconds = std::env::var(CLOCK_UNIX_ENV_VAR).ok();
    let utc_offset_seconds = std::env::var(CLOCK_UTC_OFFSET_SECONDS_ENV_VAR).ok();

    from_environment_values(unix_seconds.as_deref(), utc_offset_seconds.as_deref())
}

/// Builds the selected clock from optional environment-variable values.
fn from_environment_values(
    unix_seconds: Option<&str>,
    utc_offset_seconds: Option<&str>,
) -> Arc<dyn Clock> {
    match parse_pinned_system_time(unix_seconds) {
        Some(system_time) => Arc::new(FixedClock {
            local_utc_offset_seconds: utc_offset_seconds
                .and_then(parse_utc_offset_seconds)
                .unwrap_or_default(),
            system_time,
        }),
        None => Arc::new(RealClock),
    }
}

/// Converts one wall-clock value to Unix milliseconds for render animations.
pub(crate) fn unix_timestamp_millis(system_time: SystemTime) -> u128 {
    system_time
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis())
}

/// Converts one wall-clock value to a Unix timestamp in whole seconds.
///
/// Values before the Unix epoch and values too large for `i64` use zero. This
/// matches the persistence layer's historical fallback while keeping the time
/// read behind [`Clock`].
pub(crate) fn unix_timestamp_seconds(clock: &dyn Clock) -> i64 {
    clock
        .now_system_time()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok())
        .unwrap_or_default()
}

/// Production clock backed by `std::time`.
pub(crate) struct RealClock;

impl Clock for RealClock {
    fn local_utc_offset_seconds(&self, timestamp_seconds: i64) -> i64 {
        system_local_utc_offset_seconds(timestamp_seconds)
    }

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
    /// UTC offset returned by [`Clock::local_utc_offset_seconds`].
    local_utc_offset_seconds: i64,

    /// Wall-clock instant returned by every [`Clock::now_system_time`] call.
    system_time: SystemTime,
}

impl Clock for FixedClock {
    fn local_utc_offset_seconds(&self, _timestamp_seconds: i64) -> i64 {
        self.local_utc_offset_seconds
    }

    fn now_instant(&self) -> Instant {
        Instant::now()
    }

    fn now_system_time(&self) -> SystemTime {
        self.system_time
    }
}

/// Parses the optional pinned wall-clock environment value.
fn parse_pinned_system_time(raw: Option<&str>) -> Option<SystemTime> {
    let raw = raw?;
    let unix_seconds = raw.trim().parse::<u64>().ok()?;

    Some(SystemTime::UNIX_EPOCH + Duration::from_secs(unix_seconds))
}

/// Parses a valid UTC offset expressed in whole seconds.
fn parse_utc_offset_seconds(raw: &str) -> Option<i64> {
    let seconds = raw.trim().parse::<i32>().ok()?;
    let offset = UtcOffset::from_whole_seconds(seconds).ok()?;

    Some(i64::from(offset.whole_seconds()))
}

/// Resolves the host's UTC offset for one Unix timestamp.
///
/// Invalid timestamps and platforms without local-offset support fall back to
/// UTC so rendering remains available.
fn system_local_utc_offset_seconds(timestamp_seconds: i64) -> i64 {
    let Ok(utc_timestamp) = OffsetDateTime::from_unix_timestamp(timestamp_seconds) else {
        return 0;
    };

    UtcOffset::local_offset_at(utc_timestamp)
        .map(|local_offset| i64::from(local_offset.whole_seconds()))
        .unwrap_or_default()
}

#[cfg(test)]
#[path = "clock_test.rs"]
mod tests;
