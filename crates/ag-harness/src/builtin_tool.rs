//! The built-in `read`, `write`, and `bash` tools as [`ToolHandler`]s, bound
//! to one turn's repository, journal, and options.

use std::time::Instant;

use async_trait::async_trait;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::bash::{BashArguments, BashError, CommandOutcome, CommandTermination};
use crate::execution::BashTool;
use crate::read::{self, ReadError, ReadTool, ResultFits};
use crate::tool::{ReadAction, ReadArguments, ToolDefinition, WriteArguments};
use crate::tool_handler::{ToolContext, ToolEffects, ToolError, ToolHandler, ToolOutput};
use crate::turn::{ToolActivity, TurnError, sanitize_report_text};
use crate::write::{WriteError, WriteTool};

/// The built-in `read` tool of one turn.
pub(crate) struct ReadHandler {
    pub(crate) definition: ToolDefinition,
    pub(crate) tool: ReadTool,
}

#[async_trait]
impl ToolHandler for ReadHandler {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::ReadOnly
    }

    /// Runs one read, bounding its result to the context's share of the
    /// budget, or to the result byte limit alone without one.
    async fn call(
        &self,
        arguments: Value,
        tool_context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let arguments: ReadArguments = decode(arguments)?;
        let fits: ResultFits<'_> = match tool_context.fits.as_deref() {
            Some(fits) => fits,
            None => &fits_any,
        };
        let (result, activity) = execute_read_tool(&self.tool, &arguments, fits, Instant::now())
            .await
            .map_err(ToolError::turn)?;

        Ok(ToolOutput::builtin(result, activity))
    }
}

/// The built-in `write` tool of one turn.
pub(crate) struct WriteHandler {
    pub(crate) tool: WriteTool,
}

#[async_trait]
impl ToolHandler for WriteHandler {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::write()
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::Writes
    }

    async fn call(
        &self,
        arguments: Value,
        tool_context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let arguments: WriteArguments = decode(arguments)?;
        let (result, activity) = execute_write_tool(
            &self.tool,
            &arguments,
            Instant::now(),
            tool_context.call_id(),
        )
        .await
        .map_err(ToolError::turn)?;

        Ok(ToolOutput::builtin(result, activity))
    }
}

/// The built-in `bash` tool of one turn.
pub(crate) struct BashHandler {
    pub(crate) definition: ToolDefinition,
    pub(crate) tool: BashTool,
}

#[async_trait]
impl ToolHandler for BashHandler {
    fn definition(&self) -> ToolDefinition {
        self.definition.clone()
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::Executes
    }

    async fn call(
        &self,
        arguments: Value,
        tool_context: ToolContext,
    ) -> Result<ToolOutput, ToolError> {
        let arguments: BashArguments = decode(arguments)?;
        let started_at = Instant::now();
        let outcome = match self.tool.execute(&arguments, tool_context.call_id()).await {
            Ok(outcome) => outcome,
            Err(TurnError::CommandFailed { outcome }) if refused(&outcome) => {
                return Err(ToolError::rejected(SANDBOX_REFUSED));
            }
            Err(error) => return Err(ToolError::turn(error)),
        };
        let mut result = serde_json::to_value(&outcome).map_err(unencodable_outcome)?;
        let duration = started_at.elapsed();
        let activity = if self.tool.enforces(&arguments) && blocked(&outcome) {
            result["sandbox"] = Value::from(SANDBOX_BLOCKED);

            ToolActivity::BashSandboxDenied { duration }
        } else {
            ToolActivity::Bash { duration }
        };

        Ok(ToolOutput::builtin(result.to_string(), activity))
    }
}

/// Note added as the `sandbox` field of a `bash` result whose command the
/// sandbox likely blocked.
const SANDBOX_BLOCKED: &str =
    "The command failed, and its output suggests the sandbox blocked one of its operations, such \
     as a write outside the writable directories, a network connection, or a socket. The \
     restriction comes from the host, not the command: do not try to work around it. If the \
     operation is necessary, tell the user what was blocked, so they can rerun the command \
     outside the sandbox or extend its grants.";

/// Lowercase output fragments that suggest the sandbox blocked an
/// operation, after the heuristic Codex uses to offer an unsandboxed retry.
const SANDBOX_DENIAL_MARKERS: [&str; 7] = [
    "operation not permitted",
    "permission denied",
    "read-only file system",
    "sandbox",
    "landlock",
    "seccomp",
    "failed to write file",
];

/// Returns whether `outcome`, of a command that ran inside the sandbox,
/// failed with output suggesting the sandbox blocked one of its
/// operations. The operating system reports a denial as an ordinary error,
/// so this is a heuristic: a failure the file permissions caused reads the
/// same.
fn blocked(outcome: &CommandOutcome) -> bool {
    outcome.exit_code.is_some_and(|code| code != 0)
        && [
            &outcome.stderr,
            &outcome.stderr_tail,
            &outcome.stdout,
            &outcome.stdout_tail,
        ]
        .into_iter()
        .any(|text| {
            let text = text.to_ascii_lowercase();

            SANDBOX_DENIAL_MARKERS
                .iter()
                .any(|marker| text.contains(marker))
        })
}

/// Feedback for a `bash` call whose command the sandbox refused to start.
const SANDBOX_REFUSED: &str = "The sandbox refused to start this command, so nothing ran. The \
                               refusal comes from the sandbox configuration, not the command, for \
                               example a granted directory that is missing, so every `bash` \
                               command fails the same way until the host fixes it; continue \
                               without `bash` and report the problem.";

