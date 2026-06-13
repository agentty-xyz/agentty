//! In-memory structured system log events for the current Agentty process.

use std::collections::VecDeque;

/// Maximum number of system log entries retained for the current process.
pub const SYSTEM_LOG_LIMIT: usize = 1000;

/// Source area associated with a system log entry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemLogCategory {
    /// Forge operations such as pull-request or merge-request refreshes.
    Forge,
    /// Git synchronization and branch status operations.
    Git,
    /// Session lifecycle and status changes.
    Session,
    /// Manual or background project synchronization.
    Sync,
    /// Process-level startup and housekeeping events.
    System,
    /// Version-check and self-update operations.
    Update,
}

impl SystemLogCategory {
    /// Returns the compact label rendered in the logs page.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Forge => "forge",
            Self::Git => "git",
            Self::Session => "session",
            Self::Sync => "sync",
            Self::System => "system",
            Self::Update => "update",
        }
    }
}

/// Severity used to highlight system log entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SystemLogLevel {
    /// Informational lifecycle or background-work event.
    Info,
    /// Successful completion of a notable operation.
    Success,
    /// Recoverable failure or degraded workflow.
    Warning,
    /// Operation failure that needs user attention.
    Error,
}

impl SystemLogLevel {
    /// Returns the compact uppercase label rendered in the logs page.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Info => "INFO",
            Self::Success => "OK",
            Self::Warning => "WARN",
            Self::Error => "ERR",
        }
    }
}

/// Untimestamped system log payload emitted by app and background producers.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemLogEvent {
    /// Source area associated with this event.
    pub category: SystemLogCategory,
    /// Optional secondary detail rendered after the primary message.
    pub detail: Option<String>,
    /// Highlight severity for this event.
    pub level: SystemLogLevel,
    /// Primary human-readable event text.
    pub message: String,
}

impl SystemLogEvent {
    /// Creates a structured system log event.
    pub fn new(
        level: SystemLogLevel,
        category: SystemLogCategory,
        message: impl Into<String>,
    ) -> Self {
        Self {
            category,
            detail: None,
            level,
            message: message.into(),
        }
    }

    /// Attaches secondary detail to the event.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());

        self
    }
}

/// Timestamped system log entry stored in the process-local buffer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SystemLogEntry {
    /// Source area associated with this entry.
    pub category: SystemLogCategory,
    /// Optional secondary detail rendered after the primary message.
    pub detail: Option<String>,
    /// Highlight severity for this entry.
    pub level: SystemLogLevel,
    /// Primary human-readable entry text.
    pub message: String,
    /// Process-local monotonically increasing sequence number.
    pub sequence: u64,
    /// Wall-clock Unix timestamp in seconds recorded by the reducer.
    pub timestamp_unix_seconds: i64,
}

/// Process-local bounded system log buffer.
pub struct SystemLogBuffer {
    entries: VecDeque<SystemLogEntry>,
    next_sequence: u64,
}

impl SystemLogBuffer {
    /// Returns the entries retained in chronological order.
    pub fn entries(&self) -> &VecDeque<SystemLogEntry> {
        &self.entries
    }

    /// Returns whether the buffer currently has no entries.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the number of retained entries.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Records one event with a reducer-supplied timestamp and trims the
    /// oldest entries when the process-local limit is exceeded.
    pub fn push(&mut self, timestamp_unix_seconds: i64, event: SystemLogEvent) {
        self.entries.push_back(SystemLogEntry {
            category: event.category,
            detail: event.detail,
            level: event.level,
            message: event.message,
            sequence: self.next_sequence,
            timestamp_unix_seconds,
        });
        self.next_sequence = self.next_sequence.saturating_add(1);

        while self.entries.len() > SYSTEM_LOG_LIMIT {
            self.entries.pop_front();
        }
    }
}

impl Default for SystemLogBuffer {
    /// Creates an empty process-local system log buffer.
    fn default() -> Self {
        Self {
            entries: VecDeque::with_capacity(SYSTEM_LOG_LIMIT),
            next_sequence: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verifies pushed events are timestamped and sequenced in insertion
    /// order.
    #[test]
    fn push_records_timestamped_entries_in_order() {
        // Arrange
        let mut buffer = SystemLogBuffer::default();

        // Act
        buffer.push(
            10,
            SystemLogEvent::new(
                SystemLogLevel::Info,
                SystemLogCategory::System,
                "Agentty started",
            ),
        );
        buffer.push(
            11,
            SystemLogEvent::new(
                SystemLogLevel::Success,
                SystemLogCategory::Sync,
                "Sync complete",
            )
            .with_detail("main"),
        );

        // Assert
        let entries = buffer.entries().iter().collect::<Vec<_>>();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].sequence, 0);
        assert_eq!(entries[0].timestamp_unix_seconds, 10);
        assert_eq!(entries[0].message, "Agentty started");
        assert_eq!(entries[1].sequence, 1);
        assert_eq!(entries[1].detail.as_deref(), Some("main"));
    }

    /// Verifies the bounded buffer drops oldest entries past the retention
    /// limit.
    #[test]
    fn push_purges_oldest_entries_after_limit() {
        // Arrange
        let mut buffer = SystemLogBuffer::default();

        // Act
        for index in 0..=SYSTEM_LOG_LIMIT {
            let timestamp = i64::try_from(index).expect("test index should fit in i64");
            buffer.push(
                timestamp,
                SystemLogEvent::new(
                    SystemLogLevel::Info,
                    SystemLogCategory::System,
                    format!("event {index}"),
                ),
            );
        }

        // Assert
        assert_eq!(buffer.len(), SYSTEM_LOG_LIMIT);
        assert_eq!(
            buffer.entries().front().map(|entry| entry.sequence),
            Some(1)
        );
        assert_eq!(
            buffer.entries().back().map(|entry| entry.sequence),
            Some(SYSTEM_LOG_LIMIT as u64)
        );
    }
}
