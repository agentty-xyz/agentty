use std::sync::Arc;
use std::time::Instant;

use crate::bash::{BashArguments, BashError};
use crate::context;
use crate::effect::Effects;
use crate::execution::BashTool;
use crate::file_system::FileSystem;
use crate::lifecycle::{LifecycleEmitter, LifecycleId, ToolErrorType, ToolLifecycle};
use crate::model::{
    Model, ModelError, ModelMessage, ModelRequest, ModelResponse, ReasoningEffort,
    ensure_unique_tool_call_ids,
};
use crate::read::{self, ReadError, ReadTool};
use crate::repository::Repository;
use crate::reservation::WriteJournal;
use crate::tool::{ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments, WriteArguments};
use crate::turn::{
    ModelRequestActivity, ToolActivity, TurnError, TurnOptions, TurnOutcome, TurnReport,
    sanitize_report_text, sanitized_completion_metadata,
};
use crate::write::{WriteError, WriteTool};

/// Shared execution dependencies and the immutable configuration for one turn.
pub(crate) struct Engine<'a> {
    pub(crate) context_budget: context::ContextBudget,
    pub(crate) context_estimator: &'a dyn context::ContextEstimator,
    pub(crate) effects: Effects,
    pub(crate) file_system: &'a Arc<dyn FileSystem>,
    pub(crate) lifecycle: &'a LifecycleEmitter,
    pub(crate) model: &'a Arc<dyn Model>,
    pub(crate) model_reasoning_effort: Option<ReasoningEffort>,
    pub(crate) options: &'a TurnOptions,
    pub(crate) repository: Option<&'a Repository>,
}

