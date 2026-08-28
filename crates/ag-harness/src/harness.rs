use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use thiserror::Error;

use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::{
    LifecycleEmitter, LifecycleId, LifecycleObserver, ToolErrorType, ToolLifecycle, TurnErrorType,
    TurnLifecycle,
};
use crate::model::{
    CompletionMetadata, Model, ModelError, ModelMessage, ModelRequest, ModelResponse,
    ensure_unique_tool_call_ids,
};
use crate::policy::Policy;
use crate::read::{ReadError, ReadTool};
use crate::schema_contract::OutputSchema;
use crate::tool::{
    ReadArguments, Tool, ToolCall, ToolCallArguments, ToolDefinition, WriteArguments,
};
use crate::write::{WriteError, WriteTool};

const DEFAULT_MAX_HISTORY_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_TOOL_CALLS: usize = 8;

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
}

/// Observable, content-free activity from one successful model turn.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TurnReport {
    duration: Duration,
    model_requests: Vec<ModelRequestActivity>,
    tool_calls: Vec<ToolActivity>,
}

impl TurnReport {
    /// Returns the complete elapsed turn time.
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
}

/// Observable facts about one successful provider request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ModelRequestActivity {
    completion: Option<CompletionMetadata>,
    duration: Duration,
    response_type: crate::lifecycle::ModelResponseType,
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

    /// Returns whether the request produced output or a tool call.
    pub fn response_type(&self) -> crate::lifecycle::ModelResponseType {
        self.response_type
    }
}

/// Sanitized details about one successful built-in tool operation.
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
            | Self::ReadRejected { duration, .. }
            | Self::Write { duration, .. }
            | Self::WriteRejected { duration, .. } => *duration,
        }
    }

    /// Returns the bounded built-in tool name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Read { .. } | Self::ReadRejected { .. } => "read",
            Self::Write { .. } | Self::WriteRejected { .. } => "write",
        }
    }

    /// Returns the repository-relative target path.
    pub fn path(&self) -> &str {
        match self {
            Self::Read { path, .. }
            | Self::ReadRejected { path, .. }
            | Self::Write { path, .. }
            | Self::WriteRejected { path, .. } => path,
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

/// In-memory sequence of model turns sharing conversation and tool history.
pub struct ChatSession<'a> {
    harness: &'a Harness,
    history: ChatHistory,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl ChatSession<'_> {
    /// Adds a system prompt to every model request in this chat.
    #[must_use]
    pub fn with_system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }

    /// Sends one prompt and retains the successful turn in session history.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] under the same conditions as [`Harness::run`]. A
    /// failed turn is not added to the conversation history.
    pub async fn send(&mut self, prompt: impl Into<String>) -> Result<TurnOutcome, TurnError> {
        let mut messages = self.history.messages();
        if let Some(system_prompt) = &self.system_prompt {
            messages.insert(0, ModelMessage::System(system_prompt.clone()));
        }
        let retained_messages = messages.len();
        let request = ModelRequest::with_history(messages, prompt.into(), self.schema.clone());
        let (outcome, mut messages) = self.harness.run_request(request).await?;
        let turn = messages.split_off(retained_messages);
        self.history.push(turn);

        Ok(outcome)
    }
}

struct ChatHistory {
    bytes: usize,
    max_bytes: usize,
    turns: VecDeque<Vec<ModelMessage>>,
}

impl ChatHistory {
    fn new(max_bytes: usize) -> Self {
        Self {
            bytes: 0,
            max_bytes,
            turns: VecDeque::new(),
        }
    }

    fn messages(&self) -> Vec<ModelMessage> {
        self.turns
            .iter()
            .flat_map(|turn| turn.iter().cloned())
            .collect()
    }

    fn push(&mut self, turn: Vec<ModelMessage>) {
        self.bytes = self.bytes.saturating_add(retained_bytes(&turn));
        self.turns.push_back(turn);

        while self.bytes > self.max_bytes && !self.turns.is_empty() {
            let evicted_bytes = self
                .turns
                .pop_front()
                .map_or(self.bytes, |evicted| retained_bytes(&evicted));
            self.bytes = self.bytes.saturating_sub(evicted_bytes);
        }
    }
}

fn retained_bytes(messages: &[ModelMessage]) -> usize {
    messages.iter().fold(0, |bytes, message| {
        bytes.saturating_add(message.retained_bytes())
    })
}

/// Application-facing harness for one complete model turn.
///
/// A turn advertises policy-approved tools, executes validated native calls,
/// returns tool results to the model, and finishes with locally validated
/// structured output.
pub struct Harness {
    file_system: Arc<dyn FileSystem>,
    lifecycle: LifecycleEmitter,
    max_history_bytes: usize,
    max_tool_calls: usize,
    model: Arc<dyn Model>,
    policy: Policy,
    repository_root: Option<PathBuf>,
}

impl Harness {
    /// Creates a deny-by-default harness backed by the local filesystem.
    pub fn new(model: impl Model + 'static) -> Self {
        Self {
            file_system: Arc::new(LocalFileSystem),
            lifecycle: LifecycleEmitter::default(),
            max_history_bytes: DEFAULT_MAX_HISTORY_BYTES,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            model: Arc::new(model),
            policy: Policy::default(),
            repository_root: None,
        }
    }

    /// Roots repository-scoped tools at `repository_root`.
    #[must_use]
    pub fn repository(mut self, repository_root: impl Into<PathBuf>) -> Self {
        self.repository_root = Some(repository_root.into());

        self
    }

    /// Enables one built-in tool for model requests.
    #[must_use]
    pub fn allow(mut self, tool: Tool) -> Self {
        self.policy.allow(tool);

        self
    }