/// Returns whether `outcome` records a command the sandbox refused before
/// starting it: the policy could not be enforced, nothing ran, and nothing
/// remains to clean up. The model can continue without it, so the turn
/// does.
fn refused(outcome: &CommandOutcome) -> bool {
    outcome.execution_failure == Some(BashError::Unavailable)
        && outcome.termination == CommandTermination::Failed
        && !outcome.cleanup_failed
}

/// Decodes a built-in tool's arguments; the engine sends only arguments it
/// already decoded, so a failure rejects a call made outside a turn.
fn decode<T: DeserializeOwned>(arguments: Value) -> Result<T, ToolError> {
    serde_json::from_value(arguments).map_err(|error| {
        ToolError::rejected(format!("failed to parse function arguments: {error}"))
    })
}

/// Fails a `bash` call whose command outcome cannot be encoded as its
/// result.
fn unencodable_outcome(_: serde_json::Error) -> ToolError {
    ToolError::turn(BashError::Execution)
}

/// Accepts every result; bounds a `read` called outside a turn to the
/// result byte limit alone.
fn fits_any(_: &str) -> bool {
    true
}

/// Returns the bounded report summary of a `read`: its path, else its
/// query, else the tool name.
pub(crate) fn read_summary(arguments: &ReadArguments) -> &str {
    arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read")
}

async fn execute_read_tool(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    fits: ResultFits<'_>,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    if let Some(error) = arguments.validation_error() {
        return reject_invalid_read_arguments(arguments, error, started_at);
    }
    if arguments.action() == ReadAction::File {
        return execute_file_read(read_tool, arguments, fits, started_at).await;
    }

    execute_repository_inspection(read_tool, arguments, fits, started_at).await
}

fn reject_invalid_read_arguments(
    arguments: &ReadArguments,
    error: &str,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let summary = read_summary(arguments);
    let result = read::invalid_arguments_tool_result(error, summary).map_err(ReadError::from)?;
    let activity = if arguments.action() == ReadAction::File {
        ToolActivity::ReadRejected {
            duration: started_at.elapsed(),
            path: sanitize_report_text(summary),
        }
    } else {
        ToolActivity::ReadInspectionRejected {
            action: arguments.action(),
            duration: started_at.elapsed(),
            summary: sanitize_report_text(summary),
        }
    };

    Ok((result, activity))
}

async fn execute_file_read(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    fits: ResultFits<'_>,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    match read_tool.execute(arguments).await {
        Ok(output) => match output.to_tool_result_within(fits) {
            Ok((result, shown)) => Ok((
                result,
                ToolActivity::Read {
                    duration: started_at.elapsed(),
                    end_line: shown.end_line(),
                    path: sanitize_report_text(shown.path()),
                    start_line: shown.start_line(),
                    truncated: shown.truncated(),
                },
            )),
            Err(error) => error
                .to_tool_result(output.path())
                .map_err(ReadError::from)
                .map_err(TurnError::from)
                .map(|result| {
                    (
                        result,
                        ToolActivity::ReadRejected {
                            duration: started_at.elapsed(),
                            path: sanitize_report_text(output.path()),
                        },
                    )
                }),
        },
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(arguments.path())
            .map_err(ReadError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::ReadRejected {
                        duration: started_at.elapsed(),
                        path: sanitize_report_text(arguments.path()),
                    },
                )
            }),
        Err(error) => Err(error.into()),
    }
}

async fn execute_repository_inspection(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    fits: ResultFits<'_>,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let fallback_summary = read_summary(arguments);
    match read_tool.execute_inspection(arguments, fits).await {
        Ok((result, summary)) => Ok((
            result,
            ToolActivity::ReadInspection {
                action: arguments.action(),
                arguments: serde_json::to_string(arguments).map_err(ReadError::from)?,
                duration: started_at.elapsed(),
                summary: sanitize_report_text(&summary),
            },
        )),
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(fallback_summary)
            .map_err(ReadError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::ReadInspectionRejected {
                        action: arguments.action(),
                        duration: started_at.elapsed(),
                        summary: sanitize_report_text(fallback_summary),
                    },
                )
            }),
        Err(error) => Err(error.into_read_error(fallback_summary.to_string()).into()),
    }
}

async fn execute_write_tool(
    write_tool: &WriteTool,
    arguments: &WriteArguments,
    started_at: Instant,
    call_id: &str,
) -> Result<(String, ToolActivity), TurnError> {
    match write_tool.execute(arguments, call_id).await {
        Ok(output) => {
            let activity = ToolActivity::Write {
                bytes_written: output.bytes_written(),
                duration: started_at.elapsed(),
                path: sanitize_report_text(output.path()),
            };
            let result = output
                .to_tool_result()
                .map_err(WriteError::from)
                .map_err(TurnError::from)?;

            Ok((result, activity))
        }
        Err(error) if error.is_model_correctable() => error
            .to_tool_result(arguments.path())
            .map_err(WriteError::from)
            .map_err(TurnError::from)
            .map(|result| {
                (
                    result,
                    ToolActivity::WriteRejected {
                        duration: started_at.elapsed(),
                        path: sanitize_report_text(arguments.path()),
                    },
                )
            }),
        Err(error) => Err(error.into()),
    }
}

#[cfg(test)]
#[path = "builtin_tool_test.rs"]
mod tests;
