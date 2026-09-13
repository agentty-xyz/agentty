//! Immutable turn options, outcomes, observable activity, and terminal errors.

use std::fmt;
use std::num::NonZeroUsize;
use std::time::Duration;

use serde_json::Value;
use thiserror::Error;

use crate::comparison::ComparisonBase;
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
    comparison_base: Option<ComparisonBase>,
    limits: TurnLimits,
    schema: OutputSchema,
    tool_policy: ToolPolicy,
}

impl TurnOptions {
    /// Resolves a required schema, explicit permissions, and execution limits.
    pub fn new(schema: OutputSchema, tool_policy: ToolPolicy, limits: TurnLimits) -> Self {
        Self {
            comparison_base: None,
            limits,
            schema,
            tool_policy,
        }
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
#[derive(Clone, Debug, Eq, PartialEq)]
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
}

/// Observable, content-free activity from one successful model turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnReport {
    duration: Duration,
    model_requests: Vec<ModelRequestActivity>,
    tool_calls: Vec<ToolActivity>,
}

impl TurnReport {
    /// Returns the complete elapsed turn time, including persistence for
    /// durable session turns.
    pub fn duration(&self) -> Duration {
        self.duration
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
            model_requests,
            tool_calls,
        }
    }
}

/// Observable facts about one provider request in a successful turn.
#[derive(Clone, Debug, Eq, PartialEq)]
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
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum ToolActivity {
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
            Self::Read { duration, .. }
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
        }
    }

    /// Returns the repository-relative target or bounded inspection summary.
    pub fn path(&self) -> &str {
        match self {
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
}

impl TurnError {
    /// Returns the stable lifecycle classification for this failure.
    pub fn error_type(&self) -> TurnErrorType {
        match self {
            Self::Model(error) => TurnErrorType::Model(error.error_type()),
            Self::ToolDenied { .. } => TurnErrorType::ToolDenied,
            Self::Read(_) | Self::Write(_) => TurnErrorType::Tool,
            Self::RepositoryRequired | Self::ComparisonRepositoryMismatch => {
                TurnErrorType::RepositoryRequired
            }
            Self::ToolCallLimit { .. } => TurnErrorType::ToolCallLimit,
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
