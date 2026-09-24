use std::collections::VecDeque;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use serde_json::json;
use tokio::sync::OnceCell;

use crate::cancellation::{ControlledTurn, TurnControl};
use crate::compaction::{self, SessionCheckpoint};
use crate::context::{self, ContextBudget, ContextEstimator, HeuristicContextEstimator};
use crate::effect::Effects;
use crate::engine::Engine;
use crate::file_system::{FileSystem, LocalFileSystem};
use crate::input::{InputBlock, TurnInput};
use crate::lifecycle::{LifecycleEmitter, LifecycleObserver, TurnErrorType, TurnLifecycle};
use crate::model::{Model, ModelError, ModelMessage, ModelRequest, ReasoningEffort};
use crate::model_registry::{ModelRegistration, ModelRegistry, ModelRegistryError};
use crate::policy::ToolPolicy;
use crate::repository::Repository;
use crate::schema_contract::OutputSchema;
use crate::session::{AcquiredTurn, Database, LoadedSession, NewSession, SessionError};
use crate::store::SessionStore;
use crate::tool::Tool;
use crate::turn::{HistoryActivity, TurnError, TurnLimits, TurnOptions, TurnOutcome};
use crate::write_journal::WriteRecord;
use crate::{
    ExecutionIdentity, HostRequest, HostTurnAcquisition, HostTurnRecord, store_coordinator,
};

const DEFAULT_MAX_HISTORY_BYTES: usize = 256 * 1024;