    /// Replaces the local filesystem implementation.
    #[must_use]
    pub fn file_system(mut self, file_system: impl FileSystem + 'static) -> Self {
        self.file_system = Arc::new(file_system);

        self
    }

    /// Sends metadata-only turn, model, and tool events to `observer`.
    ///
    /// This observer owns model events for requests made through the harness.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.lifecycle = LifecycleEmitter::new(observer);

        self
    }

    /// Overrides the maximum number of native calls allowed in one turn.
    #[must_use]
    pub fn max_tool_calls(mut self, max_tool_calls: NonZeroUsize) -> Self {
        self.max_tool_calls = max_tool_calls.get();

        self
    }

    /// Overrides the retained chat-history payload budget.
    ///
    /// Complete oldest turns are evicted when the budget is exceeded, so
    /// native tool-call and tool-result messages are never split.
    #[must_use]
    pub fn max_history_bytes(mut self, max_history_bytes: NonZeroUsize) -> Self {
        self.max_history_bytes = max_history_bytes.get();

        self
    }

    /// Runs one prompt through tool execution to terminal structured output.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when the model fails, requests a denied tool,
    /// exceeds the call limit, or a requested repository operation fails.
    pub async fn run(
        &self,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Result<Value, TurnError> {
        self.run_report(prompt, schema)
            .await
            .map(TurnOutcome::into_output)
    }

    /// Runs one prompt and returns validated output with sanitized activity.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] under the same conditions as [`Harness::run`].
    pub async fn run_report(
        &self,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Result<TurnOutcome, TurnError> {
        let request = ModelRequest::new(prompt, schema);
        self.run_request(request).await.map(|(outcome, _)| outcome)
    }

    /// Starts an in-memory chat whose responses must match `schema`.
    pub fn chat(&self, schema: OutputSchema) -> ChatSession<'_> {
        ChatSession {
            harness: self,
            history: ChatHistory::new(self.max_history_bytes),
            schema,
            system_prompt: None,
        }
    }

    async fn run_request(
        &self,
        request: ModelRequest,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>), TurnError> {
        let turn = self.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let result = self.run_turn(request, turn_id).await;

        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(error) => turn.failed(error.error_type()),
            }
        }

        result
    }

    async fn run_turn(
        &self,
        request: ModelRequest,
        turn_id: Option<LifecycleId>,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>), TurnError> {
        let started_at = Instant::now();
        let (mut request, read_tool, write_tool) = self.prepare_request(request)?;
        let mut completed_tool_calls = 0_usize;
        let mut model_request_index = 0_u64;
        let mut model_requests = Vec::new();
        let mut tool_calls = Vec::new();

        loop {
            let (response, activity) = self
                .complete_model_request(&request, model_request_index, turn_id)
                .await?;
            model_requests.push(activity);
            model_request_index += 1;

            match response {
                ModelResponse::Output(output) => {
                    request.record_output(&output);
                    let report = TurnReport {
                        duration: started_at.elapsed(),
                        model_requests,
                        tool_calls,
                    };

                    return Ok((TurnOutcome { output, report }, request.into_messages()));
                }
                ModelResponse::ToolCall(call) => {
                    let (result, activity) = self
                        .execute_tool_call(
                            &call,
                            read_tool.as_ref(),
                            write_tool.as_ref(),
                            completed_tool_calls,
                            turn_id,
                        )
                        .await?;
                    request.record_tool_result(call, result);
                    tool_calls.push(activity);
                    completed_tool_calls += 1;
                }
                ModelResponse::ToolCalls(calls) => {
                    if calls.is_empty() {
                        return Err(ModelError::MissingToolCall.into());
                    }
                    ensure_unique_tool_call_ids(&calls)?;
                    if calls.len() > self.max_tool_calls.saturating_sub(completed_tool_calls) {
                        return Err(TurnError::ToolCallLimit {
                            limit: self.max_tool_calls,
                        });
                    }
                    let mut results = Vec::with_capacity(calls.len());
                    for call in &calls {
                        let (result, activity) = self
                            .execute_tool_call(
                                call,
                                read_tool.as_ref(),
                                write_tool.as_ref(),
                                completed_tool_calls,
                                turn_id,
                            )
                            .await?;
                        results.push(result);
                        tool_calls.push(activity);
                        completed_tool_calls += 1;
                    }
                    request.record_tool_results(calls, results);
                }
            }
        }
    }

    fn prepare_request(
        &self,
        mut request: ModelRequest,
    ) -> Result<(ModelRequest, Option<ReadTool>, Option<WriteTool>), TurnError> {
        if self.lifecycle.is_enabled() {
            request.mark_lifecycle_observed();
        }
        let read_allowed = self.policy.allows(Tool::Read);
        let write_allowed = self.policy.allows(Tool::Write);
        if !read_allowed && !write_allowed {
            return Ok((request, None, None));
        }
        let repository_root = self
            .repository_root
            .as_ref()
            .ok_or(TurnError::RepositoryRequired)?;
        let read_tool = read_allowed.then(|| {
            request = request.clone().with_tool(ToolDefinition::read());
            ReadTool::new(self.file_system.clone(), repository_root.clone())
        });
        let write_tool = write_allowed.then(|| {
            request = request.clone().with_tool(ToolDefinition::write());
            WriteTool::new(self.file_system.clone(), repository_root.clone())
        });

        Ok((request, read_tool, write_tool))
    }

    async fn complete_model_request(
        &self,
        request: &ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<(ModelResponse, ModelRequestActivity), TurnError> {
        let started_at = Instant::now();
        let model_lifecycle =
            self.lifecycle
                .start_model_request(self.model.metadata(), model_request_index, turn_id);
        let operation = self.model.complete_with_optional_metadata(request.clone());
        let completion = match model_lifecycle.as_ref() {
            Some(model_lifecycle) => model_lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (response, completion) = match completion {
            Ok(completion) => completion,
            Err(error) => {
                if let Some(model_lifecycle) = model_lifecycle {
                    model_lifecycle.failed(error.error_type(), error.http_status());
                }

                return Err(error.into());
            }
        };
        if let Some(output) = response.output()
            && let Err(error) = request.schema().validate_value(output)
        {
            let error = ModelError::from(error);
            if let Some(model_lifecycle) = model_lifecycle {
                model_lifecycle.failed(error.error_type(), error.http_status());
            }

            return Err(error.into());
        }
        let response_type = response.response_type();
        let activity = ModelRequestActivity {
            completion: completion.as_ref().map(sanitized_completion_metadata),
            duration: started_at.elapsed(),
            response_type,
        };
        if let Some(model_lifecycle) = model_lifecycle {
            model_lifecycle.completed(completion, response_type);
        }

        Ok((response, activity))
    }

    async fn execute_tool_call(
        &self,
        call: &ToolCall,
        read_tool: Option<&ReadTool>,
        write_tool: Option<&WriteTool>,
        completed_tool_calls: usize,
        turn_id: Option<LifecycleId>,
    ) -> Result<(String, ToolActivity), TurnError> {
        let mut tool_lifecycle =
            self.lifecycle
                .request_tool(call.id().to_string(), call.name().to_string(), turn_id);
        let execution = match call.arguments() {
            ToolCallArguments::Read(arguments) => {
                read_tool.map(|tool| ToolExecution::Read(tool, arguments))
            }
            ToolCallArguments::Write(arguments) => {
                write_tool.map(|tool| ToolExecution::Write(tool, arguments))
            }
        };
        let Some(execution) = execution else {
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.denied();
            }

            return Err(TurnError::ToolDenied {
                name: call.name().to_string(),
            });
        };
        if completed_tool_calls >= self.max_tool_calls {
            if let Some(tool_lifecycle) = tool_lifecycle {
                tool_lifecycle.failed(ToolErrorType::CallLimit);
            }

            return Err(TurnError::ToolCallLimit {
                limit: self.max_tool_calls,
            });
        }
        if let Some(tool_lifecycle) = tool_lifecycle.as_mut() {
            tool_lifecycle.started();
        }
        let operation = execute_tool(execution);
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
                        ToolActivity::ReadRejected { .. } | ToolActivity::WriteRejected { .. }
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
            Self::RepositoryRequired => TurnErrorType::RepositoryRequired,
            Self::ToolCallLimit { .. } => TurnErrorType::ToolCallLimit,
        }
    }
}

