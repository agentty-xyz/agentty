//! Turn configuration, results, and control.
//!
//! [`TurnOptions`] replace harness defaults for one turn. A finished turn
//! returns a [`TurnOutcome`] whose [`TurnReport`] lists model requests and tool
//! activity. [`SessionTurn`] and [`OneShotTurn`] configure a turn before it
//! runs; `start` returns a [`ControlledTurn`] whose [`TurnControl`] cancels it
//! and observes settlement.

use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

use crate::bash::{BashConfig, BashError};
pub use crate::cancellation::{ControlledTurn, SettlementError, TurnControl};
use crate::comparison::ComparisonBase;
pub use crate::effect::EffectSettlementError;
pub use crate::harness::{OneShotTurn, SessionTurn};
use crate::lifecycle::{ModelResponseType, TurnErrorType};
use crate::model::{CompletionMetadata, ModelError};
use crate::policy::ToolPolicy;
use crate::read::ReadError;
use crate::schema_contract::OutputSchema;
use crate::tool::ReadAction;
use crate::write::WriteError;

/// Fully resolved configuration, fixed for the lifetime of one engine run.
///
/// Construct a new value for each turn. Explicit options never inherit
/// permissions or schema changes from an earlier turn. Counters, cancellation,
/// and conversation history belong to execution state, not this snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnOptions {
    bash: Option<BashConfig>,
    comparison_base: Option<ComparisonBase>,
    limits: TurnLimits,
    schema: OutputSchema,
    tool_policy: ToolPolicy,
}

impl TurnOptions {
    /// Resolves a required schema, explicit permissions, and execution limits.
    pub fn new(schema: OutputSchema, tool_policy: ToolPolicy, limits: TurnLimits) -> Self {
        Self {
            bash: None,
            comparison_base: None,
            limits,
            schema,
            tool_policy,
        }
    }

    /// Selects immutable Bash host policy for this turn. The separate Bash
    /// tool permission must also be enabled. Does not change session defaults.
    #[must_use]
    pub fn with_bash(mut self, configuration: BashConfig) -> Self {
        self.bash = Some(configuration);

        self
    }

    /// Returns this turn's Bash host policy, if configured.
    pub fn bash(&self) -> Option<&BashConfig> {
        self.bash.as_ref()
    }

    /// Returns the execution bounds for this turn.
    pub fn limits(&self) -> TurnLimits {
        self.limits
    }

    /// Returns the schema required for every terminal model output.
    pub fn schema(&self) -> &OutputSchema {
        &self.schema
    }

    /// Returns the complete permissions for this turn.
    pub fn tool_policy(&self) -> ToolPolicy {
        self.tool_policy
    }

    /// Sets the host-validated comparison base for this turn only.
    #[must_use]
    pub fn with_comparison_base(mut self, base: ComparisonBase) -> Self {
        self.comparison_base = Some(base);

        self
    }

    /// Returns the selected comparison base, if comparisons are available.
    pub fn comparison_base(&self) -> Option<&ComparisonBase> {
        self.comparison_base.as_ref()
    }
}

/// Immutable execution bounds; consuming a budget does not modify these limits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TurnLimits {
    max_tool_calls: NonZeroUsize,
}

impl TurnLimits {
    /// Sets the total permitted tool calls, including calls in batches.
    pub fn new(max_tool_calls: NonZeroUsize) -> Self {
        Self { max_tool_calls }
    }

    /// Returns the total permitted tool calls in a turn.
    pub fn max_tool_calls(self) -> NonZeroUsize {
        self.max_tool_calls
    }
}

impl Default for TurnLimits {
    fn default() -> Self {
        Self::new(NonZeroUsize::new(8).unwrap_or(NonZeroUsize::MIN))
    }
}

/// Successful model turn paired with observable execution activity.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TurnOutcome {
    output: Value,
    report: TurnReport,
}

impl TurnOutcome {
    /// Returns the locally validated structured model output.
    pub fn output(&self) -> &Value {
        &self.output
    }

    /// Returns sanitized timing, model, and tool activity for the turn.
    pub fn report(&self) -> &TurnReport {
        &self.report
    }

    /// Consumes the outcome and returns its validated output.
    pub fn into_output(self) -> Value {
        self.output
    }

