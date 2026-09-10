use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::sync::OnceCell;

use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::{
    LifecycleEmitter, LifecycleId, LifecycleObserver, ToolErrorType, ToolLifecycle, TurnErrorType,
    TurnLifecycle,
};
use crate::model::{
    Model, ModelError, ModelMessage, ModelRequest, ModelResponse, ReasoningEffort,
    ensure_unique_tool_call_ids,
};
use crate::policy::Policy;
use crate::read::{self, ReadError, ReadTool};
use crate::repository::Repository;
use crate::schema_contract::OutputSchema;
use crate::session::{AcquiredTurn, Database, LoadedSession, NewSession, SessionError};
use crate::tool::{
    ReadAction, ReadArguments, Tool, ToolCall, ToolCallArguments, ToolDefinition, WriteArguments,
};
use crate::turn::{
    ModelRequestActivity, ResumeFailure, ToolActivity, TurnError, TurnOutcome, TurnReport,
    sanitize_report_text, sanitized_completion_metadata,
};
use crate::write::{WriteError, WriteTool};

const DEFAULT_MAX_HISTORY_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_TOOL_CALLS: usize = 8;

/// Durable, resumable sequence of model turns.
pub struct Session<'a> {
    database: Database,
    harness: &'a Harness,
    history: SessionHistory,
    id: String,
    provider_session_id: Option<String>,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl Session<'_> {
    /// Returns the stable application-provided session identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Sends one prompt and durably records its lifecycle and messages.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the model turn or persistence operation
    /// fails.
    pub async fn send(&mut self, prompt: impl Into<String>) -> Result<TurnOutcome, SessionError> {
        let started_at = Instant::now();
        let turn = self.harness.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let mut result = self.send_turn(prompt.into(), turn_id).await;
        if let Ok(outcome) = &mut result {
            outcome.set_duration(started_at.elapsed());
        }
        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(
                    SessionError::Turn(error) | SessionError::TurnPersistence { turn: error, .. },
                ) => {
                    turn.failed(error.error_type());
                }
                Err(_) => turn.failed(TurnErrorType::Session),
            }
        }

        result
    }

    async fn send_turn(
        &mut self,
        prompt: String,
        turn_id: Option<LifecycleId>,
    ) -> Result<TurnOutcome, SessionError> {
        let AcquiredTurn {
            mut guard,
            provider_session_id,
            turn_position,
            turns,
        } = self.database.begin_turn(&self.id, &prompt).await?;
        self.history.replace(turns);
        self.provider_session_id = provider_session_id;
        let mut messages = self.history.messages();
        if let Some(system_prompt) = &self.system_prompt {
            messages.insert(0, ModelMessage::System(system_prompt.clone()));
        }
        let retained_messages = messages.len();
        let mut request = ModelRequest::with_history(messages, prompt, self.schema.clone());
        request.set_provider_session_id(self.provider_session_id.clone());
        let result = tokio::select! {
            biased;
            error = guard.ownership_failure() => {
                guard.mark_interrupted();

                return Err(error);
            }
            result = self.harness.run_turn(request, turn_id) => result,
        };
        let (outcome, mut messages, provider_session_id) = match result {
            Ok(result) => result,
            Err(error) => {
                self.provider_session_id = None;
                let persistence = self
                    .database
                    .fail_turn(&self.id, turn_position, &error)
                    .await;
                if let Err(persistence) = persistence {
                    guard.mark_interrupted();

                    return Err(SessionError::TurnPersistence {
                        turn: error,
                        persistence: Box::new(persistence),
                    });
                }
                guard.disarm();

                return Err(error.into());
            }
        };
        let turn = messages.split_off(retained_messages);
        let persistence = self
            .database
            .complete_turn(
                &self.id,
                turn_position,
                &turn[1..],
                provider_session_id.as_deref(),
            )
            .await;
        if let Err(error) = persistence {
            guard.mark_interrupted();

            return Err(error);
        }
        guard.disarm();
        self.provider_session_id = provider_session_id;
        self.history.push(turn);

        Ok(outcome)
    }
}

