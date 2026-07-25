//! Deterministic wall-clock values shared by one frontend frame.

/// One coherent wall-clock snapshot projected to a frontend render pass.
///
/// Keeping seconds, subsecond animation time, and current plus visible-reset
/// UTC offsets together prevents one frame from mixing multiple host-clock
/// reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameTime {
    local_utc_offset_seconds: i64,
    rate_limit_reset_local_utc_offset_seconds: i64,
    unix_millis: u128,
    unix_seconds: i64,
}

impl FrameTime {
    /// Creates one frame timestamp from values resolved at the infrastructure
    /// clock boundary.
    pub(crate) const fn new(
        unix_seconds: i64,
        unix_millis: u128,
        local_utc_offset_seconds: i64,
    ) -> Self {
        Self {
            local_utc_offset_seconds,
            rate_limit_reset_local_utc_offset_seconds: local_utc_offset_seconds,
            unix_millis,
            unix_seconds,
        }
    }

    /// Projects the local UTC offset active at the visible quota reset.
    #[must_use]
    pub(crate) const fn with_rate_limit_reset_local_utc_offset_seconds(
        mut self,
        rate_limit_reset_local_utc_offset_seconds: i64,
    ) -> Self {
        self.rate_limit_reset_local_utc_offset_seconds = rate_limit_reset_local_utc_offset_seconds;

        self
    }

    /// Returns the local UTC offset active at this frame timestamp.
    pub(crate) const fn local_utc_offset_seconds(self) -> i64 {
        self.local_utc_offset_seconds
    }

    /// Returns the local UTC offset active at the visible quota reset.
    pub(crate) const fn rate_limit_reset_local_utc_offset_seconds(self) -> i64 {
        self.rate_limit_reset_local_utc_offset_seconds
    }

    /// Returns the frame timestamp as Unix milliseconds for animations.
    pub(crate) const fn unix_millis(self) -> u128 {
        self.unix_millis
    }

    /// Returns the frame timestamp as Unix seconds for timers and day keys.
    pub(crate) const fn unix_seconds(self) -> i64 {
        self.unix_seconds
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_time_exposes_one_coherent_snapshot() {
        // Arrange
        let frame_time = FrameTime::new(1_700_000_000, 1_700_000_000_125, -28_800)
            .with_rate_limit_reset_local_utc_offset_seconds(-25_200);

        // Act & Assert
        assert_eq!(frame_time.unix_seconds(), 1_700_000_000);
        assert_eq!(frame_time.unix_millis(), 1_700_000_000_125);
        assert_eq!(frame_time.local_utc_offset_seconds(), -28_800);
        assert_eq!(
            frame_time.rate_limit_reset_local_utc_offset_seconds(),
            -25_200
        );
    }
}