    pub(crate) fn new(output: Value, report: TurnReport) -> Self {
        Self { output, report }
    }

    pub(crate) fn set_duration(&mut self, duration: Duration) {
        self.report.duration = duration;
    }

    pub(crate) fn set_history(&mut self, history: HistoryActivity) {
        self.report.history = history;
    }
}

/// Observable, content-free activity from one successful model turn.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct TurnReport {
    duration: Duration,
    #[serde(default)]
    history: HistoryActivity,
    model_requests: Vec<ModelRequestActivity>,
    tool_calls: Vec<ToolActivity>,
}

impl TurnReport {
    /// Returns the complete elapsed turn time, including persistence for
    /// legacy durable session turns. Host-ID submissions retain the engine
    /// duration captured before persistence so recovered reports stay
    /// identical.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns how the durable session history was projected into the turn's
    /// initial request. One-shot turns and reports recorded before this
    /// field existed report no replayed, evicted, or summarized history.
    pub fn history(&self) -> HistoryActivity {
        self.history
    }

    /// Returns one entry for every provider request made during the turn.
    pub fn model_requests(&self) -> &[ModelRequestActivity] {
        &self.model_requests
    }

    /// Returns successful repository tool activity without file contents.
    pub fn tool_calls(&self) -> &[ToolActivity] {
        &self.tool_calls
    }

    pub(crate) fn new(
        duration: Duration,
        model_requests: Vec<ModelRequestActivity>,
        tool_calls: Vec<ToolActivity>,
    ) -> Self {
        Self {
            duration,
            history: HistoryActivity::default(),
            model_requests,
            tool_calls,
        }
    }
}

/// Content-free facts about how loaded session history entered a turn's
/// initial request, so hosts can observe context loss instead of inferring
/// it from model answers.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Deserialize, Serialize)]
pub struct HistoryActivity {
    checkpoint_replayed: bool,
    evicted_turns: usize,
    replayed_turns: usize,
}

impl HistoryActivity {
    /// Whether a checkpoint summary was replayed ahead of the retained turns.
    /// False when the session has no checkpoint or the summary did not fit
    /// the context budget.
    pub fn checkpoint_replayed(self) -> bool {
        self.checkpoint_replayed
    }

    /// Loaded turns the context budget evicted from the initial request. The
    /// byte-based replay budget bounds loading itself and is not counted.
    pub fn evicted_turns(self) -> usize {
        self.evicted_turns
    }

    /// Loaded turns replayed in the initial request.
    pub fn replayed_turns(self) -> usize {
        self.replayed_turns
    }

    pub(crate) fn new(
        checkpoint_replayed: bool,
        evicted_turns: usize,
        replayed_turns: usize,
    ) -> Self {
        Self {
            checkpoint_replayed,
            evicted_turns,
            replayed_turns,
        }
    }
}

/// Observable facts about one provider request in a successful turn.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub struct ModelRequestActivity {
    completion: Option<CompletionMetadata>,
    duration: Duration,
    response_type: ModelResponseType,
}

impl ModelRequestActivity {
    /// Returns sanitized provider completion metadata, when available.
    pub fn completion(&self) -> Option<&CompletionMetadata> {
        self.completion.as_ref()
    }

    /// Returns the elapsed provider-request time.
    pub fn duration(&self) -> Duration {
        self.duration
    }

    /// Returns whether the request produced output, a tool call, or a rejected
    /// native continuation that the harness replayed.
    pub fn response_type(&self) -> ModelResponseType {
        self.response_type
    }

    pub(crate) fn new(
        completion: Option<CompletionMetadata>,
        duration: Duration,
        response_type: ModelResponseType,
    ) -> Self {
        Self {
            completion,
            duration,
            response_type,
        }
    }
}

