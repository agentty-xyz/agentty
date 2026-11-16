//! Pinned wall clock shared by PTY feature runs and the showcase generators.

/// Environment variable that pins the agentty wall clock.
///
/// Mirrors `agentty::infra::clock::CLOCK_UNIX_ENV_VAR`, which is private to
/// the binary crate and therefore not importable from integration targets.
pub(crate) const PINNED_CLOCK_ENV_VAR: &str = "AGENTTY_CLOCK_UNIX";
/// Environment variable that pins the UTC offset paired with the clock.
///
/// Mirrors `agentty::infra::clock::CLOCK_UTC_OFFSET_SECONDS_ENV_VAR`, which is
/// private to the binary crate and therefore not importable from integration
/// targets.
pub(crate) const PINNED_CLOCK_UTC_OFFSET_ENV_VAR: &str = "AGENTTY_CLOCK_UTC_OFFSET_SECONDS";
/// Wall-clock time every pinned run uses: `2026-07-01T00:00:00Z`.
///
/// Captured frames are hashed to decide whether a GIF needs re-recording, so
/// anything derived from the wall clock, such as session timers and dashboard
/// activity, must not drift between runs.
pub(crate) const PINNED_CLOCK_UNIX_SECONDS: i64 = 1_782_864_000;
/// UTC offset used by every pinned run so host timezones cannot alter frames.
pub(crate) const PINNED_CLOCK_UTC_OFFSET_SECONDS: i64 = 0;