/// Builder for one durable session.
pub struct SessionBuilder<'a> {
    harness: &'a Harness,
    id: String,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl<'a> SessionBuilder<'a> {
    /// Adds a system prompt that is restored with the session.
    #[must_use]
    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }

    /// Creates the session in the configured database.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when storage is not configured, the identifier
    /// already exists, or SQLite cannot create the session.
    pub async fn create(self) -> Result<Session<'a>, SessionError> {
        self.harness.create_session(self).await
    }
}

pub(crate) struct SessionHistory {
    bytes: usize,
    max_bytes: usize,
    turns: VecDeque<Vec<ModelMessage>>,
}

impl SessionHistory {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            bytes: 0,
            max_bytes,
            turns: VecDeque::new(),
        }
    }

    pub(crate) fn messages(&self) -> Vec<ModelMessage> {
        self.turns
            .iter()
            .flat_map(|turn| turn.iter().cloned())
            .collect()
    }

    pub(crate) fn push(&mut self, turn: Vec<ModelMessage>) {
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

    fn replace(&mut self, turns: Vec<Vec<ModelMessage>>) {
        self.bytes = 0;
        self.turns.clear();
        for turn in turns {
            self.push(turn);
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
    database: OnceCell<Database>,
    database_path: Option<PathBuf>,
    file_system: Arc<dyn FileSystem>,
    lifecycle: LifecycleEmitter,
    max_history_bytes: usize,
    max_tool_calls: usize,
    model: Arc<dyn Model>,
    model_reasoning_effort: Option<ReasoningEffort>,
    policy: Policy,
    repository: Option<Repository>,
}

impl Harness {
    /// Creates a deny-by-default harness backed by the local filesystem.
    pub fn new(model: impl Model + 'static) -> Self {
        Self {
            database: OnceCell::new(),
            database_path: None,
            file_system: Arc::new(LocalFileSystem),
            lifecycle: LifecycleEmitter::default(),
            max_history_bytes: DEFAULT_MAX_HISTORY_BYTES,
            max_tool_calls: DEFAULT_MAX_TOOL_CALLS,
            model: Arc::new(model),
            model_reasoning_effort: None,
            policy: Policy::default(),
            repository: None,
        }
    }

    /// Configures the SQLite database used by durable sessions.
    ///
    /// The first create or resume initializes one shared connection pool and
    /// runs migrations. Reconfiguring the path resets that shared database.
    #[must_use]
    pub fn database(mut self, path: impl Into<PathBuf>) -> Self {
        self.database = OnceCell::new();
        self.database_path = Some(path.into());

        self
    }

    /// Configures the validated repository root and Git executable used by
    /// tools.
    #[must_use]
    pub fn repository(mut self, repository: Repository) -> Self {
        self.repository = Some(repository);

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

    /// Sets the reasoning depth for model calls that do not specify one.
    #[must_use]
    pub fn model_reasoning_effort(mut self, reasoning_effort: ReasoningEffort) -> Self {
        self.model_reasoning_effort = Some(reasoning_effort);

        self
    }

    /// Runs one prompt without creating durable session history.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when the model fails, requests a denied tool,
    /// exceeds the call limit, or a requested repository operation fails.
    pub async fn run_once(
        &self,
        prompt: impl Into<String>,
        schema: OutputSchema,
    ) -> Result<TurnOutcome, TurnError> {
        let request = ModelRequest::new(prompt, schema);
        let turn = self.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let result = self.run_turn(request, turn_id).await;

        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(error) => turn.failed(error.error_type()),
            }
        }

        result.map(|(outcome, _, _)| outcome)
    }

    /// Builds a new durable session whose responses must match `schema`.
    pub fn session(&self, id: impl Into<String>, schema: OutputSchema) -> SessionBuilder<'_> {
        SessionBuilder {
            harness: self,
            id: id.into(),
            schema,
            system_prompt: None,
        }
    }

    /// Resumes a durable session and restores its bounded completed history.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when storage is not configured, the session is
    /// missing, its model differs, or SQLite cannot load it.
    pub async fn resume(&self, id: &str) -> Result<Session<'_>, SessionError> {
        let database = self.open_database().await?;
        let loaded = database.load_session(id).await?;
        self.validate_session_model(id, &loaded)?;
        let mut history = SessionHistory::new(loaded.max_history_bytes);
        for turn in loaded.turns {
            history.push(turn);
        }

        Ok(Session {
            database,
            harness: self,
            history,
            id: id.to_string(),
            provider_session_id: loaded.provider_session_id,
            schema: loaded.schema,
            system_prompt: loaded.system_prompt,
        })
    }

    async fn create_session<'a>(
        &'a self,
        builder: SessionBuilder<'a>,
    ) -> Result<Session<'a>, SessionError> {
        let database = self.open_database().await?;
        let config = NewSession::new(builder.id, builder.schema)
            .with_optional_system_prompt(builder.system_prompt);
        database
            .create_session(&config, self.model.metadata(), self.max_history_bytes)
            .await?;

        Ok(Session {
            database,
            harness: self,
            history: SessionHistory::new(self.max_history_bytes),
            id: config.id().to_string(),
            provider_session_id: None,
            schema: config.schema().clone(),
            system_prompt: config.system_prompt().map(str::to_string),
        })
    }

    async fn open_database(&self) -> Result<Database, SessionError> {
        let path = self
            .database_path
            .as_deref()
            .ok_or(SessionError::StorageRequired)?;

        self.database
            .get_or_try_init(|| Database::open(path))
            .await
            .cloned()
    }

    fn validate_session_model(&self, id: &str, loaded: &LoadedSession) -> Result<(), SessionError> {
        let (Some(stored_provider), Some(stored_model)) = (&loaded.provider, &loaded.model) else {
            if loaded.provider.is_none() && loaded.model.is_none() {
                return Ok(());
            }

            return Err(SessionError::InvalidData {
                reason: format!("session `{id}` has incomplete model identity"),
            });
        };
        let metadata = self.model.metadata();
        let (actual_provider, actual_model) = metadata.as_ref().map_or(
            ("unavailable".to_string(), "unavailable".to_string()),
            |metadata| {
                (
                    metadata.provider().to_string(),
                    metadata.model().to_string(),
                )
            },
        );
        if actual_provider != *stored_provider || actual_model != *stored_model {
            return Err(SessionError::ModelMismatch {
                actual_model,
                actual_provider,
                id: id.to_string(),
                stored_model: stored_model.clone(),
                stored_provider: stored_provider.clone(),
            });
        }

        Ok(())
    }

    async fn run_turn(
        &self,
        request: ModelRequest,
        turn_id: Option<LifecycleId>,
    ) -> Result<(TurnOutcome, Vec<ModelMessage>, Option<String>), TurnError> {
        let started_at = Instant::now();
        let (mut request, read_tool, write_tool) = self.prepare_request(request)?;
        let mut completed_tool_calls = 0_usize;
        let mut model_request_index = 0_u64;
        let mut model_requests = Vec::new();
        let mut tool_calls = Vec::new();

        loop {
            let (
                response,
                activities,
                provider_session_id,
                native_resume_rejected,
                reasoning_content,
            ) = self
                .complete_model_request(&request, model_request_index, turn_id)
                .await?;
            if native_resume_rejected || provider_session_id.is_some() {
                request.set_provider_session_id(provider_session_id);
            }
            model_request_index = model_request_index
                .saturating_add(u64::try_from(activities.len()).unwrap_or(u64::MAX));
            model_requests.extend(activities);

            match response {
                ModelResponse::Output(output) => {
                    request.record_output_with_reasoning(&output, reasoning_content);
                    let report = TurnReport::new(started_at.elapsed(), model_requests, tool_calls);

                    let provider_session_id = request.provider_session_id().map(str::to_string);

                    return Ok((
                        TurnOutcome::new(output, report),
                        request.into_messages(),
                        provider_session_id,
                    ));
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
        if request.model_reasoning_effort().is_none()
            && let Some(reasoning_effort) = self.model_reasoning_effort
        {
            request = request.with_model_reasoning_effort(reasoning_effort);
        }
        let read_allowed = self.policy.allows(Tool::Read);
        let write_allowed = self.policy.allows(Tool::Write);
        if !read_allowed && !write_allowed {
            return Ok((request, None, None));
        }
        let repository = self
            .repository
            .as_ref()
            .ok_or(TurnError::RepositoryRequired)?;
        let read_tool = read_allowed.then(|| {
            request = request.clone().with_tool(ToolDefinition::read());
            ReadTool::with_git(
                self.file_system.clone(),
                repository.root().to_path_buf(),
                repository.git_executable().to_path_buf(),
            )
        });
        let write_tool = write_allowed.then(|| {
            request = request.clone().with_tool(ToolDefinition::write());
            WriteTool::new(self.file_system.clone(), repository.root().to_path_buf())
        });

        Ok((request, read_tool, write_tool))
    }

    async fn complete_model_request(
        &self,
        request: &ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<
        (
            ModelResponse,
            Vec<ModelRequestActivity>,
            Option<String>,
            bool,
            Option<String>,
        ),
        TurnError,
    > {
        let native_resume = request.provider_session_id().is_some();
        match self
            .complete_model_attempt(request.clone(), model_request_index, turn_id)
            .await
        {
            Ok((response, activity, provider_session_id, reasoning_content)) => Ok((
                response,
                vec![activity],
                provider_session_id,
                false,
                reasoning_content,
            )),
            Err(ModelAttemptError {
                duration,
                error: ModelError::ResumeUnavailable,
            }) if native_resume => {
                let rejected_activity = ModelRequestActivity::new(
                    None,
                    duration,
                    crate::lifecycle::ModelResponseType::ResumeUnavailable,
                );
                let mut replay_request = request.clone();
                replay_request.set_provider_session_id(None);
                let replay_index = model_request_index.saturating_add(1);
                match self
                    .complete_model_attempt(replay_request, replay_index, turn_id)
                    .await
                {
                    Ok((response, replay_activity, provider_session_id, reasoning_content)) => {
                        Ok((
                            response,
                            vec![rejected_activity, replay_activity],
                            provider_session_id,
                            true,
                            reasoning_content,
                        ))
                    }
                    Err(failure) => Err(ResumeFailure::Replay {
                        source: failure.error,
                    }
                    .into_model_error()
                    .into()),
                }
            }
            Err(failure) if native_resume => Err(ResumeFailure::Native {
                source: failure.error,
            }
            .into_model_error()
            .into()),
            Err(failure) => Err(failure.error.into()),
        }
    }

    async fn complete_model_attempt(
        &self,
        request: ModelRequest,
        model_request_index: u64,
        turn_id: Option<LifecycleId>,
    ) -> Result<
        (
            ModelResponse,
            ModelRequestActivity,
            Option<String>,
            Option<String>,
        ),
        ModelAttemptError,
    > {
        let started_at = Instant::now();
        let model_lifecycle =
            self.lifecycle
                .start_model_request(self.model.metadata(), model_request_index, turn_id);
        let operation = self.model.complete(request.clone());
        let completion = match model_lifecycle.as_ref() {
            Some(model_lifecycle) => model_lifecycle.scope(operation).await,
            None => operation.await,
        };
        let (response, completion, provider_session_id, reasoning_content) = match completion {
            Ok(completion) => completion.into_parts(),
            Err(error) => {
                if let Some(model_lifecycle) = model_lifecycle {
                    model_lifecycle.failed(error.error_type(), error.http_status());
                }

                return Err(ModelAttemptError {
                    duration: started_at.elapsed(),
                    error,
                });
            }
        };
        if let Some(output) = response.output()
            && let Err(error) = request.schema().validate_value(output)
        {
            let error = ModelError::from(error);
            if let Some(model_lifecycle) = model_lifecycle {
                model_lifecycle.failed(error.error_type(), error.http_status());
            }

            return Err(ModelAttemptError {
                duration: started_at.elapsed(),
                error,
            });
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

        Ok((response, activity, provider_session_id, reasoning_content))
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

enum ToolExecution<'a> {
    Read(&'a ReadTool, &'a ReadArguments),
    Write(&'a WriteTool, &'a WriteArguments),
}

struct ModelAttemptError {
    duration: Duration,
    error: ModelError,
}

async fn execute_tool(execution: ToolExecution<'_>) -> Result<(String, ToolActivity), TurnError> {
    let started_at = Instant::now();

    match execution {
        ToolExecution::Read(read_tool, arguments) => {
            execute_read_tool(read_tool, arguments, started_at).await
        }
        ToolExecution::Write(write_tool, arguments) => {
            execute_write_tool(write_tool, arguments, started_at).await
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
) -> Result<(String, ToolActivity), TurnError> {
    match write_tool.execute(arguments).await {
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
#[path = "harness_test.rs"]
mod tests;