/// Sanitized details about one built-in tool operation.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, Serialize)]
#[non_exhaustive]
pub enum ToolActivity {
    /// Content-free shell activity. Command text and output are never reported.
    Bash {
        /// Time spent executing and observing the sandbox.
        duration: Duration,
    },
    /// A bounded repository file read.
    Read {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Final included one-based line, when the file was nonempty.
        end_line: Option<u64>,
        /// Repository-relative path that was read.
        path: String,
        /// Requested one-based starting line.
        start_line: u64,
        /// Whether additional file content followed the result.
        truncated: bool,
    },
    /// A read-only repository inspection other than a worktree file read.
    ReadInspection {
        /// Selected inspection action.
        action: ReadAction,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Bounded path, query, or revision summary.
        summary: String,
    },
    /// A model-correctable repository inspection rejection returned to the
    /// model.
    ReadInspectionRejected {
        /// Selected inspection action.
        action: ReadAction,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Bounded path, query, or revision summary.
        summary: String,
    },
    /// A model-correctable repository read rejection returned to the model.
    ReadRejected {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was rejected.
        path: String,
    },
    /// A repository file write.
    Write {
        /// Number of bytes in the resulting file.
        bytes_written: usize,
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was written.
        path: String,
    },
    /// A model-correctable repository write rejection returned to the model.
    WriteRejected {
        /// Elapsed tool-execution time.
        duration: Duration,
        /// Repository-relative path that was rejected.
        path: String,
    },
}

impl ToolActivity {
    /// Returns the elapsed tool-execution time.
    pub fn duration(&self) -> Duration {
        match self {
            Self::Bash { duration }
            | Self::Read { duration, .. }
            | Self::ReadInspection { duration, .. }
            | Self::ReadInspectionRejected { duration, .. }
            | Self::ReadRejected { duration, .. }
            | Self::Write { duration, .. }
            | Self::WriteRejected { duration, .. } => *duration,
        }
    }

    /// Returns the bounded built-in tool name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Read { .. }
            | Self::ReadInspection { .. }
            | Self::ReadInspectionRejected { .. }
            | Self::ReadRejected { .. } => "read",
            Self::Write { .. } | Self::WriteRejected { .. } => "write",
            Self::Bash { .. } => "bash",
        }
    }

    /// Returns the repository-relative target or bounded inspection summary.
    pub fn path(&self) -> &str {
        match self {
            Self::Bash { .. } => "",
            Self::Read { path, .. }
            | Self::ReadRejected { path, .. }
            | Self::Write { path, .. }
            | Self::WriteRejected { path, .. } => path,
            Self::ReadInspection { summary, .. } | Self::ReadInspectionRejected { summary, .. } => {
                summary
            }
        }
    }
}

impl fmt::Display for ToolActivity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bash { duration } => {
                write!(formatter, "bash ({})", format_report_duration(*duration))
            }
            Self::Read {
                duration,
                end_line,
                path,
                start_line,
                truncated,
            } => {
                let path = sanitize_report_text(path);
                let lines = end_line.map_or_else(
                    || format!("line {start_line}"),
                    |end_line| format!("lines {start_line}-{end_line}"),
                );
                let continuation = if *truncated { ", truncated" } else { "" };

                write!(
                    formatter,
                    "read {path} ({lines}{continuation}; {})",
                    format_report_duration(*duration)
                )
            }
            Self::ReadInspection {
                action,
                duration,
                summary,
            } => write!(
                formatter,
                "read {} {} (completed; {})",
                action.as_str(),
                sanitize_report_text(summary),
                format_report_duration(*duration)
            ),
            Self::ReadInspectionRejected {
                action,
                duration,
                summary,
            } => write!(
                formatter,
                "read {} {} (rejected; {})",
                action.as_str(),
                sanitize_report_text(summary),
                format_report_duration(*duration)
            ),
            Self::ReadRejected { duration, path } => write!(
                formatter,
                "read {} (rejected; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
            Self::Write {
                bytes_written,
                duration,
                path,
            } => write!(
                formatter,
                "write {} ({bytes_written} bytes; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
            Self::WriteRejected { duration, path } => write!(
                formatter,
                "write {} (rejected; {})",
                sanitize_report_text(path),
                format_report_duration(*duration)
            ),
        }
    }
}

