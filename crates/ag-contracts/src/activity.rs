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
    /// Returns the state to display: the provider's [`Self::status`], except
    /// that a completed command whose own exit status is nonzero, or that a
    /// signal terminated, failed.
    ///
    /// A search command such as `grep` or `rg` that found no match exits
    /// with 1 and counts as failed too; a host that wants to soften it can
    /// compare [`Self::effective_exit_code`] with 1.
    pub fn outcome(&self) -> ActivityStatus {
        let command_failed =
            self.facts.signal.is_some() || self.effective_exit_code().is_some_and(|code| code != 0);
        if self.status == ActivityStatus::Completed && command_failed {
            ActivityStatus::Failed
        } else {
            self.status
        }
    }

    /// Returns the command's own exit status: the first nonzero status of
    /// its final pipeline's stages, so `cargo test | tail` reports the
    /// status of `cargo test`, else [`Self::exit_code`].
    pub fn effective_exit_code(&self) -> Option<i64> {
        self.facts
            .pipe_status
            .iter()
            .copied()
            .find(|status| *status != 0)
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
    /// Line range a file read covered.
    pub lines: Option<LineRange>,
    /// Exit statuses of each stage of a command's final pipeline, first
    /// stage first, when it had more than one. Display-only: the command can
    /// influence it.
    pub pipe_status: Vec<i64>,
    /// First line of why the operation was rejected or failed.
    pub reason: Option<String>,
    /// Signal that terminated a command.
    pub signal: Option<i64>,
    /// What the operation acted on: a path, a search query, or a command's
    /// first line.
    pub target: Option<String>,
}

impl ActivityFacts {
    /// Longest text a bounded fact keeps, in characters.
    pub const TEXT_LIMIT: usize = 240;

    /// Returns the first nonblank line of `text` without control
    /// characters, cut to [`Self::TEXT_LIMIT`] characters with a trailing
    /// `…`, or `None` when `text` is blank.
    pub fn bounded_line(text: &str) -> Option<String> {
        let line = text.lines().map(str::trim).find(|line| !line.is_empty())?;
        let mut characters = line.chars().filter(|character| !character.is_control());
        let mut bounded: String = characters.by_ref().take(Self::TEXT_LIMIT).collect();
        if characters.next().is_some() {
            bounded.push('…');
        }

        Some(bounded)
    }
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

impl LineRange {
    /// Returns the range a read starting at one-based line `offset` and
    /// reading at most `limit` lines covers, or `None` when the read named
    /// neither and covered the whole file. A zero is treated as absent.
    pub fn from_window(offset: Option<u64>, limit: Option<u64>) -> Option<Self> {
        let offset = offset.filter(|offset| *offset > 0);
        let limit = limit.filter(|limit| *limit > 0);
        if offset.is_none() && limit.is_none() {
            return None;
        }
        let first = offset.unwrap_or(1);

        Some(Self {
            first,
            last: limit.map(|limit| first.saturating_add(limit - 1)),
        })
    }
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
