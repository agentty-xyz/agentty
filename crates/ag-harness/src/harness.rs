use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::Value;
use thiserror::Error;

use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::{
    LifecycleEmitter, LifecycleId, LifecycleObserver, ToolErrorType, TurnErrorType, TurnLifecycle,
};
use crate::model::{
    CompletionMetadata, Model, ModelError, ModelMessage, ModelRequest, ModelResponse,
};
use crate::policy::Policy;
use crate::read::{ReadError, ReadTool};
use crate::schema_contract::OutputSchema;
use crate::tool::{
    ReadArguments, Tool, ToolCall, ToolCallArguments, ToolDefinition, WriteArguments,
};
use crate::write::{WriteError, WriteTool};

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
    /// Returns normalized provider completion metadata, when available.
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
            | Self::Write { duration, .. }
            | Self::WriteRejected { duration, .. } => *duration,
        }
    }

    /// Returns the bounded built-in tool name.
    pub fn name(&self) -> &'static str {
        match self {
            Self::Read { .. } => "read",
            Self::Write { .. } | Self::WriteRejected { .. } => "write",
        }
    }

    /// Returns the repository-relative target path.
    pub fn path(&self) -> &str {
        match self {
            Self::Read { path, .. }
            | Self::Write { path, .. }
            | Self::WriteRejected { path, .. } => path,
        }
    }
}

/// In-memory sequence of model turns sharing conversation and tool history.
pub struct ChatSession<'a> {
    harness: &'a Harness,
    messages: Vec<ModelMessage>,
    schema: OutputSchema,
}

impl ChatSession<'_> {
    /// Sends one prompt and retains the successful turn in session history.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] under the same conditions as [`Harness::run`]. A
    /// failed turn is not added to the conversation history.
    pub async fn send(&mut self, prompt: impl Into<String>) -> Result<TurnOutcome, TurnError> {
        let request =
            ModelRequest::with_history(self.messages.clone(), prompt.into(), self.schema.clone());
        let (outcome, messages) = self.harness.run_request(request).await?;
        self.messages = messages;

        Ok(outcome)
    }
}

/// Application-facing harness for one complete model turn.
///
/// A turn advertises policy-approved tools, executes validated native calls,
/// returns tool results to the model, and finishes with locally validated
/// structured output.
pub struct Harness {
    file_system: Arc<dyn FileSystem>,
    lifecycle: LifecycleEmitter,
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
            messages: Vec::new(),
            schema,
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
                .start_model_request(None, model_request_index, turn_id);
        let (response, completion) = match self
            .model
            .complete_with_optional_metadata(request.clone())
            .await
        {
            Ok(completion) => completion,
            Err(error) => {
                if let Some(model_lifecycle) = model_lifecycle {
                    model_lifecycle.failed(error.error_type());
                }

                return Err(error.into());
            }
        };
        let response_type = response.response_type();
        let activity = ModelRequestActivity {
            completion: completion.clone(),
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
        let mut tool_lifecycle = self
            .lifecycle
            .request_tool(call.name().to_string(), turn_id);
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
        match execute_tool(execution).await {
            Ok(result) => {
                if let Some(tool_lifecycle) = tool_lifecycle {
                    tool_lifecycle.completed();
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
        ToolExecution::Read(read_tool, arguments) => {
            let output = read_tool.execute(arguments).await?;
            let activity = ToolActivity::Read {
                duration: started_at.elapsed(),
                end_line: output.end_line(),
                path: output.path().to_string(),
                start_line: output.start_line(),
                truncated: output.truncated(),
            };
            let result = output
                .to_tool_result()
                .map_err(ReadError::from)
                .map_err(TurnError::from)?;

            Ok((result, activity))
        }
        ToolExecution::Write(write_tool, arguments) => match write_tool.execute(arguments).await {
            Ok(output) => {
                let activity = ToolActivity::Write {
                    bytes_written: output.bytes_written(),
                    duration: started_at.elapsed(),
                    path: output.path().to_string(),
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
                            path: arguments.path().to_string(),
                        },
                    )
                }),
            Err(error) => Err(error.into()),
        },
    }
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
    use crate::model::{MockModel, ModelMessage};

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
        let arguments = serde_json::from_value::<ReadArguments>(json!({
            "path": "Cargo.toml",
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
                "stop".to_string(),
                Some("response-1".to_string()),
                Some("reported-model".to_string()),
                None,
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
                tool_name,
                turn_id: event_turn_id,
                ..
            } if tool_name == "read" && *event_turn_id == turn_id
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
        let mut model = MockModel::new();
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
    async fn chat_retains_successful_conversation_history() {
        // Arrange
        let mut model = MockModel::new();
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
    async fn chat_does_not_retain_a_failed_turn() {
        // Arrange
        let mut model = MockModel::new();
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
    async fn report_describes_model_requests_and_repository_reads() {
        // Arrange
        let mut model = MockModel::new();
        let call_count = Arc::new(AtomicUsize::new(0));
        model
            .expect_complete_with_optional_metadata()
            .times(2)
            .returning(move |_| {
                if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Ok(response_without_metadata(ModelResponse::ToolCall(
                        read_call("call_read"),
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
        assert_eq!(
            final_request
                .completion()
                .expect("metadata should be present")
                .response_model(),
            Some("reported-model")
        );
        assert_eq!(outcome.report().tool_calls().len(), 1);
        let activity = &outcome.report().tool_calls()[0];
        assert_eq!(activity.name(), "read");
        assert_eq!(activity.path(), "Cargo.toml");
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
        let mut model = MockModel::new();
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
        assert!(!event_debug.contains("provider-call-id"));
        assert!(!event_debug.contains("[workspace]"));
    }

    #[tokio::test]
    async fn requires_repository_when_read_is_allowed() {
        // Arrange
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
    async fn returns_typed_read_failure() {
        // Arrange
        let mut model = MockModel::new();
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
        let mut model = MockModel::new();
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