/// Failure returned by a complete harness turn.
#[derive(Debug, Error)]
pub enum TurnError {
    /// Sandbox preparation, execution, or cleanup failed. Retains all observed
    /// diagnostics, including simultaneous execution and cleanup failures.
    #[error("sandbox command failed")]
    CommandFailed {
        /// Normalized observed outcome; command content is never telemetry.
        outcome: Box<crate::bash::CommandOutcome>,
    },
    /// Command persistence failed; the observed execution result is retained
    /// separately when available, including cleanup and main-exit diagnostics.
    #[error("command journal failed: {source}")]
    CommandJournal {
        /// Observed result; absent when intent persistence prevented execution.
        outcome: Option<Box<crate::bash::CommandOutcome>>,
        /// Underlying transactional store error.
        #[source]
        source: Box<crate::SessionError>,
    },
    /// A sandbox policy, execution, or cleanup failed.
    #[error(transparent)]
    Bash(#[from] BashError),
    /// Cancellation stopped the waiter; persistence may still be settling.
    #[error("turn cancelled")]
    Cancelled,
    /// Provider request, response decoding, or terminal validation failed.
    #[error(transparent)]
    Model(#[from] ModelError),
    /// The model requested a tool unavailable under the configured policy.
    #[error("tool `{name}` is denied by policy")]
    ToolDenied {
        /// Denied native function name.
        name: String,
    },
    /// A repository read failed.
    #[error(transparent)]
    Read(#[from] ReadError),
    /// Repository-scoped tools were enabled without a repository root.
    #[error("repository root is required when a repository tool is allowed")]
    RepositoryRequired,
    /// The comparison base was validated for another repository scope.
    #[error("comparison base does not belong to the configured repository scope")]
    ComparisonRepositoryMismatch,
    /// A repository write failed.
    #[error(transparent)]
    Write(#[from] WriteError),
    /// The model exceeded the bounded number of calls in one turn.
    #[error("model exceeded the per-turn tool call limit of {limit}")]
    ToolCallLimit {
        /// Configured maximum calls.
        limit: usize,
    },
    /// Mandatory content exceeds the effective model's declared context
    /// budget, before any history is considered.
    #[error("mandatory request content weighs {required} but the model context budget is {budget}")]
    ContextBudgetExceeded {
        /// Total declared budget in approximate weight units.
        budget: u64,
        /// Approximate weight of instructions, current input, advertised tool
        /// definitions, and reserved output.
        required: u64,
    },
}

impl TurnError {
    /// Returns the stable lifecycle classification for this failure.
    pub fn error_type(&self) -> TurnErrorType {
        match self {
            Self::Cancelled => TurnErrorType::Cancelled,
            Self::Model(error) => TurnErrorType::Model(error.error_type()),
            Self::ToolDenied { .. } => TurnErrorType::ToolDenied,
            Self::Read(_)
            | Self::Write(_)
            | Self::Bash(_)
            | Self::CommandJournal { .. }
            | Self::CommandFailed { .. } => TurnErrorType::Tool,
            Self::RepositoryRequired => TurnErrorType::RepositoryRequired,
            Self::ComparisonRepositoryMismatch => TurnErrorType::ComparisonRepositoryMismatch,
            Self::ToolCallLimit { .. } => TurnErrorType::ToolCallLimit,
            Self::ContextBudgetExceeded { .. } => TurnErrorType::ContextBudget,
        }
    }
}

#[derive(Debug, Error)]
pub(crate) enum ResumeFailure {
    #[error("native provider continuation failed: {source}")]
    Native {
        #[source]
        source: ModelError,
    },
    #[error("native provider continuation was unavailable and history replay failed: {source}")]
    Replay {
        #[source]
        source: ModelError,
    },
}

impl ResumeFailure {
    pub(crate) fn into_model_error(self) -> ModelError {
        let source = match &self {
            Self::Native { source } | Self::Replay { source } => source,
        };
        if !matches!(source, ModelError::Request(_)) {
            return match self {
                Self::Native { source } | Self::Replay { source } => source,
            };
        }
        let error_type = source.error_type();
        let http_status = source.http_status();

        ModelError::classified_request(error_type, http_status, Box::new(self))
    }
}

pub(crate) fn sanitized_completion_metadata(metadata: &CompletionMetadata) -> CompletionMetadata {
    CompletionMetadata::new(
        sanitize_report_text(metadata.finish_reason()),
        metadata.response_id().map(sanitize_report_text),
        metadata.response_model().map(sanitize_report_text),
        metadata.system_fingerprint().map(sanitize_report_text),
        metadata.usage().copied(),
    )
}

pub(crate) fn sanitize_report_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn format_report_duration(duration: Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

#[cfg(test)]
#[path = "turn_test.rs"]
mod tests;