/// Durable, resumable sequence of model turns.
///
/// Owns its runtime resources and captured defaults, so it can outlive the
/// creating harness and move into a spawned task.
pub struct Session {
    checkpoint: Option<SessionCheckpoint>,
    database: Arc<dyn SessionStore>,
    harness: Harness,
    history: SessionHistory,
    id: String,
    model_generation: i64,
    provider_session_id: Option<String>,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl Session {
    /// Selects a registered model for subsequent turns of an idle session.
    ///
    /// Clears native continuation and fences older handles, including when
    /// switching back to a previous registration. Historical provider reasoning
    /// is not portable and causes an explicit rejection. Ordinary completed
    /// messages and tool groups remain intact. Recovery lookup remains usable
    /// on stale handles; new execution requires a fresh handle.
    ///
    /// # Errors
    /// Returns an error for an unknown key, active turn or unsettled effect,
    /// stale handle, unsupported history, or persistence failure. A dropped
    /// waiter can still commit the switch; resume to observe its result.
    pub async fn switch_model(
        &mut self,
        registry: &ModelRegistry,
        key: &str,
    ) -> Result<(), SessionError> {
        let registration = registry.resolve(key)?.clone();
        registration
            .model()
            .validate_schema(&self.schema)
            .map_err(TurnError::from)?;
        let generation = store_coordinator::switch_model(
            Arc::clone(&self.database),
            self.id.clone(),
            self.model_generation,
            registration.clone(),
        )
        .await?;
        self.harness.model = registration.model();
        if self.harness.execution_identity.is_none()
            || self
                .harness
                .model_registration
                .as_ref()
                .is_some_and(|previous| {
                    self.harness.execution_identity.as_ref() == Some(previous.identity())
                })
        {
            self.harness.execution_identity = Some(registration.identity().clone());
        }
        self.harness.model_registration = Some(registration);
        self.model_generation = generation;
        self.provider_session_id = None;

        Ok(())
    }

    /// Loads all command intents and outcomes, including interrupted turns.
    ///
    /// # Errors
    /// Returns a store failure if command records cannot be loaded.
    pub async fn commands(&self) -> Result<Vec<crate::CommandRecord>, SessionError> {
        self.database.load_commands(&self.id).await
    }

    /// Explicitly asserts that a stopped turn's command no longer prevents safe
    /// execution. The host must first stop or otherwise account for its
    /// effects. This never runs a command, erases its unknown outcome, or
    /// rolls back writes. Prefer retained `TurnControl::retry_commands`
    /// while a control is available.
    ///
    /// # Errors
    /// Rejects another session/store/owner or a still-live turn.
    pub async fn reconcile_command(
        &self,
        record: &crate::CommandRecord,
    ) -> Result<(), SessionError> {
        if record.owner().session_id() != self.id
            || record.owner().store_identity() != self.database.identity()
        {
            return Err(SessionError::InvalidData {
                reason: "command belongs to another session".into(),
            });
        }
        self.database
            .reconcile_command(record.owner(), record.id)
            .await?;
        crate::command_settlement::Commands::reconcile(record);

        Ok(())
    }

    /// Generates and publishes a compaction checkpoint covering every
    /// completed turn, replacing covered turns with a structured summary in
    /// later outgoing requests.
    ///
    /// Generation runs the session's current model through the shared engine
    /// with every tool denied, outside store writer transactions, and bounded
    /// by the effective context budget: the most recent uncovered turns that
    /// fit are summarized together with the previous checkpoint. Dropping the
    /// returned future cancels generation before anything is published.
    /// Publication is atomic and only succeeds while the model selection and
    /// existing coverage are still current. On any failure the previous
    /// checkpoint stays usable and projection falls back to bounded recent
    /// history. Canonical messages, host requests, model provenance, and
    /// write journals always remain intact.
    ///
    /// Returns `None` without a model call when no completed turn is
    /// uncovered.
    ///
    /// # Errors
    /// Returns [`SessionError`] for a stale handle, model or validation
    /// failure, a summary violating the bounded checkpoint schema, or a
    /// publication rejected as [`SessionError::CheckpointStale`].
    pub async fn compact(&mut self) -> Result<Option<SessionCheckpoint>, SessionError> {
        let loaded = self.database.load_session(&self.id).await?;
        crate::session_model::check_generation(
            &self.id,
            loaded.model_generation,
            self.model_generation,
        )?;
        let Some(boundary) = loaded.latest_completed_turn else {
            return Ok(None);
        };
        if loaded
            .checkpoint
            .as_ref()
            .is_some_and(|checkpoint| checkpoint.covered_through() >= boundary)
        {
            self.checkpoint = loaded.checkpoint;

            return Ok(None);
        }
        let schema = compaction::summary_schema().map_err(SessionError::Schema)?;
        let options = TurnOptions::new(schema, ToolPolicy::default(), self.harness.limits);
        let input = self.compaction_input(&loaded, &options)?;
        let outcome = self.harness.run_compaction(input, &options).await?;
        let checkpoint = SessionCheckpoint::new(
            boundary,
            self.model_generation,
            loaded.provider,
            loaded.model,
            outcome.output().clone(),
        )?;
        self.database
            .publish_checkpoint(&self.id, &checkpoint)
            .await?;
        self.provider_session_id = None;
        self.checkpoint = Some(checkpoint.clone());

        Ok(Some(checkpoint))
    }

    /// Returns the checkpoint this handle currently projects ahead of recent
    /// turns. Acquisition refreshes it together with replayed history.
    pub fn checkpoint(&self) -> Option<&SessionCheckpoint> {
        self.checkpoint.as_ref()
    }

    /// Renders the bounded generation source: the previous summary plus the
    /// most recent uncovered turns that fit the effective context budget.
    fn compaction_input(
        &self,
        loaded: &LoadedSession,
        options: &TurnOptions,
    ) -> Result<TurnInput, SessionError> {
        let mut turns: Vec<&Vec<ModelMessage>> = loaded.turns.iter().collect();

        loop {
            let source = compaction::render_source(loaded.checkpoint.as_ref(), &turns);
            let input = TurnInput::text(source);
            let Some(budget) = self.harness.context_budget() else {
                return Ok(input);
            };
            match context::admit_mandatory_content(
                self.harness.context_estimator.as_ref(),
                budget,
                Some(compaction::GENERATION_INSTRUCTIONS),
                &input,
                options,
            ) {
                Ok(_) => return Ok(input),
                Err(_) if !turns.is_empty() => {
                    turns.remove(0);
                }
                Err(error) => return Err(error.into()),
            }
        }
    }

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
        self.database.load_writes(&self.id).await
    }

    /// Sends one input and durably records its lifecycle and messages.
    ///
    /// Plain strings remain the text-only path; ordered text/image content
    /// uses [`TurnInput`]. Resolves the stored session schema and captured
    /// permission and tool-limit defaults afresh; earlier explicit overrides
    /// are not inherited.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the model turn or persistence operation
    /// fails.
    pub async fn send(&mut self, input: impl Into<TurnInput>) -> Result<TurnOutcome, SessionError> {
        self.send_with_options(input, self.harness.default_options(self.schema.clone()))
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
        input: impl Into<TurnInput>,
        options: TurnOptions,
    ) -> Result<TurnOutcome, SessionError> {
        self.send_observed(input.into(), options, None, None).await
    }