impl Engine<'_> {
    pub(crate) async fn run(
        &self,
        request: ModelRequest,
        turn_id: Option<LifecycleId>,
        journal: Option<WriteJournal>,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>), TurnError> {
        let started_at = Instant::now();
        let (mut request, tools) = self.prepare_request(request, journal)?;
        let mut model_request_index = 0_u64;
        let mut model_requests = Vec::new();
        let mut tool_calls = Vec::new();

        loop {
            let (response, activity, reasoning_content) = self
                .complete_model_request(&request, model_request_index, turn_id)
                .await?;
            model_request_index = model_request_index.saturating_add(1);
            model_requests.push(activity);

            match response {
                ModelResponse::Output(output) => {
                    request.record_output_with_reasoning(&output, reasoning_content);
                    let report = TurnReport::new(started_at.elapsed(), model_requests, tool_calls);

                    return Ok((TurnOutcome::new(output, report), request.into_messages()));
                }
                ModelResponse::ToolCall(call) => {
                    let (result, activity) = self.execute_tool_call(&call, &tools, turn_id).await?;
                    request.record_tool_result(call, result);
                    tool_calls.push(activity);
                }
                ModelResponse::ToolCalls(calls) => {
                    if calls.is_empty() {
                        return Err(ModelError::MissingToolCall.into());
                    }
                    ensure_unique_tool_call_ids(&calls)?;
                    let mut results = Vec::with_capacity(calls.len());
                    for call in &calls {
                        let (result, activity) =
                            self.execute_tool_call(call, &tools, turn_id).await?;
                        results.push(result);
                        tool_calls.push(activity);
                    }
                    request.record_tool_results(calls, results);
                }
            }
            // Tool traffic grows the request between model calls; a grown
            // request that no longer fits fails typed here instead of
            // overflowing the provider context.
            context::admit_grown_request(self.context_estimator, self.context_budget, &request)?;
        }
    }

    fn prepare_request(
        &self,
        mut request: ModelRequest,
        journal: Option<WriteJournal>,
    ) -> Result<(ModelRequest, Tools), TurnError> {
        if self.lifecycle.is_enabled() {
            request.mark_lifecycle_observed();
        }
        if request.model_reasoning_effort().is_none()
            && let Some(reasoning_effort) = self.model_reasoning_effort
        {
            request = request.with_model_reasoning_effort(reasoning_effort);
        }
        if let Some(base) = self.options.comparison_base()
            && !self
                .repository
                .is_some_and(|repository| base.matches_repository(repository))
        {
            return Err(TurnError::ComparisonRepositoryMismatch);
        }
        let read_allowed = self.options.tool_policy().allows(Tool::Read);
        let write_allowed = self.options.tool_policy().allows(Tool::Write);
        let bash_allowed = self.options.tool_policy().allows(Tool::Bash);
        if !read_allowed && !write_allowed && !bash_allowed {
            return Ok((request, Tools::default()));
        }
        let repository = self
            .repository
            .as_ref()
            .ok_or(TurnError::RepositoryRequired)?;
        for tool in context::advertised_tools(self.options) {
            request = request.with_tool(tool);
        }
        let read_tool = read_allowed.then(|| {
            ReadTool::with_git(
                Arc::clone(self.file_system),
                repository.root().to_path_buf(),
                repository.git_executable().to_path_buf(),
                self.options.comparison_base().cloned(),
            )
        });
        let write_tool = write_allowed.then(|| {
            let mut tool = WriteTool::new(
                Arc::clone(self.file_system),
                repository.root().to_path_buf(),
            );
            tool.journal.clone_from(&journal);
            tool.effects = self.effects.clone();

            tool
        });

        let bash_tool = if bash_allowed {
            Some(BashTool::new(
                self.options
                    .bash()
                    .cloned()
                    .ok_or(BashError::InvalidPolicy)?,
                repository.root().to_path_buf(),
                self.effects.commands().clone(),
                journal,
            )?)
        } else {
            None
        };

        Ok((
            request,
            Tools {
                read: read_tool,
                write: write_tool,
                bash: bash_tool,
            },
        ))
    }

    async fn complete_model_request(
        &self,
        request: &ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<(ModelResponse, ModelRequestActivity, Option<String>), ModelError> {
        let started_at = Instant::now();
        let model_lifecycle =
            self.lifecycle
                .start_model_request(self.model.metadata(), model_request_index, turn_id);
        let operation = self.model.complete(request.clone());
        let completion = match model_lifecycle.as_ref() {
            Some(model_lifecycle) => model_lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (response, completion, reasoning_content) = match completion {
            Ok(completion) => completion.into_parts(),
            Err(error) => {
                if let Some(model_lifecycle) = model_lifecycle {
                    model_lifecycle.failed(error.error_type(), error.http_status());
                }

                return Err(error);
            }
        };
        if let Some(output) = response.output()
            && let Err(error) = request.schema().validate_value(output)
        {
            let error = ModelError::from(error);
            if let Some(model_lifecycle) = model_lifecycle {
                model_lifecycle.failed(error.error_type(), error.http_status());
            }

            return Err(error);
        }
        let response_type = response.response_type();
        let activity = ModelRequestActivity::new(
            completion.as_ref().map(sanitized_completion_metadata),
            started_at.elapsed(),
            response_type,
        );
        if let Some(model_lifecycle) = model_lifecycle {
            model_lifecycle.completed(completion, response_type);
        }

        Ok((response, activity, reasoning_content))
    }

    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        tools: &Tools,
        turn_id: Option<LifecycleId>,
    ) -> Result<(String, ToolActivity), TurnError> {
        let mut tool_lifecycle =
            self.lifecycle
                .request_tool(call.id().to_string(), call.name().to_string(), turn_id);
        let execution = match call.arguments() {
            ToolCallArguments::Bash(arguments) => tools
                .bash
                .as_ref()
                .map(|tool| ToolExecution::Bash(tool, arguments)),
            ToolCallArguments::Read(arguments) => tools
                .read
                .as_ref()
                .map(|tool| ToolExecution::Read(tool, arguments)),
            ToolCallArguments::Write(arguments) => tools
                .write
                .as_ref()
                .map(|tool| ToolExecution::Write(tool, arguments)),
        };
        let Some(execution) = execution else {
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.denied();
            }

            return Err(TurnError::ToolDenied {
                name: call.name().to_string(),
            });
        };
        if let Some(tool_lifecycle) = tool_lifecycle.as_mut() {
            tool_lifecycle.started();
        }
        let operation = execute_tool(execution, call.id());
        let result = match tool_lifecycle.as_ref() {
            Some(tool_lifecycle) => tool_lifecycle.scope(operation).await,
            None => operation.await,
        };

        Self::finish_tool_call(result, tool_lifecycle)
    }

    fn finish_tool_call(
        result: Result<(String, ToolActivity), TurnError>,
        tool_lifecycle: Option<ToolLifecycle>,
    ) -> Result<(String, ToolActivity), TurnError> {
        match result {
            Ok(result) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    if matches!(
                        &result.1,
                        ToolActivity::ReadInspectionRejected { .. }
                            | ToolActivity::ReadRejected { .. }
                            | ToolActivity::WriteRejected { .. }
                    ) {
                        tool_lifecycle.failed(ToolErrorType::Execution);
                    } else {
                        tool_lifecycle.completed();
                    }
                }

                Ok(result)
            }
            Err(error) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    tool_lifecycle.failed(ToolErrorType::Execution);
                }

                Err(error)
            }
        }
    }
}

