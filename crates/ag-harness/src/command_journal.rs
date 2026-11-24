//! Command intents and outcomes are separate from single-file patch records.

use std::fmt;
use std::path::PathBuf;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::store::TurnOwner;

/// Frozen invocation stored before a sandbox can spawn. Environment values are
/// excluded; policy contains their names and host-supplied revision.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
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

/// Recorded command result. Exit, timeout, output, and cleanup are independent.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CommandOutcome {
    /// Whether the executor could not acknowledge its cleanup scope.
    pub cleanup_failed: bool,
    /// Scope of cleanup; a best-effort scope never proves detached
    /// descendants stopped.
    pub cleanup_scope: CommandCleanupScope,
    /// Content-free execution error classification, if any.
    pub execution_failure: Option<crate::bash::BashError>,
    /// How the command ended and its main shell exited, encoded as
    /// top-level fields.
    #[serde(flatten)]
    pub exit: CommandExit,
    /// Captured standard error, decoded with UTF-8 replacement; its start
    /// when bytes were omitted.
    pub stderr: String,
    /// Standard-error bytes omitted between `stderr` and `stderr_tail`.
    #[serde(default)]
    pub stderr_omitted_bytes: u64,
    /// End of standard error, empty unless bytes were omitted.
    #[serde(default)]
    pub stderr_tail: String,
    /// Captured standard output, decoded with UTF-8 replacement; its start
    /// when bytes were omitted.
    pub stdout: String,
    /// Standard-output bytes omitted between `stdout` and `stdout_tail`.
    #[serde(default)]
    pub stdout_omitted_bytes: u64,
    /// End of standard output, empty unless bytes were omitted.
    #[serde(default)]
    pub stdout_tail: String,
    /// Whether the combined capture budget omitted any bytes. Records from
    /// before head-and-tail capture kept only an arrival-order prefix and
    /// report no omitted counts.
    pub truncated: bool,
}

/// How a `bash` command ended and its main shell exited, shared by its
/// [`CommandOutcome`], tool activity, live tool-call item, and lifecycle
/// event. Each field defaults when absent, so records from before a field
/// existed still decode.
#[derive(Clone, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CommandExit {
    /// Main shell exit code, when observed.
    #[serde(default)]
    pub exit_code: Option<i32>,
    /// Exit statuses of each stage of the command's final pipeline, first
    /// stage first, when it had more than one; `exit_code` stays the
    /// shell's own status. Empty for any other command, when the statuses
    /// could not be observed, when the turn's `bash` has no temporary
    /// directory to receive them, and for an excluded command run without a
    /// shell. Display-only and model-influenceable: the command can forge
    /// or suppress it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pipe_status: Vec<i32>,
    /// Main shell terminating signal, when observed.
    #[serde(default)]
    pub signal: Option<i32>,
    /// Overall completion reason, independent of the main shell exit: a
    /// command that ran past its deadline, was cancelled, or failed to run
    /// is not [`CommandTermination::Completed`] whatever its exit code.
    /// `Completed` in reports recorded before it existed.
    #[serde(default)]
    pub termination: CommandTermination,
}

impl CommandExit {
    /// Returns a completed exit with `exit_code`, `signal`, and no pipeline
    /// statuses.
    pub fn new(exit_code: Option<i32>, signal: Option<i32>) -> Self {
        Self {
            exit_code,
            pipe_status: Vec::new(),
            signal,
            termination: CommandTermination::Completed,
        }
    }

    /// Returns this exit with the final pipeline's `pipe_status`.
    #[must_use]
    pub fn with_pipe_status(mut self, pipe_status: Vec<i32>) -> Self {
        self.pipe_status = pipe_status;

        self
    }

    /// Returns this exit with how the command ended.
    #[must_use]
    pub fn with_termination(mut self, termination: CommandTermination) -> Self {
        self.termination = termination;

        self
    }
}

impl fmt::Display for CommandExit {
    /// Writes `exit 101`, `signal 9`, or `exit 0, pipeline 101 0`, after
    /// `timed out`, `cancelled`, or `failed to run` when the command did not
    /// complete; nothing for a completed command with neither an exit code
    /// nor a signal.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut parts = Vec::new();
        match self.termination {
            CommandTermination::Completed => {}
            CommandTermination::Deadline => parts.push("timed out".to_string()),
            CommandTermination::Cancelled => parts.push("cancelled".to_string()),
            CommandTermination::Failed => parts.push("failed to run".to_string()),
        }
        match (self.exit_code, self.signal) {
            (Some(code), _) => parts.push(format!("exit {code}")),
            (None, Some(signal)) => parts.push(format!("signal {signal}")),
            (None, None) => {}
        }
        if !self.pipe_status.is_empty() {
            let statuses = self
                .pipe_status
                .iter()
                .map(i32::to_string)
                .collect::<Vec<_>>()
                .join(" ");
            parts.push(format!("pipeline {statuses}"));
        }

        formatter.write_str(&parts.join(", "))
    }
}

/// Completion scope of the selected executor, carried with every result and
/// durable record.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandCleanupScope {
    /// Linux PID namespace termination includes detached descendants.
    PidNamespace,
    /// Process-group observation and cleanup are best effort. Descendants
    /// that escape the group may keep executing with whatever isolation the
    /// selected executor documents — the native macOS sandbox leaves them
    /// their inherited Seatbelt policy, while the unsandboxed executor
    /// applies none.
    ProcessGroupBestEffort,
}

/// Overall execution reason; this never overwrites a known main exit.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum CommandTermination {
    /// The backend reported completion in its documented scope.
    #[default]
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
#[non_exhaustive]
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
    /// Constructs an unobserved invocation. The store must assign the ID
    /// atomically while validating the live owner.
    pub(crate) fn pending(id: i64, owner: TurnOwner, intent: CommandIntent) -> Self {
        Self {
            id,
            intent,
            outcome: None,
            reconciled: false,
            owner,
        }
    }

    /// Original owner, retained across expiry, interruption, and reopen.
    pub(crate) fn owner(&self) -> &TurnOwner {
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