enum ToolExecution<'a> {
    Read(&'a ReadTool, &'a ReadArguments),
    Write(&'a WriteTool, &'a WriteArguments),
}

async fn execute_tool(execution: ToolExecution<'_>) -> Result<(String, ToolActivity), TurnError> {
    let started_at = Instant::now();

    match execution {
        ToolExecution::Read(read_tool, arguments) => match read_tool.execute(arguments).await {
            Ok(output) => {
                let activity = ToolActivity::Read {
                    duration: started_at.elapsed(),
                    end_line: output.end_line(),
                    path: sanitize_report_text(output.path()),
                    start_line: output.start_line(),
                    truncated: output.truncated(),
                };
                let result = output
                    .to_tool_result()
                    .map_err(ReadError::from)
                    .map_err(TurnError::from)?;

                Ok((result, activity))
            }
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
        },
        ToolExecution::Write(write_tool, arguments) => match write_tool.execute(arguments).await {
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
        },
    }
}

fn format_report_duration(duration: Duration) -> String {
    if duration.as_millis() == 0 {
        "<1 ms".to_string()
    } else {
        format!("{} ms", duration.as_millis())
    }
}

fn sanitized_completion_metadata(metadata: &CompletionMetadata) -> CompletionMetadata {
    CompletionMetadata::new(
        sanitize_report_text(metadata.finish_reason()),
        metadata.response_id().map(sanitize_report_text),
        metadata.response_model().map(sanitize_report_text),
        metadata.system_fingerprint().map(sanitize_report_text),
        metadata.usage().copied(),
    )
}