#[derive(Default)]
struct Tools {
    bash: Option<BashTool>,
    read: Option<ReadTool>,
    write: Option<WriteTool>,
}

enum ToolExecution<'a> {
    Bash(&'a BashTool, &'a BashArguments),
    Read(&'a ReadTool, &'a ReadArguments),
    Write(&'a WriteTool, &'a WriteArguments),
}

async fn execute_tool(
    execution: ToolExecution<'_>,
    call_id: &str,
) -> Result<(String, ToolActivity), TurnError> {
    let started_at = Instant::now();

    match execution {
        ToolExecution::Bash(tool, arguments) => {
            let outcome = tool.execute(arguments, call_id).await?;
            let result = serde_json::to_string(&outcome).map_err(|_| BashError::Execution)?;

            Ok((
                result,
                ToolActivity::Bash {
                    duration: started_at.elapsed(),
                },
            ))
        }
        ToolExecution::Read(read_tool, arguments) => {
            execute_read_tool(read_tool, arguments, started_at).await
        }
        ToolExecution::Write(write_tool, arguments) => {
            execute_write_tool(write_tool, arguments, started_at, call_id).await
        }
    }
}

async fn execute_read_tool(
    read_tool: &ReadTool,
    arguments: &ReadArguments,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    if let Some(error) = arguments.validation_error() {
        return reject_invalid_read_arguments(arguments, error, started_at);
    }
    if arguments.action() == ReadAction::File {
        return execute_file_read(read_tool, arguments, started_at).await;
    }

    execute_repository_inspection(read_tool, arguments, started_at).await
}

fn reject_invalid_read_arguments(
    arguments: &ReadArguments,
    error: &str,
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let summary = arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read");
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
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    match read_tool.execute(arguments).await {
        Ok(output) => match output.to_tool_result() {
            Ok(result) => Ok((
                result,
                ToolActivity::Read {
                    duration: started_at.elapsed(),
                    end_line: output.end_line(),
                    path: sanitize_report_text(output.path()),
                    start_line: output.start_line(),
                    truncated: output.truncated(),
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
    started_at: Instant,
) -> Result<(String, ToolActivity), TurnError> {
    let fallback_summary = arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read");
    match read_tool.execute_inspection(arguments).await {
        Ok((result, summary)) => Ok((
            result,
            ToolActivity::ReadInspection {
                action: arguments.action(),
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
#[path = "engine_test.rs"]
mod tests;