    /// Prepares a turn with explicit options and a retained cancellation
    /// control.
    ///
    /// The returned future starts execution when polled. Its control observes
    /// persistence and managed-effect settlement even if the future is dropped.
    /// Cancellation may race a successful terminal commit; inspect stored
    /// history after settlement.
    ///
    /// # Errors
    /// The future returns [`SessionError`] for execution or persistence
    /// failure, including [`TurnError::Cancelled`] when cancellation stops
    /// its waiter.
    pub fn send_controlled(
        &mut self,
        input: impl Into<TurnInput>,
        options: TurnOptions,
    ) -> ControlledTurn<'_, SessionError> {
        let input = input.into();
        let mut session = Self {
            checkpoint: self.checkpoint.clone(),
            database: Arc::clone(&self.database),
            harness: self.harness.snapshot(),
            history: SessionHistory::new(self.history.max_bytes),
            id: self.id.clone(),
            model_generation: self.model_generation,
            provider_session_id: None,
            schema: self.schema.clone(),
            system_prompt: self.system_prompt.clone(),
        };

        ControlledTurn::new(move |control| async move {
            session
                .send_observed(input, options, Some(control), None)
                .await
        })
    }

    /// Submits a host request once. Matching completed retries return the
    /// original output and activity; active or stopped retries return typed
    /// errors containing recovery records. New attempts require new IDs.
    ///
    /// # Errors
    /// Returns a conflict for changed effective configuration, an identity
    /// error if the harness is unidentified, or an execution/store error.
    pub async fn submit(
        &mut self,
        host_id: impl Into<String>,
        input: impl Into<TurnInput>,
        options: TurnOptions,
    ) -> Result<TurnOutcome, SessionError> {
        let input = input.into();
        let request = self.host_request(host_id.into(), &input, &options)?;

        self.send_observed(input, options, None, Some(request))
            .await
    }

    /// Prepares a host request with the existing retained cancellation control.
    /// Cancellation can race a terminal commit; use `recover` after settlement.
    ///
    /// # Errors
    /// Returns identity validation errors before execution. The future returns
    /// the same execution, duplicate, and persistence errors as `submit`.
    pub fn submit_controlled(
        &mut self,
        host_id: impl Into<String>,
        input: impl Into<TurnInput>,
        options: TurnOptions,
    ) -> Result<ControlledTurn<'_, SessionError>, SessionError> {
        let input = input.into();
        let request = self.host_request(host_id.into(), &input, &options)?;
        let mut session = Self {
            checkpoint: self.checkpoint.clone(),
            database: Arc::clone(&self.database),
            harness: self.harness.snapshot(),
            history: SessionHistory::new(self.history.max_bytes),
            id: self.id.clone(),
            model_generation: self.model_generation,
            provider_session_id: None,
            schema: self.schema.clone(),
            system_prompt: self.system_prompt.clone(),
        };

        Ok(ControlledTurn::new(move |control| async move {
            session
                .send_observed(input, options, Some(control), Some(request))
                .await
        }))
    }

    /// Loads recorded status, complete output, and known writes without model
    /// or tool execution. This does not establish that pending effects stopped.
    ///
    /// # Errors
    /// Returns an error for invalid identifiers or unavailable/corrupt storage.
    pub async fn recover(&self, host_id: &str) -> Result<Option<HostTurnRecord>, SessionError> {
        crate::recovery::validate_identifier(host_id)?;
        self.database.load_request(&self.id, host_id).await
    }

    fn host_request(
        &self,
        id: String,
        input: &TurnInput,
        options: &TurnOptions,
    ) -> Result<HostRequest, SessionError> {
        let identity = self
            .harness
            .execution_identity
            .as_ref()
            .ok_or(SessionError::ExecutionIdentityRequired)?;
        let metadata = self.harness.model.metadata();
        let repository = self.harness.repository.as_ref().map(|repository| {
            json!({
                "root": repository.root().as_os_str().as_encoded_bytes(),
                "git": repository.git_executable().as_os_str().as_encoded_bytes(),
            })
        });
        // Text-only input keeps the legacy string form so recorded text
        // retries stay recognizable after image support.
        let input_identity = if input.has_images() {
            let blocks: Vec<_> = input
                .blocks()
                .iter()
                .map(|block| match block {
                    InputBlock::Image(image) => json!({
                        "image": {
                            "media_type": image.media_type().as_str(),
                            "sha256": image.content_digest(),
                        },
                    }),
                    InputBlock::Text(text) => json!({ "text": text }),
                })
                .collect();

            json!({"version": 2, "blocks": blocks})
        } else {
            json!(input.joined_text())
        };
        let mut configuration = json!({
            "version": 1,
            "input": input_identity,
            "options": {
                "schema": options.schema().value(),
                "permissions": options.tool_policy(),
                "max_tool_calls": options.limits().max_tool_calls(),
                "comparison": options.comparison_base().map(crate::ComparisonBase::identity),
            },
            "repository": repository,
            "system_prompt": self.system_prompt,
            "reasoning": self.harness.model_reasoning_effort,
            "execution_identity": identity,
            "model": metadata.as_ref().map(|metadata| (metadata.provider(), metadata.model())),
            "max_history_bytes": self.history.max_bytes,
        });
        if let Some(registration) = &self.harness.model_registration {
            let capabilities = registration.capabilities();
            configuration["model_registration"] = json!({
                "identity": registration.identity(),
                "native_continuation": capabilities.native_continuation,
                "tool_calls": capabilities.tool_calls,
            });
            // Only image-bearing requests depend on this capability; adding
            // it to text requests would invalidate recorded text retries.
            if input.has_images() {
                configuration["model_registration"]["image_input"] =
                    json!(capabilities.image_input);
            }
        }
        if let Some(bash) = options.bash() {
            configuration["options"]["bash"] = bash.fingerprint();
        }

        HostRequest::from_configuration(id, configuration)
    }

    async fn send_observed(
        &mut self,
        input: TurnInput,
        options: TurnOptions,
        control: Option<TurnControl>,
        request: Option<HostRequest>,
    ) -> Result<TurnOutcome, SessionError> {
        let started_at = Instant::now();
        let mut turn = if request.is_none() {
            self.harness.lifecycle.start_turn()
        } else {
            None
        };
        let mut result = self
            .send_turn(
                input,
                &options,
                &mut turn,
                control.as_ref(),
                request.as_ref(),
            )
            .await;
        if request.is_none()
            && let Ok(outcome) = &mut result
        {
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
        input: TurnInput,
        options: &TurnOptions,
        turn: &mut Option<TurnLifecycle>,
        control: Option<&TurnControl>,
        request: Option<&HostRequest>,
    ) -> Result<TurnOutcome, SessionError> {
        self.harness.check_input_capability(&input)?;
        // A declared context budget supersedes the byte-based image rejection:
        // images are weighed by the estimator during mandatory admission.
        if self.harness.context_budget().is_none() {
            self.check_image_history_budget(&input)?;
        }
        let available_history_weight = self
            .harness
            .context_budget()
            .map(|budget| {
                context::admit_mandatory_content(
                    self.harness.context_estimator.as_ref(),
                    budget,
                    self.system_prompt.as_deref(),
                    &input,
                    options,
                )
            })
            .transpose()
            .map_err(SessionError::from)?;
        let effects = control.map_or_else(Effects::default, |control| control.effects.clone());
        let _effects = effects.retain();
        let acquisition = self.acquire_turn(&input, options, control, request, &effects);
        let acquired = tokio::select! {
            biased;
            () = cancelled(control) => return Err(TurnError::Cancelled.into()),
            result = acquisition => result?,
        };
        let AcquiredTurn {
            checkpoint,
            mut guard,
            provider_session_id,
            turns,
        } = match acquired {
            HostTurnAcquisition::Acquired(acquired) => acquired,
            HostTurnAcquisition::Recorded(record) => return record.into_outcome(),
        };
        self.checkpoint = checkpoint;
        let host_request = request.is_some();
        if host_request {
            *turn = self.harness.lifecycle.start_turn();
        }
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        self.history.replace(turns);
        self.provider_session_id = provider_session_id;
        let (request, retained_messages, history) =
            self.build_request(input, options, available_history_weight);
        let journal = guard.write_journal();
        let mut engine = self.harness.engine(options);
        engine.effects = effects;
        let result = tokio::select! {
            biased;
            () = cancelled(control) => return Err(TurnError::Cancelled.into()),
            error = guard.ownership_failure() => {
                guard.mark_interrupted();

                return Err(error);
            }
            result = engine.run(request, turn_id, Some(journal)) => result,
        };
        let (mut outcome, mut messages, provider_session_id) = match result {
            Ok(result) => result,
            Err(error) => {
                self.provider_session_id = None;
                let persistence = guard.fail(&error).await;
                if let Err(persistence) = persistence {
                    guard.mark_interrupted();

                    return Err(SessionError::TurnPersistence {
                        turn: error,
                        persistence: Box::new(persistence),
                    });
                }

                return Err(error.into());
            }
        };
        outcome.set_history(history);
        let turn = messages.split_off(retained_messages);
        let persistence = if host_request {
            guard
                .complete_request(&turn[1..], provider_session_id.as_deref(), &outcome)
                .await
        } else {
            guard
                .complete(&turn[1..], provider_session_id.as_deref())
                .await
        };
        if let Err(error) = persistence {
            guard.mark_interrupted();

            return Err(error);
        }
        self.provider_session_id = provider_session_id;
        self.history.push(turn);

        Ok(outcome)
    }

    /// Projects the checkpoint summary and loaded uncovered history into one
    /// request under the effective context budget, keeping the count of
    /// retained messages that precede new output and the observable
    /// projection facts.
    fn build_request(
        &self,
        input: TurnInput,
        options: &TurnOptions,
        available_history_weight: Option<u64>,
    ) -> (ModelRequest, usize, HistoryActivity) {
        let estimator = self.harness.context_estimator.as_ref();
        let checkpoint_message = self
            .checkpoint
            .as_ref()
            .map(SessionCheckpoint::history_message);
        let loaded_turns = self.history.turns().len();
        let (checkpoint_message, mut messages, evicted_turns) = match available_history_weight {
            Some(available_weight) => {
                // The summary is admitted ahead of recent turns; when even it
                // cannot fit, projection falls back to recent history alone.
                let (checkpoint_message, remaining_weight) = match checkpoint_message {
                    Some(message) => {
                        let weight = estimator.message_weight(&message);
                        if weight <= available_weight {
                            (Some(message), available_weight - weight)
                        } else {
                            (None, available_weight)
                        }
                    }
                    None => (None, available_weight),
                };

                let (messages, evicted_turns) =
                    context::select_recent_turns(estimator, self.history.turns(), remaining_weight);

                (checkpoint_message, messages, evicted_turns)
            }
            None => (checkpoint_message, self.history.messages(), 0),
        };
        let history = HistoryActivity::new(
            checkpoint_message.is_some(),
            evicted_turns,
            loaded_turns.saturating_sub(evicted_turns),
        );
        if let Some(checkpoint_message) = checkpoint_message {
            messages.insert(0, checkpoint_message);
        }
        if let Some(system_prompt) = &self.system_prompt {
            messages.insert(0, ModelMessage::System(system_prompt.clone()));
        }
        let retained_messages = messages.len();
        let mut request = ModelRequest::with_history(messages, input, options.schema().clone());
        // A continuation's provider-side conversation can hold turns that the
        // byte replay budget already evicted from loading, so a budgeted
        // registration always replays the projected normalized history. A
        // checkpointed session replays for the same reason: the provider-side
        // conversation retains covered turns instead of the summary.
        request.set_provider_session_id(
            if available_history_weight.is_some() || self.checkpoint.is_some() {
                None
            } else {
                self.provider_session_id.clone()
            },
        );

        (request, retained_messages, history)
    }

    /// Rejects image input that the replay budget would evict immediately,
    /// together with every earlier turn, instead of losing context silently.
    /// Registrations with a declared context budget skip this byte check and
    /// weigh images through mandatory-content admission instead.
    fn check_image_history_budget(&self, input: &TurnInput) -> Result<(), SessionError> {
        let input_bytes = input.retained_bytes();
        if input.has_images() && input_bytes > self.history.max_bytes {
            return Err(SessionError::ImageInputExceedsHistory {
                id: self.id.clone(),
                input_bytes,
                max_history_bytes: self.history.max_bytes,
            });
        }

        Ok(())
    }

    async fn acquire_turn(
        &self,
        input: &TurnInput,
        options: &TurnOptions,
        control: Option<&TurnControl>,
        request: Option<&HostRequest>,
        effects: &Effects,
    ) -> Result<HostTurnAcquisition, SessionError> {
        if let Some(request) = request {
            store_coordinator::acquire_request(
                Arc::clone(&self.database),
                (self.id.clone(), self.model_generation),
                input.clone(),
                options.clone(),
                request.clone(),
                control.map(|control| control.settlement.clone()),
                effects.clone(),
            )
            .await
        } else {
            store_coordinator::acquire(
                Arc::clone(&self.database),
                (self.id.clone(), self.model_generation),
                input.clone(),
                options.clone(),
                control.map(|control| control.settlement.clone()),
                effects.clone(),
            )
            .await
            .map(HostTurnAcquisition::Acquired)
        }
    }
}

