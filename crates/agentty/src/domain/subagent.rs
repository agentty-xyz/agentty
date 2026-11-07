//! Domain model for the user-selectable subagent concurrency cap.

use std::num::NonZeroUsize;

/// Concurrent provider-native subagents one harness turn may run.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum SubagentLimit {
    /// One subagent at a time.
    One,
    /// Two concurrent subagents.
    #[default]
    Two,
    /// Four concurrent subagents.
    Four,
    /// Eight concurrent subagents.
    Eight,
}

impl SubagentLimit {
    /// All selectable limits in settings display order.
    pub const ALL: [Self; 4] = [Self::One, Self::Two, Self::Four, Self::Eight];

    /// Returns the concurrency cap passed to worker execution policies.
    #[must_use]
    pub fn limit(self) -> NonZeroUsize {
        let count = match self {
            Self::One => 1,
            Self::Two => 2,
            Self::Four => 4,
            Self::Eight => 8,
        };

        NonZeroUsize::new(count).unwrap_or(NonZeroUsize::MIN)
    }

    /// Returns the persisted wire value for this limit.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::One => "1",
            Self::Two => "2",
            Self::Four => "4",
            Self::Eight => "8",
        }
    }

    /// Parses a persisted limit.
    ///
    /// Returns `None` for unknown values so callers can fall back to
    /// [`SubagentLimit::default`].
    #[must_use]
    pub fn parse_persisted(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|limit| limit.as_str() == value)
    }
}

#[cfg(test)]
#[path = "subagent_test.rs"]
mod tests;
