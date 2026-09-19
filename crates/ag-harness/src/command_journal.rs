//! Command intents and outcomes are separate from single-file patch records.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::TurnOwner;

/// Frozen invocation stored before a sandbox can spawn. Environment values are
/// excluded; policy contains their names and host-supplied revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandIntent {
    /// Provider tool-call identifier.
    pub call_id: String,
    /// Validated shell source; sensitive historical content, never telemetry.
    pub command: String,
    /// Effective host policy snapshot without environment values.
    pub policy: Value,
    /// Canonical workspace for this invocation.
    pub workspace: PathBuf,
}

/// Recorded sandbox result. Exit, timeout, output, and cleanup are independent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CommandOutcome {
    /// Whether the backend could not acknowledge its cleanup scope.
    pub cleanup_failed: bool,
    /// Scope of cleanup; macOS never proves detached descendants stopped.
    pub cleanup_scope: CommandCleanupScope,
    /// Content-free execution error classification, if any.
    pub execution_failure: Option<crate::BashError>,
    /// Main shell exit code, when observed.
    pub exit_code: Option<i32>,
    /// Main shell terminating signal, when observed.
    pub signal: Option<i32>,
    /// Captured standard error, decoded with UTF-8 replacement.
    pub stderr: String,
    /// Captured standard output, decoded with UTF-8 replacement.
    pub stdout: String,
    /// Overall completion reason, independent of the main shell exit.
    pub termination: CommandTermination,
    /// Whether the combined capture budget discarded any bytes.
    pub truncated: bool,
}

/// Native completion scope, carried with every result and durable record.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandCleanupScope {
    /// Linux PID namespace termination includes detached descendants.
    PidNamespace,
    /// macOS process-group observation and cleanup are best effort. Detached
    /// descendants may still execute within their inherited Seatbelt policy.
    ProcessGroupBestEffort,
}

/// Overall execution reason; this never overwrites a known main exit.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandTermination {
    /// The backend reported completion in its documented scope.
    Completed,
    /// The host's original execution deadline elapsed.
    Deadline,
    /// The caller cancelled or dropped execution.
    Cancelled,
    /// Preparation or execution failed.
    Failed,
}

/// An invocation and its observed result. `None` is an unknown outcome, never
/// permission to replay. Reconciliation does not fabricate a missing result.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandRecord {
    /// Store-assigned command identifier.
    pub id: i64,
    /// Immutable invocation committed before spawning.
    pub intent: CommandIntent,
    /// Observed result; absent after interrupted or failed recording.
    pub outcome: Option<CommandOutcome>,
    /// Explicit host reconciliation of a pending or unresolved command.
    pub reconciled: bool,
    pub(crate) owner: TurnOwner,
}

impl CommandRecord {
    /// Constructs an unobserved invocation for a host-implemented store. The
    /// store must assign the ID atomically while validating the live owner.
    pub fn pending(id: i64, owner: TurnOwner, intent: CommandIntent) -> Self {
        Self {
            id,
            intent,
            outcome: None,
            reconciled: false,
            owner,
        }
    }

    /// Original owner, retained across expiry, interruption, and reopen.
    pub fn owner(&self) -> &TurnOwner {
        &self.owner
    }

    /// Whether this record still blocks admission of a new turn.
    pub fn blocks_admission(&self) -> bool {
        !self.reconciled
            && self
                .outcome
                .as_ref()
                .is_none_or(|outcome| outcome.cleanup_failed)
    }
}

#[cfg(test)]
#[path = "command_journal_test.rs"]
mod tests;