/// Builder for one durable session.
///
/// Captures harness configuration immediately and can outlive the harness
/// or move into a spawned task before opening storage.
pub struct SessionBuilder {
    harness: Harness,
    id: String,
    schema: OutputSchema,
    system_prompt: Option<String>,
}

impl SessionBuilder {
    /// Adds a system prompt that is restored with the session.
    #[must_use]
    pub fn system_prompt(mut self, system_prompt: impl Into<String>) -> Self {
        self.system_prompt = Some(system_prompt.into());

        self
    }

    /// Creates the session in the configured store.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when storage is not configured, the identifier
    /// already exists, or the store cannot create the session.
    pub async fn create(self) -> Result<Session, SessionError> {
        let database = self.harness.open_database().await?;
        let config = NewSession::new(self.id, self.schema)
            .with_optional_system_prompt(self.system_prompt)
            .with_registration_identity(
                self.harness
                    .model_registration
                    .as_ref()
                    .map(|registration| registration.identity().clone()),
            );
        database
            .create_session(
                &config,
                self.harness.model.metadata(),
                self.harness.max_history_bytes,
            )
            .await?;
        let history = SessionHistory::new(self.harness.max_history_bytes);

        Ok(Session {
            checkpoint: None,
            database,
            harness: self.harness,
            history,
            id: config.id().to_string(),
            model_generation: 0,
            provider_session_id: None,
            schema: config.schema().clone(),
            system_prompt: config.system_prompt().map(str::to_string),
        })
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

    pub(crate) fn turns(&self) -> &VecDeque<Vec<ModelMessage>> {
        &self.turns
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
/// Session builders and resumed sessions capture this configuration; later
/// reconfiguration affects only newly obtained handles.
pub struct Harness {
    context_estimator: Arc<dyn ContextEstimator>,
    database: Arc<OnceCell<Database>>,
    database_path: Option<PathBuf>,
    execution_identity: Option<ExecutionIdentity>,
    file_system: Arc<dyn FileSystem>,
    lifecycle: LifecycleEmitter,
    limits: TurnLimits,
    max_history_bytes: usize,
    model: Arc<dyn Model>,
    model_reasoning_effort: Option<ReasoningEffort>,
    model_registration: Option<ModelRegistration>,
    policy: ToolPolicy,
    repository: Option<Repository>,
    store: Option<Arc<dyn SessionStore>>,
}

impl Harness {
    /// Creates a deny-by-default harness backed by the local filesystem.
    pub fn new(model: impl Model + 'static) -> Self {
        Self::from_shared_model(Arc::new(model))
    }

    /// Creates a harness using a registered model and its execution identity.
    ///
    /// Captures the registration immediately, so the harness and its sessions
    /// can outlive the registry. Capabilities are host declarations, not tool
    /// permissions. Durable sessions retain the key and revision and require
    /// the same registration on resume. This does not switch a stored model.
    ///
    /// # Errors
    /// Returns [`ModelRegistryError::UnknownKey`] for an unregistered key.
    pub fn from_registry(registry: &ModelRegistry, key: &str) -> Result<Self, ModelRegistryError> {
        let registration = registry.resolve(key)?.clone();
        let mut harness = Self::from_shared_model(registration.model());
        harness.execution_identity = Some(registration.identity().clone());
        harness.model_registration = Some(registration);

        Ok(harness)
    }

    /// Returns the captured registration, or `None` for direct construction.
    pub fn model_registration(&self) -> Option<&ModelRegistration> {
        self.model_registration.as_ref()
    }

    /// Identifies all execution configuration not captured by explicit options.
    /// Required for host-ID submission, including injected models/filesystems.
    /// Hosts must revise it when their behavior or configuration changes.
    /// For registered models, this overrides the host execution assertion but
    /// retains the selected registration in request fingerprints.
    #[must_use]
    pub fn execution_identity(mut self, identity: ExecutionIdentity) -> Self {
        self.execution_identity = Some(identity);

        self
    }

    /// Configures the SQLite database used by durable sessions.
    ///
    /// The first create or resume initializes one shared connection pool and
    /// runs migrations. Reconfiguring the path starts a new lazy pool for
    /// future handles; existing handles retain their shared database.
    #[must_use]
    pub fn database(mut self, path: impl Into<PathBuf>) -> Self {
        self.store = None;
        self.database = Arc::new(OnceCell::new());
        self.database_path = Some(path.into());

        self
    }

    /// Selects a host-provided session store, replacing SQLite configuration.
    ///
    /// Builders and sessions retain their selected store when this harness is
    /// reconfigured. `run_once` never accesses the store. Implementations must
    /// satisfy the atomic lifecycle contract of [`SessionStore`].
    #[must_use]
    pub fn store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.store = Some(store);
        self.database = Arc::new(OnceCell::new());
        self.database_path = None;

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

    /// Replaces the approximate content estimator used by context projection.
    ///
    /// Projection activates when the effective registration declares a
    /// [`ContextBudget`] in its capabilities: requests keep the most recent
    /// complete turns that fit the remaining weight, and mandatory content
    /// that cannot fit fails with [`TurnError::ContextBudgetExceeded`] before
    /// acquisition. Estimates are deterministic approximations, never exact
    /// provider token counts.
    #[must_use]
    pub fn context_estimator(mut self, estimator: impl ContextEstimator + 'static) -> Self {
        self.context_estimator = Arc::new(estimator);

        self
    }

    /// Runs one input without creating durable session history.
    ///
    /// Plain strings remain the text-only path; ordered text/image content
    /// uses [`TurnInput`].
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when the model fails, requests a denied tool,
    /// exceeds the call limit, or a requested repository operation fails.
    pub async fn run_once(
        &self,
        input: impl Into<TurnInput>,
        schema: OutputSchema,
    ) -> Result<TurnOutcome, TurnError> {
        self.run_once_with_options(input, self.default_options(schema))
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
        input: impl Into<TurnInput>,
        options: TurnOptions,
    ) -> Result<TurnOutcome, TurnError> {
        self.run_once_observed(input.into(), options, Effects::default())
            .await
    }

    /// Prepares a storage-free turn with separately retained cancellation.
    ///
    /// Dropping the returned future requests cancellation. Observe persistence
    /// settlement and managed filesystem-effect settlement separately through
    /// the retained control. Neither waits for remote provider work.
    ///
    /// # Errors
    /// The future returns [`TurnError`], including [`TurnError::Cancelled`].
    pub fn run_once_controlled(
        &self,
        input: impl Into<TurnInput>,
        options: TurnOptions,
    ) -> ControlledTurn<'_, TurnError> {
        let harness = self.snapshot();
        let input = input.into();

        ControlledTurn::new(move |control| async move {
            tokio::select! {
                biased;
                () = control.cancelled() => Err(TurnError::Cancelled),
                result = harness.run_once_observed(input, options, control.effects.clone()) => result,
            }
        })
    }

    /// Builds a durable session with `schema` as its default output contract.
    ///
    /// Explicit turn options may select a different schema without changing
    /// the stored default. Captures current configuration without opening
    /// the database.
    pub fn session(&self, id: impl Into<String>, schema: OutputSchema) -> SessionBuilder {
        SessionBuilder {
            harness: self.snapshot(),
            id: id.into(),
            schema,
            system_prompt: None,
        }
    }

    /// Resumes a durable session and restores its bounded completed history.
    ///
    /// Captures current harness defaults while retaining the stored schema,
    /// system prompt, and history budget.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when storage is not configured, the session is
    /// missing, its model or registration differs, or the store cannot load it.
    pub async fn resume(&self, id: &str) -> Result<Session, SessionError> {
        let database = self.open_database().await?;
        let loaded = database.load_session(id).await?;
        self.validate_session_model(id, &loaded)?;
        let mut history = SessionHistory::new(loaded.max_history_bytes);
        for turn in loaded.turns {
            history.push(turn);
        }

        Ok(Session {
            checkpoint: loaded.checkpoint,
            database,
            harness: self.snapshot(),
            history,
            id: id.to_string(),
            model_generation: loaded.model_generation,
            provider_session_id: loaded.provider_session_id,
            schema: loaded.schema,
            system_prompt: loaded.system_prompt,
        })
    }

    fn from_shared_model(model: Arc<dyn Model>) -> Self {
        Self {
            context_estimator: Arc::new(HeuristicContextEstimator),
            database: Arc::new(OnceCell::new()),
            database_path: None,
            execution_identity: None,
            file_system: Arc::new(LocalFileSystem),
            lifecycle: LifecycleEmitter::default(),
            limits: TurnLimits::default(),
            max_history_bytes: DEFAULT_MAX_HISTORY_BYTES,
            model,
            model_reasoning_effort: None,
            model_registration: None,
            policy: ToolPolicy::default(),
            repository: None,
            store: None,
        }
    }

    async fn run_once_observed(
        &self,
        input: TurnInput,
        options: TurnOptions,
        effects: Effects,
    ) -> Result<TurnOutcome, TurnError> {
        self.check_input_capability(&input)?;
        if let Some(budget) = self.context_budget() {
            context::admit_mandatory_content(
                self.context_estimator.as_ref(),
                budget,
                None,
                &input,
                &options,
            )?;
        }
        let _effects = effects.retain();
        let request = ModelRequest::new(input, options.schema().clone());
        let turn = self.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let mut engine = self.engine(&options);
        engine.effects = effects;
        let result = engine.run(request, turn_id, None).await;

        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(error) => turn.failed(error.error_type()),
            }
        }

        result.map(|(outcome, _, _)| outcome)
    }

