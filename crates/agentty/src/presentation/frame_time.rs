//! Deterministic wall-clock values shared by one frontend frame.

/// One coherent wall-clock snapshot projected to a frontend render pass.
///
/// Keeping seconds, subsecond animation time, and the local UTC offset
/// together prevents one frame from mixing multiple host-clock reads.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct FrameTime {
    local_utc_offset_seconds: i64,
    /// Offset the host clock resolved for one displayed past timestamp, such
    /// as the visible session's start, stored as `(timestamp, offset)`.
    resolved_utc_offset: Option<(i64, i64)>,
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
            resolved_utc_offset: None,
            unix_millis,
            unix_seconds,
        }
    }

    /// Records the local UTC offset the clock boundary resolved for
    /// `timestamp_seconds`, which can differ from the frame offset across
    /// daylight-saving transitions.
    #[must_use]
    pub(crate) const fn with_local_utc_offset_at(
        mut self,
        timestamp_seconds: i64,
        utc_offset_seconds: i64,
    ) -> Self {
        self.resolved_utc_offset = Some((timestamp_seconds, utc_offset_seconds));

        self
    }

    /// Returns the local UTC offset active at this frame timestamp.
    pub(crate) const fn local_utc_offset_seconds(self) -> i64 {
        self.local_utc_offset_seconds
    }

    /// Returns the local UTC offset for `timestamp_seconds`, falling back to
    /// the frame offset when no offset was resolved for that timestamp.
    pub(crate) fn local_utc_offset_seconds_at(self, timestamp_seconds: i64) -> i64 {
        match self.resolved_utc_offset {
            Some((resolved_timestamp, utc_offset_seconds))
                if resolved_timestamp == timestamp_seconds =>
            {
                utc_offset_seconds
            }
            _ => self.local_utc_offset_seconds,
        }
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
#[path = "frame_time_test.rs"]
mod tests;
