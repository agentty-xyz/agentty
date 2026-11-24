//! Provider-observed activity for display, independent of telemetry capture.

use std::time::{Duration, SystemTime};

/// A bounded display snapshot of one observed provider operation.
///
/// Match updates by `(attempt_id, id)`. Retries use new attempt identities.
/// Names describe tools, MCP `server/tool` pairs, or explicitly invoked
/// skills, never inferred file reads. Optional input and output previews are
/// display-only: they must not be replayed to providers or implicitly exported
/// as telemetry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivityEvent {
    /// Unique provider attempt identity, including retries and repairs.
    pub attempt_id: String,
    /// Process exit code when the provider reports one.
    pub exit_code: Option<i64>,
    /// Structured, display-only facts the provider reported about the
    /// operation, boxed to keep events small.
    pub facts: Box<ActivityFacts>,
    /// Provider operation identity within the attempt.
    pub id: String,
    /// Bounded command or argument preview, when reported by the provider.
    pub input: Option<String>,
    /// Provider-independent operation category.
    pub kind: ActivityKind,
    /// Bounded tool, MCP `server/tool`, or skill identity suitable for display.
    pub name: String,
    /// Time this snapshot was observed by the adapter.
    pub observed_at: SystemTime,
    /// Bounded result preview, updated as output arrives when supported. An
    /// adapter may fill it only for failed operations, with the result's
    /// last lines.
    pub output: Option<String>,
    /// Provider parent operation identity, scoped to the same attempt.
    pub parent_id: Option<String>,
    /// Latest lifecycle state the provider reported; see [`Self::outcome`]
    /// for the state to display.
    pub status: ActivityStatus,
}

impl ActivityEvent {
    /// Exit status of a pipeline stage that `SIGPIPE` ended because a later
    /// stage stopped reading, as `git log | head` reports for `git log`.
    const SIGPIPE_STATUS: i64 = 128 + 13;

    /// Returns the state to display: the provider's [`Self::status`], except
    /// that a completed operation whose own exit status is nonzero, or that
    /// a signal terminated, failed.
    ///
    /// A [`ActivityKind::Search`] that exits with 1 found no match and stays
    /// completed. Every other exit status, including 1 from a search run as
    /// an [`ActivityKind::Command`], is judged as is.
    pub fn outcome(&self) -> ActivityStatus {
        let exit_code = self.effective_exit_code();
        let found_nothing = self.kind == ActivityKind::Search && exit_code == Some(1);
        let failed = self.facts.signal.is_some()
            || (!found_nothing && exit_code.is_some_and(|code| code != 0));
        if self.status == ActivityStatus::Completed && failed {
            ActivityStatus::Failed
        } else {
            self.status
        }
    }

    /// Returns the operation's own exit status: the first nonzero status of
    /// its final pipeline's stages, so `cargo test | tail` reports the
    /// status of `cargo test`, else [`Self::exit_code`]. A stage before the
    /// last that exited with 141 (`SIGPIPE`) only stopped because a later
    /// stage stopped reading, so it is skipped.
    pub fn effective_exit_code(&self) -> Option<i64> {
        let stages = &self.facts.pipe_status;
        let last = stages.len().saturating_sub(1);
        stages
            .iter()
            .enumerate()
            .find(|(index, status)| {
                **status != 0 && !(*index < last && **status == Self::SIGPIPE_STATUS)
            })
            .map(|(_, status)| *status)
            .or(self.exit_code)
    }
}

/// Structured facts about one operation, each present only when the
/// provider reports it. Text fields are bounded single lines.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ActivityFacts {
    /// Lines a file change added and removed.
    pub diff_stat: Option<DiffStat>,
    /// Time the operation took.
    pub duration: Option<Duration>,
    /// Why a failed operation failed, when the provider says.
    pub failure: Option<ActivityFailure>,
    /// Line range a file read covered.
    pub lines: Option<LineRange>,
    /// Exit statuses of each stage of a command's final pipeline, first
    /// stage first, when it had more than one. Display-only: the command can
    /// influence it.
    pub pipe_status: Vec<i64>,
    /// First line of the provider's explanation of a failure, such as
    /// `hunk 1 context mismatch at line 1216`.
    pub reason: Option<String>,
    /// Signal that terminated a command.
    pub signal: Option<i64>,
    /// What the operation acted on: a path, a search query, or a command
    /// flattened to one line.
    pub target: Option<String>,
}

/// Why an operation failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityFailure {
    /// The user or an approval policy denied it, so it did not run.
    Denied,
    /// It ran, or tried to, and failed, such as an execution error or a run
    /// that ended in an error while it was open.
    Error,
    /// The tool refused the call, such as malformed arguments, a path
    /// outside the repository, or a patch that does not apply.
    Rejected,
    /// It took too long: a command hit its deadline, or an approval was not
    /// decided in time.
    TimedOut,
    /// The tool does not exist or the turn does not offer it.
    Unavailable,
}

/// Lines a file change added and removed.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct DiffStat {
    /// Lines added.
    pub added: u64,
    /// Lines removed.
    pub removed: u64,
}

/// One-based line range of a file read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineRange {
    /// First line read.
    pub first: u64,
    /// Last line read, when the read was limited.
    pub last: Option<u64>,
}

/// Category of explicitly observed activity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityKind {
    /// Command execution.
    Command,
    /// File modification.
    FileChange,
    /// Directory or path listing.
    List,
    /// Model Context Protocol server tool invocation, named `server/tool`.
    Mcp,
    /// File content read.
    Read,
    /// Repository text or file-name search.
    Search,
    /// Named skill invocation.
    Skill,
    /// Built-in, dynamic, or subagent tool invocation.
    Tool,
    /// Provider web search.
    WebSearch,
}

impl ActivityKind {
    /// Returns whether the kind only inspects the repository: a read, a
    /// search, or a listing.
    pub fn is_exploration(self) -> bool {
        matches!(self, Self::List | Self::Read | Self::Search)
    }
}

/// Lifecycle state for an observed operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityStatus {
    /// Provider reported that the operation started.
    Running,
    /// Provider reported successful completion.
    Completed,
    /// Provider reported failure.
    Failed,
    /// Attempt ended without an observed terminal result, including
    /// cancellation.
    Interrupted,
}

#[cfg(test)]
#[path = "activity_test.rs"]
mod tests;