    /// Runs one bounded, tool-free summarization turn for checkpoint
    /// generation, owned by the caller's lifecycle like an ephemeral turn.
    async fn run_compaction(
        &self,
        input: TurnInput,
        options: &TurnOptions,
    ) -> Result<TurnOutcome, TurnError> {
        let request = ModelRequest::with_history(
            vec![ModelMessage::System(
                compaction::GENERATION_INSTRUCTIONS.to_string(),
            )],
            input,
            options.schema().clone(),
        );
        let turn = self.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let engine = self.engine(options);
        let result = engine.run(request, turn_id, None).await;
        if let Some(turn) = turn {
            match &result {
                Ok(_) => turn.completed(),
                Err(error) => turn.failed(error.error_type()),
            }
        }

        result.map(|(outcome, _, _)| outcome)
    }

    fn snapshot(&self) -> Self {
        Self {
            context_estimator: Arc::clone(&self.context_estimator),
            database: Arc::clone(&self.database),
            database_path: self.database_path.clone(),
            execution_identity: self.execution_identity.clone(),
            file_system: Arc::clone(&self.file_system),
            lifecycle: self.lifecycle.clone(),
            limits: self.limits,
            max_history_bytes: self.max_history_bytes,
            model: Arc::clone(&self.model),
            model_reasoning_effort: self.model_reasoning_effort,
            model_registration: self.model_registration.clone(),
            policy: self.policy,
            repository: self.repository.clone(),
            store: self.store.clone(),
        }
    }