fn sanitize_report_text(text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use std::io::{self, Cursor};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use mockall::Sequence;
    use serde_json::json;

    use super::*;
    use crate::file_system::MockFileSystem;
    use crate::model::ModelMessage;

    fn model() -> crate::model::MockModel {
        let mut model = crate::model::MockModel::new();
        model.expect_metadata().return_const(None);

        model
    }

    fn object_schema() -> OutputSchema {
        OutputSchema::new(json!({
            "type": "object",
            "properties": { "summary": { "type": "string" } },
            "required": ["summary"],
            "additionalProperties": false
        }))
        .expect("schema should be valid")
    }

    fn read_call(id: &str) -> ToolCall {
        read_call_with_path(id, "Cargo.toml")
    }

    fn read_call_with_path(id: &str, path: &str) -> ToolCall {
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "path": path,
            "limit": 1
        }))
        .expect("read arguments should be valid");

        ToolCall::read(id.to_string(), arguments, None)
    }

    fn response_without_metadata(
        response: ModelResponse,
    ) -> (ModelResponse, Option<crate::CompletionMetadata>) {
        (response, None)
    }

    fn response_with_metadata(
        response: ModelResponse,
    ) -> (ModelResponse, Option<crate::CompletionMetadata>) {
        (
            response,
            Some(crate::CompletionMetadata::new(
                "stop\nforged".to_string(),
                Some("response\u{1b}-1".to_string()),
                Some("reported\nmodel".to_string()),
                Some("finger\tprint".to_string()),
                Some(crate::CompletionUsage::new(
                    None,
                    None,
                    Some(12),
                    Some(4),
                    None,
                    Some(16),
                )),
            )),
        )
    }

    fn write_call(id: &str, patch: &str) -> ToolCall {
        let arguments = serde_json::from_value::<WriteArguments>(json!({
            "path": "src/lib.rs",
            "patch": patch
        }))
        .expect("write arguments should be valid");

        ToolCall::write(id.to_string(), arguments, None)
    }

    fn readable_file_system() -> MockFileSystem {
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo/Cargo.toml")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| {
                Ok(Box::new(Cursor::new(
                    b"[workspace]\nmember = true\n".to_vec(),
                )))
            });

        file_system
    }

    fn turn_started_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
        match event.kind() {
            crate::LifecycleEventKind::TurnStarted { turn_id } => Some(*turn_id),
            _ => None,
        }
    }

    fn model_started_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
        match event.kind() {
            crate::LifecycleEventKind::ModelRequestStarted { model_call_id, .. } => {
                Some(*model_call_id)
            }
            _ => None,
        }
    }

    fn tool_requested_id(event: &crate::LifecycleEvent) -> Option<crate::LifecycleId> {
        match event.kind() {
            crate::LifecycleEventKind::ToolRequested { tool_call_id, .. } => Some(*tool_call_id),
            _ => None,
        }
    }

    fn assert_read_tool_lifecycle(events: &[crate::LifecycleEvent]) {
        let turn_id = turn_started_id(&events[0]).expect("first event should start the turn");
        let first_model_call_id =
            model_started_id(&events[1]).expect("second event should start the model request");
        assert!(matches!(
            events[1].kind(),
            crate::LifecycleEventKind::ModelRequestStarted {
                model: None,
                request_index: 0,
                turn_id: Some(event_turn_id),
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[2].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted {
                completion: None,
                model_call_id,
                response_type: crate::ModelResponseType::ToolCall,
                turn_id: Some(event_turn_id),
                ..
            } if *model_call_id == first_model_call_id && *event_turn_id == turn_id
        ));
        let tool_call_id =
            tool_requested_id(&events[3]).expect("fourth event should request the tool");
        assert!(matches!(
            events[3].kind(),
            crate::LifecycleEventKind::ToolRequested {
                provider_call_id,
                tool_name,
                turn_id: event_turn_id,
                ..
            } if provider_call_id == "provider-call-id"
                && tool_name == "read"
                && *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[4].kind(),
            crate::LifecycleEventKind::ToolStarted {
                tool_call_id: event_tool_call_id,
                turn_id: event_turn_id,
            } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[5].kind(),
            crate::LifecycleEventKind::ToolCompleted {
                tool_call_id: event_tool_call_id,
                turn_id: event_turn_id,
                ..
            } if *event_tool_call_id == tool_call_id && *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[6].kind(),
            crate::LifecycleEventKind::ModelRequestStarted {
                request_index: 1,
                turn_id: Some(event_turn_id),
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[7].kind(),
            crate::LifecycleEventKind::ModelRequestCompleted {
                completion: None,
                response_type: crate::ModelResponseType::Output,
                turn_id: Some(event_turn_id),
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(matches!(
            events[8].kind(),
            crate::LifecycleEventKind::TurnCompleted {
                turn_id: event_turn_id,
                ..
            } if *event_turn_id == turn_id
        ));
        assert!(turn_started_id(&events[1]).is_none());
        assert!(model_started_id(&events[0]).is_none());
        assert!(tool_requested_id(&events[0]).is_none());
    }

    #[tokio::test]
    async fn completes_read_tool_round_trip() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                let call_index = call_count.fetch_add(1, Ordering::SeqCst);
                if call_index == 0 {
                    assert_eq!(request.tools(), &[ToolDefinition::read()]);

                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        read_call("call_read"),
                    )));
                }
                assert_eq!(request.messages().len(), 3);
                assert!(matches!(
                    &request.messages()[0],
                    ModelMessage::User(prompt) if prompt == "inspect the manifest"
                ));
                assert!(matches!(
                    &request.messages()[1],
                    ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
                ));
                assert!(matches!(
                    &request.messages()[2],
                    ModelMessage::ToolResult {
                        call_id,
                        content,
                        name,
                    }
                        if call_id == "call_read"
                            && name == "read"
                            && serde_json::from_str::<Value>(content)
                                .is_ok_and(|value| value["content"] == "[workspace]")
                ));

                Ok(response_without_metadata(ModelResponse::Output(
                    json!({ "summary": "workspace" }),
                )))
            });
        let harness = Harness::new(model)
            .file_system(readable_file_system())
            .repository("repo")
            .allow(Tool::Read);

        // Act
        let output = harness
            .run("inspect the manifest", object_schema())
            .await
            .expect("tool round trip should succeed");

        // Assert
        assert_eq!(output, json!({ "summary": "workspace" }));
    }

    #[tokio::test]
    async fn completes_multiple_read_tools_from_one_model_response() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                        read_call("call_one"),
                        read_call("call_two"),
                    ])));
                }
                assert!(matches!(
                    &request.messages()[1],
                    ModelMessage::AssistantToolCalls(calls)
                        if calls.iter().map(ToolCall::id).collect::<Vec<_>>()
                            == ["call_one", "call_two"]
                ));
                assert!(matches!(
                    &request.messages()[2],
                    ModelMessage::ToolResult { call_id, .. } if call_id == "call_one"
                ));
                assert!(matches!(
                    &request.messages()[3],
                    ModelMessage::ToolResult { call_id, .. } if call_id == "call_two"
                ));

                Ok(response_without_metadata(ModelResponse::Output(
                    json!({ "summary": "workspace" }),
                )))
            });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(4)
            .returning(|path| {
                if path == std::path::Path::new("repo") {
                    Ok(PathBuf::from("/repo"))
                } else {
                    Ok(PathBuf::from("/repo/Cargo.toml"))
                }
            });
        file_system
            .expect_open_beneath()
            .times(2)
            .returning(|_, _| {
                Ok(Box::new(Cursor::new(
                    b"[workspace]\nmember = true\n".to_vec(),
                )))
            });
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Read);

        // Act
        let outcome = harness
            .run_report("inspect two files", object_schema())
            .await
            .expect("parallel tool round trip should succeed");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "workspace" }));
        assert_eq!(outcome.report().tool_calls().len(), 2);
    }

    #[tokio::test]
    async fn returns_correctable_read_rejection_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        read_call("call_read"),
                    )));
                }
                assert!(matches!(
                    &request.messages()[2],
                    ModelMessage::ToolResult { content, .. }
                        if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                            value["path"] == "Cargo.toml" && value["status"] == "rejected"
                        })
                ));

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let mut file_system = MockFileSystem::new();
        let mut sequence = Sequence::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_canonicalize()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")));
        file_system.expect_open_beneath().times(0);
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Read)
            .with_lifecycle_observer(move |event| {
                observed_events
                    .lock()
                    .expect("event recorder should not be poisoned")
                    .push(event);
            });

        // Act
        let outcome = harness
            .run_report("inspect", object_schema())
            .await
            .expect("model should recover from a rejected read path");

        // Assert
        assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
        assert!(matches!(
            outcome.report().tool_calls(),
            [ToolActivity::ReadRejected { path, .. }] if path == "Cargo.toml"
        ));
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert!(matches!(
            events[5].kind(),
            crate::LifecycleEventKind::ToolFailed {
                error_type: ToolErrorType::Execution,
                ..
            }
        ));
        assert!(matches!(
            events[8].kind(),
            crate::LifecycleEventKind::TurnCompleted { .. }
        ));
    }

    #[tokio::test]
    async fn chat_retains_successful_conversation_history() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                let call_index = call_count.fetch_add(1, Ordering::SeqCst);
                if call_index == 0 {
                    assert_eq!(
                        request.messages(),
                        &[ModelMessage::User("first question".to_string())]
                    );

                    return Ok(response_without_metadata(ModelResponse::Output(json!({
                        "summary": "first answer"
                    }))));
                }
                assert_eq!(
                    request.messages(),
                    &[
                        ModelMessage::User("first question".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
                        ModelMessage::User("second question".to_string()),
                    ]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "second answer"
                }))))
            });
        let harness = Harness::new(model);
        let mut chat = harness.chat(object_schema());

        // Act
        let first = chat
            .send("first question")
            .await
            .expect("first chat turn should succeed");
        let second = chat
            .send("second question")
            .await
            .expect("second chat turn should succeed");

        // Assert
        assert_eq!(first.output(), &json!({"summary": "first answer"}));
        assert_eq!(second.output(), &json!({"summary": "second answer"}));
        assert_eq!(second.report().model_requests().len(), 1);
        assert!(second.report().duration() >= second.report().model_requests()[0].duration());
    }

    #[tokio::test]
    async fn chat_sends_the_system_prompt_on_every_turn() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                let expected = if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    vec![
                        ModelMessage::System("read-only instructions".to_string()),
                        ModelMessage::User("first".to_string()),
                    ]
                } else {
                    vec![
                        ModelMessage::System("read-only instructions".to_string()),
                        ModelMessage::User("first".to_string()),
                        ModelMessage::Assistant(r#"{"summary":"one"}"#.to_string()),
                        ModelMessage::User("second".to_string()),
                    ]
                };
                assert_eq!(request.messages(), expected);

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": if expected.len() == 2 { "one" } else { "two" }
                }))))
            });
        let harness = Harness::new(model);
        let mut chat = harness
            .chat(object_schema())
            .with_system_prompt("read-only instructions");

        // Act
        chat.send("first")
            .await
            .expect("first chat turn should succeed");
        let second = chat
            .send("second")
            .await
            .expect("second chat turn should succeed");

        // Assert
        assert_eq!(second.output(), &json!({"summary": "two"}));
    }

    #[test]
    fn tool_activity_display_formats_every_outcome_safely() {
        // Arrange
        let empty_read = ToolActivity::Read {
            duration: Duration::ZERO,
            end_line: None,
            path: "empty\n\u{1b}]52;c;Y2xpcGJvYXJk\u{7}.txt".to_string(),
            start_line: 3,
            truncated: false,
        };
        let read = ToolActivity::Read {
            duration: Duration::from_millis(3),
            end_line: Some(7),
            path: "input.txt".to_string(),
            start_line: 2,
            truncated: true,
        };
        let rejected_read = ToolActivity::ReadRejected {
            duration: Duration::from_millis(1),
            path: "missing.txt".to_string(),
        };
        let write = ToolActivity::Write {
            bytes_written: 12,
            duration: Duration::from_millis(2),
            path: "output.txt".to_string(),
        };
        let rejected = ToolActivity::WriteRejected {
            duration: Duration::from_millis(1),
            path: "blocked.txt".to_string(),
        };

        // Act
        let displays = [
            empty_read.to_string(),
            read.to_string(),
            rejected_read.to_string(),
            write.to_string(),
            rejected.to_string(),
        ];

        // Assert
        assert_eq!(
            displays,
            [
                "read empty\u{fffd}\u{fffd}]52;c;Y2xpcGJvYXJk\u{fffd}.txt (line 3; <1 ms)",
                "read input.txt (lines 2-7, truncated; 3 ms)",
                "read missing.txt (rejected; 1 ms)",
                "write output.txt (12 bytes; 2 ms)",
                "write blocked.txt (rejected; 1 ms)",
            ]
        );
        assert_eq!(rejected_read.duration(), Duration::from_millis(1));
        assert_eq!(rejected_read.name(), "read");
        assert_eq!(rejected_read.path(), "missing.txt");
    }

    #[test]
    fn chat_history_evicts_complete_tool_turns() {
        // Arrange
        let tool_turn = vec![
            ModelMessage::User("inspect".to_string()),
            ModelMessage::AssistantToolCall(read_call("call_read")),
            ModelMessage::ToolResult {
                call_id: "call_read".to_string(),
                content: "file contents".to_string(),
                name: "read".to_string(),
            },
            ModelMessage::Assistant(r#"{"summary":"old"}"#.to_string()),
        ];
        let latest_turn = vec![
            ModelMessage::User("latest".to_string()),
            ModelMessage::Assistant(r#"{"summary":"new"}"#.to_string()),
        ];
        let max_bytes = retained_bytes(&tool_turn).max(retained_bytes(&latest_turn));
        let mut history = ChatHistory::new(max_bytes);

        // Act
        history.push(tool_turn);
        history.push(latest_turn.clone());

        // Assert
        assert_eq!(history.messages(), latest_turn);
        assert!(history.bytes <= max_bytes);
    }

    #[tokio::test]
    async fn chat_applies_the_configured_history_budget() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(3)
            .returning(
                move |request| match call_count.fetch_add(1, Ordering::SeqCst) {
                    0 => {
                        assert_eq!(
                            request.messages(),
                            &[ModelMessage::User("first".to_string())]
                        );

                        Ok(response_without_metadata(ModelResponse::Output(json!({
                            "summary": "xxxxxxxxxxxxxxxxxxxx"
                        }))))
                    }
                    1 => {
                        assert_eq!(request.messages().len(), 3);

                        Ok(response_without_metadata(ModelResponse::Output(json!({
                            "summary": "two"
                        }))))
                    }
                    _ => {
                        assert_eq!(
                            request.messages(),
                            &[
                                ModelMessage::User("second".to_string()),
                                ModelMessage::Assistant(r#"{"summary":"two"}"#.to_string()),
                                ModelMessage::User("third".to_string()),
                            ]
                        );

                        Ok(response_without_metadata(ModelResponse::Output(json!({
                            "summary": "three"
                        }))))
                    }
                },
            );
        let harness = Harness::new(model)
            .max_history_bytes(NonZeroUsize::new(50).expect("history budget should be nonzero"));
        let mut chat = harness.chat(object_schema());

        // Act
        chat.send("first")
            .await
            .expect("first chat turn should succeed");
        chat.send("second")
            .await
            .expect("second chat turn should succeed");
        let third = chat
            .send("third")
            .await
            .expect("third chat turn should succeed");

        // Assert
        assert_eq!(third.output(), &json!({"summary": "three"}));
    }

    #[tokio::test]
    async fn chat_does_not_retain_a_failed_turn() {
        // Arrange
        let mut model = model();
        let mut sequence = Sequence::new();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|_| Err(ModelError::InvalidResponse));
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|request| {
                assert_eq!(
                    request.messages(),
                    &[ModelMessage::User("retry".to_string())]
                );

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let harness = Harness::new(model);
        let mut chat = harness.chat(object_schema());

        // Act
        let error = chat
            .send("failed question")
            .await
            .expect_err("the first turn should fail");
        let recovered = chat
            .send("retry")
            .await
            .expect("the next turn should start from clean history");

        // Assert
        assert!(matches!(
            error,
            TurnError::Model(ModelError::InvalidResponse)
        ));
        assert_eq!(recovered.output(), &json!({"summary": "recovered"}));
    }

    #[tokio::test]
    async fn rejects_schema_invalid_output_from_injected_model() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": 42
                }))))
            });
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness = Harness::new(model).with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("schema-invalid custom output should fail");

        // Assert
        assert!(matches!(
            error,
            TurnError::Model(ModelError::SchemaViolation { path, .. }) if path == "/summary"
        ));
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert!(matches!(
            events[2].kind(),
            crate::LifecycleEventKind::ModelRequestFailed {
                error_type: crate::ModelErrorType::InvalidOutput,
                ..
            }
        ));
        assert!(matches!(
            events[3].kind(),
            crate::LifecycleEventKind::TurnFailed {
                error_type: TurnErrorType::Model(crate::ModelErrorType::InvalidOutput),
                ..
            }
        ));
    }

    #[tokio::test]
    async fn report_describes_model_requests_and_repository_reads() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |_| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        read_call_with_path("call_read", "Cargo\n.toml\u{1b}"),
                    )));
                }

                Ok(response_with_metadata(ModelResponse::Output(json!({
                    "summary": "workspace"
                }))))
            });
        let harness = Harness::new(model)
            .file_system(readable_file_system())
            .repository("repo")
            .allow(Tool::Read);

        // Act
        let outcome = harness
            .run_report("inspect", object_schema())
            .await
            .expect("reported turn should succeed");

        // Assert
        assert_eq!(outcome.output(), &json!({"summary": "workspace"}));
        assert_eq!(outcome.report().model_requests().len(), 2);
        let final_request = &outcome.report().model_requests()[1];
        assert_eq!(
            final_request.response_type(),
            crate::ModelResponseType::Output
        );
        let metadata = final_request
            .completion()
            .expect("metadata should be present");
        assert_eq!(metadata.finish_reason(), "stop\u{fffd}forged");
        assert_eq!(metadata.response_id(), Some("response\u{fffd}-1"));
        assert_eq!(metadata.response_model(), Some("reported\u{fffd}model"));
        assert_eq!(metadata.system_fingerprint(), Some("finger\u{fffd}print"));
        assert_eq!(
            metadata.usage().and_then(|usage| usage.total_tokens()),
            Some(16)
        );
        assert_eq!(outcome.report().tool_calls().len(), 1);
        let activity = &outcome.report().tool_calls()[0];
        assert_eq!(activity.name(), "read");
        assert_eq!(activity.path(), "Cargo\u{fffd}.toml\u{fffd}");
        assert!(activity.duration() <= outcome.report().duration());
        assert!(matches!(
            activity,
            ToolActivity::Read {
                end_line: Some(1),
                start_line: 1,
                truncated: true,
                ..
            }
        ));
    }

    #[test]
    fn write_activity_exposes_only_sanitized_summary() {
        // Arrange
        let activity = ToolActivity::Write {
            bytes_written: 42,
            duration: Duration::from_millis(3),
            path: "src/lib.rs".to_string(),
        };

        // Act and Assert
        assert_eq!(activity.name(), "write");
        assert_eq!(activity.path(), "src/lib.rs");
        assert_eq!(activity.duration(), Duration::from_millis(3));
        assert!(matches!(
            activity,
            ToolActivity::Write {
                bytes_written: 42,
                ..
            }
        ));

        let rejected = ToolActivity::WriteRejected {
            duration: Duration::from_millis(2),
            path: "src/rejected.rs".to_string(),
        };
        assert_eq!(rejected.name(), "write");
        assert_eq!(rejected.path(), "src/rejected.rs");
        assert_eq!(rejected.duration(), Duration::from_millis(2));
    }

    #[tokio::test]
    async fn emits_correlated_lifecycle_for_read_tool_round_trip() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                assert!(request.lifecycle_observed());
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        read_call("provider-call-id"),
                    )));
                }

                Ok(response_without_metadata(ModelResponse::Output(
                    json!({ "summary": "workspace" }),
                )))
            });
        let events = Arc::new(Mutex::new(Vec::new()));
        let observed_events = Arc::clone(&events);
        let harness = Harness::new(model)
            .file_system(readable_file_system())
            .repository("repo")
            .allow(Tool::Read)
            .with_lifecycle_observer(move |event| {
                observed_events
                    .lock()
                    .expect("event recorder should not be poisoned")
                    .push(event);
            });

        // Act
        let output = harness
            .run("sensitive prompt", object_schema())
            .await
            .expect("tool round trip should succeed");

        // Assert
        assert_eq!(output, json!({ "summary": "workspace" }));
        let events = events
            .lock()
            .expect("event recorder should not be poisoned");
        assert_eq!(events.len(), 9);
        assert_eq!(
            events
                .iter()
                .map(crate::LifecycleEvent::sequence)
                .collect::<Vec<_>>(),
            (0..9).collect::<Vec<_>>()
        );
        assert_read_tool_lifecycle(&events);
        let event_debug = format!("{events:?}");
        assert!(!event_debug.contains("sensitive prompt"));
        assert!(!event_debug.contains("[workspace]"));
    }

    #[tokio::test]
    async fn requires_repository_when_read_is_allowed() {
        // Arrange
        let mut model = model();
        model.expect_complete_with_optional_metadata().times(0);
        let harness = Harness::new(model)
            .allow(Tool::Read)
            .with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("read should require a repository root");

        // Assert
        assert!(matches!(&error, TurnError::RepositoryRequired));
        assert_eq!(error.error_type(), TurnErrorType::RepositoryRequired);
    }

    #[tokio::test]
    async fn completes_write_tool_round_trip() {
        // Arrange
        let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                let call_index = call_count.fetch_add(1, Ordering::SeqCst);
                if call_index == 0 {
                    assert_eq!(request.tools(), &[ToolDefinition::write()]);

                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        write_call("call_write", patch),
                    )));
                }
                assert!(matches!(
                    &request.messages()[2],
                    ModelMessage::ToolResult {
                        call_id,
                        content,
                        name,
                    }
                        if call_id == "call_write"
                            && name == "write"
                            && serde_json::from_str::<Value>(content).is_ok_and(|value| {
                                value == json!({
                                    "bytes_written": 4,
                                    "path": "src/lib.rs",
                                    "status": "applied"
                                })
                            })
                ));

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "updated"
                }))))
            });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
        file_system
            .expect_replace_beneath()
            .times(1)
            .withf(|_, _, expected, content| {
                expected.as_deref() == Some(b"old\n".as_slice()) && content == b"new\n"
            })
            .returning(|_, _, _, _| Ok(()));
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Write);

        // Act
        let output = harness
            .run("update the file", object_schema())
            .await
            .expect("write round trip should succeed");

        // Assert
        assert_eq!(output, json!({ "summary": "updated" }));
    }

    #[tokio::test]
    async fn returns_correctable_write_rejection_to_model() {
        // Arrange
        let mut model = model();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |request| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        write_call("call_write", "not a unified diff"),
                    )));
                }
                assert!(matches!(
                    &request.messages()[2],
                    ModelMessage::ToolResult { content, .. }
                        if serde_json::from_str::<Value>(content)
                            .is_ok_and(|value| value["status"] == "rejected")
                ));

                Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "recovered"
                }))))
            });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Ok(PathBuf::from("/repo")));
        file_system
            .expect_open_beneath()
            .times(1)
            .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
        file_system.expect_replace_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Write);

        // Act
        let output = harness
            .run("update", object_schema())
            .await
            .expect("model should recover from rejected patch");

        // Assert
        assert_eq!(output, json!({ "summary": "recovered" }));
    }

    #[tokio::test]
    async fn returns_terminal_write_boundary_failure() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCall(
                    write_call(
                        "call_write",
                        "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                    ),
                )))
            });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Write);

        // Act
        let error = harness
            .run("update", object_schema())
            .await
            .expect_err("write boundary failure should end turn");

        // Assert
        assert!(matches!(
            &error,
            TurnError::Write(WriteError::RepositoryRoot { .. })
        ));
        assert_eq!(error.error_type(), TurnErrorType::Tool);
    }

    #[tokio::test]
    async fn rejects_write_call_when_policy_denies_write() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|request| {
                assert_eq!(request.tools(), []);

                Ok(response_without_metadata(ModelResponse::ToolCall(
                    write_call(
                        "call_denied",
                        "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
                    ),
                )))
            });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        file_system.expect_replace_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo");

        // Act
        let error = harness
            .run("update", object_schema())
            .await
            .expect_err("denied write should fail");

        // Assert
        assert!(matches!(
            error,
            TurnError::ToolDenied { name } if name == "write"
        ));
    }

    #[tokio::test]
    async fn rejects_tool_call_when_policy_denies_read() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|request| {
                assert_eq!(request.tools(), []);

                Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("call_denied"),
                )))
            });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("denied tool should fail");

        // Assert
        assert!(matches!(
            &error,
            TurnError::ToolDenied { name } if name == "read"
        ));
        assert_eq!(error.error_type(), TurnErrorType::ToolDenied);
    }

    #[tokio::test]
    async fn enforces_tool_call_limit() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("call_read"),
                )))
            });
        let harness = Harness::new(model)
            .file_system(readable_file_system())
            .repository("repo")
            .allow(Tool::Read)
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"))
            .with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("second tool call should exceed the limit");

        // Assert
        assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
        assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
    }

    #[tokio::test]
    async fn enforces_tool_call_limit_within_one_model_response() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                    read_call("call_one"),
                    read_call("call_two"),
                ])))
            });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Read)
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("second batched tool call should exceed the limit");

        // Assert
        assert!(matches!(error, TurnError::ToolCallLimit { limit: 1 }));
    }

    #[tokio::test]
    async fn rejects_batched_writes_before_any_write_executes() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                    write_call(
                        "call_one",
                        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
                    ),
                    write_call(
                        "call_two",
                        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
                    ),
                ])))
            });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        file_system.expect_replace_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Write)
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

        // Act
        let error = harness
            .run("update twice", object_schema())
            .await
            .expect_err("oversized batch should fail before writing");

        // Assert
        assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
        assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
    }

    #[tokio::test]
    async fn rejects_duplicate_batched_call_ids_before_any_write_executes() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                    write_call(
                        "duplicate_call",
                        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
                    ),
                    write_call(
                        "duplicate_call",
                        "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
                    ),
                ])))
            });
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        file_system.expect_replace_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Write);

        // Act
        let error = harness
            .run("update twice", object_schema())
            .await
            .expect_err("duplicate call identifiers should fail before writing");

        // Assert
        assert!(matches!(
            &error,
            TurnError::Model(ModelError::DuplicateToolCallId { id }) if id == "duplicate_call"
        ));
        assert_eq!(
            error.error_type(),
            TurnErrorType::Model(crate::ModelErrorType::InvalidToolCall)
        );
    }

    #[tokio::test]
    async fn rejects_empty_tool_call_batch() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCalls(
                    Vec::new(),
                )))
            });
        let harness = Harness::new(model);

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("empty tool batch should fail immediately");

        // Assert
        assert!(matches!(
            &error,
            TurnError::Model(ModelError::MissingToolCall)
        ));
        assert_eq!(
            error.error_type(),
            TurnErrorType::Model(crate::ModelErrorType::InvalidToolCall)
        );
    }

    #[tokio::test]
    async fn returns_typed_read_failure() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| {
                Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("call_read"),
                )))
            });
        let mut file_system = MockFileSystem::new();
        file_system
            .expect_canonicalize()
            .times(1)
            .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .allow(Tool::Read)
            .with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("filesystem failure should end the turn");

        // Assert
        assert!(matches!(
            &error,
            TurnError::Read(ReadError::RepositoryRoot { .. })
        ));
        assert_eq!(error.error_type(), TurnErrorType::Tool);
    }

    #[tokio::test]
    async fn returns_typed_model_failure() {
        // Arrange
        let mut model = model();
        model
            .expect_complete_with_optional_metadata()
            .times(1)
            .returning(|_| Err(ModelError::request(io::Error::other("offline"))));
        let mut file_system = MockFileSystem::new();
        file_system.expect_canonicalize().times(0);
        file_system.expect_open_beneath().times(0);
        let harness = Harness::new(model)
            .file_system(file_system)
            .repository("repo")
            .with_lifecycle_observer(|_| {});

        // Act
        let error = harness
            .run("inspect", object_schema())
            .await
            .expect_err("model failure should end the turn");

        // Assert
        assert!(matches!(&error, TurnError::Model(ModelError::Request(_))));
        assert_eq!(
            error.error_type(),
            TurnErrorType::Model(crate::ModelErrorType::Request)
        );
    }
}
