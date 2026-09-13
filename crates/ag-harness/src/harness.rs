use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use tokio::sync::OnceCell;

use crate::engine::Engine;
use crate::file_system::{FileSystem, LocalFileSystem};
use crate::lifecycle::{
    LifecycleEmitter, LifecycleId, LifecycleObserver, TurnErrorType, TurnLifecycle,
};
use crate::model::{Model, ModelMessage, ModelRequest, ReasoningEffort};
use crate::policy::ToolPolicy;
use crate::repository::Repository;
use crate::schema_contract::OutputSchema;
use crate::session::{AcquiredTurn, Database, LoadedSession, NewSession, SessionError};
use crate::tool::Tool;
use crate::turn::{TurnError, TurnLimits, TurnOptions, TurnOutcome};
use crate::write_journal::{WriteRecord, WriteRecordRow};

const DEFAULT_MAX_HISTORY_BYTES: usize = 256 * 1024;

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

    /// Returns durable write intents and recorded outcomes, including failed
    /// turns.
    ///
    /// Records survive history eviction and reopening. This reads stored
    /// outcomes without inspecting or modifying the repository's current
    /// files.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if journal loading fails.
    pub async fn writes(&self) -> Result<Vec<WriteRecord>, SessionError> {
        WriteRecordRow::load(self.database.pool(), &self.id).await
    }

    /// Sends one prompt and durably records its lifecycle and messages.
    ///
    /// Resolves the stored session schema and current harness permission and
    /// tool-limit defaults afresh; earlier explicit overrides are not
    /// inherited.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the model turn or persistence operation
    /// fails.
    pub async fn send(&mut self, prompt: impl Into<String>) -> Result<TurnOutcome, SessionError> {
        self.send_with_options(prompt, self.harness.default_options(self.schema.clone()))
            .await
    }

    /// Sends a durable turn using exactly these options, without changing
    /// defaults.
    ///
    /// Schema, permission, or comparison changes discard native continuation
    /// and replay completed history. Permission downgrades retain earlier
    /// tool results. Options are persisted before execution; completion is
    /// reported only after the resulting messages are committed.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] if execution or persistence fails.
    pub async fn send_with_options(
        &mut self,
        prompt: impl Into<String>,
        options: TurnOptions,
    ) -> Result<TurnOutcome, SessionError> {
        let started_at = Instant::now();
        let turn = self.harness.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let mut result = self.send_turn(prompt.into(), &options, turn_id).await;
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
        options: &TurnOptions,
        turn_id: Option<LifecycleId>,
    ) -> Result<TurnOutcome, SessionError> {
        let AcquiredTurn {
            mut guard,
            provider_session_id,
            turn_position,
            turns,
        } = self.database.begin_turn(&self.id, &prompt, options).await?;
        self.history.replace(turns);
        self.provider_session_id = provider_session_id;
        let mut messages = self.history.messages();
        if let Some(system_prompt) = &self.system_prompt {
            messages.insert(0, ModelMessage::System(system_prompt.clone()));
        }
        let retained_messages = messages.len();
        let mut request = ModelRequest::with_history(messages, prompt, options.schema().clone());
        request.set_provider_session_id(self.provider_session_id.clone());
        let journal = guard.write_journal();
        let engine = self.harness.engine(options);
        let result = tokio::select! {
            biased;
            error = guard.ownership_failure() => {
                guard.mark_interrupted();

                return Err(error);
            }
            result = engine.run(request, turn_id, Some(journal)) => result,
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
    limits: TurnLimits,
    max_history_bytes: usize,
    model: Arc<dyn Model>,
    model_reasoning_effort: Option<ReasoningEffort>,
    policy: ToolPolicy,
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
            limits: TurnLimits::default(),
            max_history_bytes: DEFAULT_MAX_HISTORY_BYTES,
            model: Arc::new(model),
            model_reasoning_effort: None,
            policy: ToolPolicy::default(),
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

    /// Allows one built-in tool by default for `run_once` and `Session::send`.
    ///
    /// Explicit turn options replace this default policy completely.
    #[must_use]
    pub fn allow(mut self, tool: Tool) -> Self {
        self.policy = self.policy.allow(tool);

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
        self.limits = TurnLimits::new(max_tool_calls);

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
        self.run_once_with_options(prompt, self.default_options(schema))
            .await
    }

    /// Runs an ephemeral turn with a complete options snapshot.
    ///
    /// Options replace harness schema, permission, and tool-limit defaults;
    /// an empty policy denies all tools. This never opens a database.
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when model execution, validation, or tools fail.
    pub async fn run_once_with_options(
        &self,
        prompt: impl Into<String>,
        options: TurnOptions,
    ) -> Result<TurnOutcome, TurnError> {
        let request = ModelRequest::new(prompt, options.schema().clone());
        let turn = self.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let result = self.engine(&options).run(request, turn_id, None).await;

        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(error) => turn.failed(error.error_type()),
            }
        }

        result.map(|(outcome, _, _)| outcome)
    }

    /// Builds a durable session with `schema` as its default output contract.
    ///
    /// Explicit turn options may select a different schema without changing
    /// the stored default.
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

    fn default_options(&self, schema: OutputSchema) -> TurnOptions {
        TurnOptions::new(schema, self.policy, self.limits)
    }

    fn engine<'a>(&'a self, options: &'a TurnOptions) -> Engine<'a> {
        Engine {
            file_system: &self.file_system,
            lifecycle: &self.lifecycle,
            model: &self.model,
            model_reasoning_effort: self.model_reasoning_effort,
            options,
            repository: self.repository.as_ref(),
        }
    }
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;