    async fn open_database(&self) -> Result<Arc<dyn SessionStore>, SessionError> {
        if let Some(store) = &self.store {
            return Ok(Arc::clone(store));
        }
        let path = self
            .database_path
            .as_deref()
            .ok_or(SessionError::StorageRequired)?;

        self.database
            .get_or_try_init(|| Database::open(path))
            .await
            .map(|database| Arc::new(database.clone()) as Arc<dyn SessionStore>)
    }

    fn validate_session_model(&self, id: &str, loaded: &LoadedSession) -> Result<(), SessionError> {
        if loaded.registration_identity.as_ref()
            != self
                .model_registration
                .as_ref()
                .map(ModelRegistration::identity)
        {
            return Err(SessionError::RegistrationMismatch { id: id.to_string() });
        }
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

    fn context_budget(&self) -> Option<ContextBudget> {
        self.model_registration
            .as_ref()
            .and_then(|registration| registration.capabilities().context_budget)
    }

    fn check_input_capability(&self, input: &TurnInput) -> Result<(), TurnError> {
        self.model.validate_input(input)?;
        if input.has_images()
            && let Some(registration) = &self.model_registration
            && !registration.capabilities().image_input
        {
            return Err(TurnError::Model(ModelError::UnsupportedImageInput {
                reason: "registered model does not declare image input support".to_string(),
            }));
        }

        Ok(())
    }

    fn default_options(&self, schema: OutputSchema) -> TurnOptions {
        TurnOptions::new(schema, self.policy, self.limits)
    }

    fn engine<'a>(&'a self, options: &'a TurnOptions) -> Engine<'a> {
        Engine {
            context_budget: self.context_budget(),
            context_estimator: self.context_estimator.as_ref(),
            effects: Effects::default(),
            file_system: &self.file_system,
            lifecycle: &self.lifecycle,
            model: &self.model,
            model_reasoning_effort: self.model_reasoning_effort,
            options,
            repository: self.repository.as_ref(),
        }
    }
}

async fn cancelled(control: Option<&TurnControl>) {
    match control {
        Some(control) => control.cancelled().await,
        None => std::future::pending().await,
    }
}

#[cfg(test)]
#[path = "harness_test.rs"]
mod tests;
