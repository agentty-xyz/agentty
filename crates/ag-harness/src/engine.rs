use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;

use crate::bash::{BashArguments, BashError};
use crate::calibration::ContextCalibration;
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
use crate::tool::{
    MalformedArguments, ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments,
    ToolDefinition, WriteArguments,
};
use crate::turn::{
    ModelRequestActivity, ToolActivity, TurnError, TurnOptions, TurnOutcome, TurnReport,
    sanitize_report_text, sanitized_completion_metadata,
};
use crate::write::{WriteError, WriteTool};
use crate::{context, schema_contract};

/// Final outputs a turn may have rejected for invalid JSON or a schema
/// violation and re-requested with corrective feedback before failing.
pub(crate) const MAX_OUTPUT_CORRECTIONS: usize = 2;

/// Consecutive tool-call responses in which every call has malformed
/// arguments that fail a turn; each earlier one is journaled and its calls
/// are answered with error results. A response with any decodable call
/// resets the count, so a model that keeps correcting itself is never cut
/// off, while one that cannot form a valid call stops after a bounded number
/// of wasted requests.
pub(crate) const MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES: usize = 3;

/// Shared execution dependencies and the immutable configuration for one turn.
pub(crate) struct Engine<'a> {
    pub(crate) context_budget: context::ContextBudget,
    pub(crate) context_calibration: &'a ContextCalibration,
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
        let recorder = journal.clone();
        let (mut request, tools) = self.prepare_request(request, journal)?;
        let mut context_fit = context::ContextFit::default();
        let mut model_request_index = 0_u64;
        let mut model_requests = Vec::new();
        let mut output_corrections = 0;
        let mut consecutive_malformed_responses = 0;
        let mut malformed_tool_calls = 0;
        let mut reads = RepeatedReads::default();
        let mut tool_calls = Vec::new();

        loop {
            // Tool traffic grows the request between model calls; a grown
            // request that no longer fits the calibrated budget is sent as a
            // reduced copy, and one that cannot be reduced enough fails
            // typed here instead of overflowing the provider context.
            let (budget, envelope) = self
                .context_calibration
                .effective_budget(self.context_budget);
            let (fitted, context_activity) = context_fit
                .fit(self.context_estimator, budget, &request)
                .map_err(|error| {
                    ContextCalibration::declared_failure(self.context_budget, envelope, error)
                })?;
            let completed = self
                .complete_model_request(fitted.unwrap_or(&request), model_request_index, turn_id)
                .await;
            let estimated_weight = context_fit.sent_weight();
            model_request_index = model_request_index.saturating_add(1);
            let (response, activity, reasoning_content) = match completed {
                // An invalid final output is re-requested with request-only
                // feedback; neither it nor the feedback is journaled, so
                // replayed history keeps only completed exchanges.
                Err(
                    error @ (ModelError::InvalidJson { .. } | ModelError::SchemaViolation { .. }),
                ) if output_corrections < MAX_OUTPUT_CORRECTIONS => {
                    output_corrections += 1;
                    request.record_output_feedback(output_feedback(&error));

                    continue;
                }
                completed => completed?,
            };
            self.context_calibration.observe(
                self.context_budget,
                estimated_weight,
                activity.completion(),
            );
            model_requests.push(activity.with_context(context_activity).with_estimate(
                estimated_weight,
                envelope.map(|envelope| envelope.tokens(estimated_weight)),
            ));

            let (calls, batched) = match response {
                ModelResponse::Output(output) => {
                    request.record_output_with_reasoning(&output, reasoning_content);
                    let report = TurnReport::new(started_at.elapsed(), model_requests, tool_calls)
                        .with_corrections(output_corrections, malformed_tool_calls);

                    return Ok((TurnOutcome::new(output, report), request.into_messages()));
                }
                ModelResponse::ToolCall(call) => (vec![call], false),
                ModelResponse::ToolCalls(calls) => {
                    if calls.is_empty() {
                        return Err(ModelError::MissingToolCall.into());
                    }
                    ensure_unique_tool_call_ids(&calls)?;

                    (calls, true)
                }
            };
            // A malformed call is journaled and answered with an error result
            // like any other call; only a model that sends nothing else for
            // several responses in a row fails the turn, unjournaled.
            let malformed = calls
                .iter()
                .filter_map(ToolCall::malformed_arguments)
                .collect::<Vec<_>>();
            if malformed.len() < calls.len() {
                consecutive_malformed_responses = 0;
            } else {
                consecutive_malformed_responses += 1;
                if consecutive_malformed_responses == MAX_CONSECUTIVE_MALFORMED_TOOL_RESPONSES {
                    return Err(ModelError::InvalidToolArguments {
                        reason: schema_contract::bounded_diagnostic(format!(
                            "`{}`: {}",
                            malformed[0].tool().name(),
                            malformed[0].reason()
                        )),
                    }
                    .into());
                }
            }
            malformed_tool_calls += malformed.len();
            let call_message = match (batched, calls.as_slice()) {
                (false, [call]) => request.record_tool_call(call.clone()),
                _ => request.record_tool_calls(calls.clone()),
            };
            Self::persist(recorder.as_ref(), call_message).await?;
            for call in &calls {
                let repeated = reads.repeat_of(call, &context_fit);
                let (result, activity) = self
                    .execute_tool_call(call, &tools, turn_id, repeated.as_deref())
                    .await?;
                let result_bytes = result.len();
                Self::persist(recorder.as_ref(), request.record_tool_result(call, result)).await?;
                let result_index = request.messages().len() - 1;
                reads.record(call, result_index, result_bytes, repeated.is_some());
                tool_calls.push(activity);
            }
        }
    }

    /// Appends one finished model response or tool result to a durable
    /// turn before the loop continues, so a stopped turn keeps it.
    async fn persist(
        recorder: Option<&WriteJournal>,
        message: &ModelMessage,
    ) -> Result<(), TurnError> {
        let Some(recorder) = recorder else {
            return Ok(());
        };

        recorder
            .append_messages(std::slice::from_ref(message))
            .await
            .map_err(|source| TurnError::MessageJournal {
                source: Box::new(source),
            })
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

    /// Runs `call` under the turn's tool policy, or, when `repeats` names
    /// the earlier call an allowed `read` repeats, answers it without running.
    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        tools: &Tools,
        turn_id: Option<LifecycleId>,
        repeats: Option<&str>,
    ) -> Result<(String, ToolActivity), TurnError> {
        let mut tool_lifecycle =
            self.lifecycle
                .request_tool(call.id().to_string(), call.name().to_string(), turn_id);
        let execution = match call.arguments() {
            ToolCallArguments::Bash(arguments) => tools
                .bash
                .as_ref()
                .map(|tool| ToolExecution::Bash(tool, arguments)),
            ToolCallArguments::Read(arguments) => tools.read.as_ref().map(|tool| match repeats {
                Some(earlier) => ToolExecution::RepeatedRead(arguments, earlier),
                None => ToolExecution::Read(tool, arguments),
            }),
            ToolCallArguments::Write(arguments) => tools
                .write
                .as_ref()
                .map(|tool| ToolExecution::Write(tool, arguments)),
            ToolCallArguments::Malformed(arguments) => tools
                .allows(arguments.tool())
                .then_some(ToolExecution::Malformed(arguments, self.options)),
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
                        ToolActivity::MalformedArguments { .. }
                            | ToolActivity::ReadInspectionRejected { .. }
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

/// Executed `read` calls of the current turn, keyed by their canonical
/// arguments, so a repeat may be answered without running it while the
/// model still sees the earlier result whole and nothing changed since.
#[derive(Default)]
struct RepeatedReads {
    /// `write` and `bash` calls handled so far in the turn.
    effects: usize,
    executed: HashMap<String, ExecutedRead>,
}

impl RepeatedReads {
    /// Returns the identifier of the earlier call of this turn that the
    /// `read` `call` repeats when no `write` or `bash` call was handled since,
    /// the last request sent that call's result whole, and the result is
    /// larger than the note that would point to it; `None` otherwise,
    /// including for any other tool.
    fn repeat_of(&self, call: &ToolCall, context_fit: &context::ContextFit) -> Option<String> {
        let executed = self.executed.get(&Self::key(call)?)?;

        (executed.effects == self.effects
            && context_fit.sends_whole(executed.result_index)
            && context::repeated_read_result(&executed.call_id).len() < executed.result_bytes)
            .then(|| executed.call_id.clone())
    }

    /// Remembers a handled call whose result of `result_bytes` is the
    /// request message at `result_index`: an executed `read` as the latest
    /// for its arguments, and any `write` or `bash` call as a possible
    /// change. A `read` answered as a repeat and a malformed call change
    /// nothing.
    fn record(
        &mut self,
        call: &ToolCall,
        result_index: usize,
        result_bytes: usize,
        repeated: bool,
    ) {
        match Self::key(call) {
            Some(_) if repeated => {}
            Some(key) => {
                self.executed.insert(
                    key,
                    ExecutedRead {
                        call_id: call.id().to_string(),
                        effects: self.effects,
                        result_bytes,
                        result_index,
                    },
                );
            }
            None if call.malformed_arguments().is_none() => self.effects += 1,
            None => {}
        }
    }

    /// Returns a `read` call's canonical arguments; `None` for other tools.
    fn key(call: &ToolCall) -> Option<String> {
        call.read_arguments()?;

        call.arguments_json().ok()
    }
}

/// One executed `read` call of the current turn.
struct ExecutedRead {
    call_id: String,
    /// `write` and `bash` calls handled before it.
    effects: usize,
    result_bytes: usize,
    result_index: usize,
}

#[derive(Default)]
struct Tools {
    bash: Option<BashTool>,
    read: Option<ReadTool>,
    write: Option<WriteTool>,
}

impl Tools {
    /// Whether the turn's tool policy enabled `tool`.
    fn allows(&self, tool: Tool) -> bool {
        match tool {
            Tool::Bash => self.bash.is_some(),
            Tool::Read => self.read.is_some(),
            Tool::Write => self.write.is_some(),
        }
    }
}

enum ToolExecution<'a> {
    Bash(&'a BashTool, &'a BashArguments),
    Read(&'a ReadTool, &'a ReadArguments),
    /// A call whose arguments its tool could not decode, answered with an
    /// error result that lists the tool's argument fields.
    Malformed(&'a MalformedArguments, &'a TurnOptions),
    /// A `read` repeating the named earlier call, answered without running.
    RepeatedRead(&'a ReadArguments, &'a str),
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
        ToolExecution::Malformed(arguments, options) => Ok((
            malformed_arguments_result(arguments, options),
            ToolActivity::MalformedArguments {
                duration: started_at.elapsed(),
                reason: sanitize_report_text(arguments.reason()),
                tool: arguments.tool(),
            },
        )),
        ToolExecution::RepeatedRead(arguments, earlier) => Ok((
            context::repeated_read_result(earlier),
            ToolActivity::ReadRepeated {
                action: arguments.action(),
                duration: started_at.elapsed(),
                summary: sanitize_report_text(read_summary(arguments)),
            },
        )),
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
    let fallback_summary = read_summary(arguments);
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

/// Returns the bounded report summary of a `read`: its path, else its
/// query, else the tool name.
fn read_summary(arguments: &ReadArguments) -> &str {
    arguments
        .path_filter()
        .or_else(|| arguments.query())
        .unwrap_or("read")
}

/// Renders the bounded feedback that re-requests a rejected final output.
fn output_feedback(error: &ModelError) -> String {
    format!(
        "[harness] Your previous final response was rejected ({}). Respond with only a single \
         JSON value that matches the output schema, with no prose or Markdown around it.",
        schema_contract::bounded_diagnostic(error)
    )
}

/// Renders the error result that answers a call with malformed arguments,
/// naming the decoding failure and every argument field of the called tool.
fn malformed_arguments_result(arguments: &MalformedArguments, options: &TurnOptions) -> String {
    let name = arguments.tool().name();
    let fields = context::advertised_tools(options)
        .into_iter()
        .find(|tool| tool.name() == name)
        .map_or_else(String::new, |tool| argument_fields(&tool));

    json!({
        "error": schema_contract::bounded_diagnostic(format!(
            "failed to parse function arguments: {}",
            arguments.reason()
        )),
        "hint": format!(
            "This call did not run. Call `{name}` again with valid JSON arguments using only its \
             documented fields: {fields}."
        ),
        "status": "rejected",
    })
    .to_string()
}

/// Lists a tool's argument fields, marking the required ones.
fn argument_fields(tool: &ToolDefinition) -> String {
    let parameters = tool.parameters();
    let required = parameters["required"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    let mut names: Vec<&String> = parameters["properties"]
        .as_object()
        .into_iter()
        .flat_map(|properties| properties.keys())
        .collect();
    // Sorted so the result does not depend on map ordering.
    names.sort();

    names
        .into_iter()
        .map(|field| {
            if required.iter().any(|name| name == field.as_str()) {
                format!("`{field}` (required)")
            } else {
                format!("`{field}`")
            }
        })
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
