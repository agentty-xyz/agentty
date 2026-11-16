//! `Harness` entry point for one-shot model turns and durable sessions, with
//! session builders, turn handles, and history.

use std::future::{Future, IntoFuture};
#[cfg(not(feature = "sqlite"))]
use std::future::{Ready, ready};
use std::hash::{BuildHasher, RandomState};
use std::num::NonZeroUsize;
#[cfg(feature = "sqlite")]
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use serde_json::json;
use sha2::{Digest, Sha256};
use thiserror::Error;
#[cfg(feature = "sqlite")]
use tokio::sync::OnceCell;
use tokio::sync::Semaphore;

use crate::budget::{BudgetLedger, Unlimited};
use crate::calibration::ContextCalibration;
use crate::cancellation::{ControlledTurn, TurnControl};
use crate::catalog::{CatalogError, ModelCatalog, ModelId, ModelSelector, Pricing, Role};
use crate::compaction::{self, CompactionSource, MAX_SUMMARY_BYTES, SessionCheckpoint};
use crate::context::{
    self, ContextBudget, ContextBudgetError, ContextEstimator, ContextSummaryPolicy,
    HeuristicContextEstimator, HistoryLimit, HistoryProjection, HistoryTurn, ReplayedHistory,
};
use crate::effect::Effects;
use crate::engine::{Engine, EngineFailure, FinishedTurn};
use crate::error::HarnessError;
use crate::event::{RunEventKind, SessionId, TurnIndex};
use crate::file_system::{FileSystem, LocalFileSystem};
use crate::input::{InputBlock, TurnInput};
use crate::lifecycle::{LifecycleEmitter, LifecycleObserver, TurnErrorType, TurnLifecycle};
use crate::memory_store::MemoryStore;
use crate::model::{Model, ModelError, ModelMessage, ModelRequest, ReasoningEffort};
use crate::model_profile::ModelProfile;
use crate::model_registry::{ModelRegistration, ModelRegistry, ModelRegistryError};
use crate::policy::ToolPolicy;
use crate::recovery::{ExecutionIdentity, HostRequest, HostTurnAcquisition, HostTurnRecord};
use crate::repository::Repository;
use crate::request_option::OutputMode;
use crate::run::{Run, RunJournal, RunRequest};
use crate::schema_contract::OutputSchema;
use crate::session::{LoadedSession, NewSession, SessionError};
use crate::session_model::ReplayTarget;
#[cfg(feature = "sqlite")]
use crate::sqlite_store::Database;
use crate::store::{AcquiredTurn, SessionStore};
use crate::tool::Tool;
use crate::transcript::{TurnRange, TurnRecord};
use crate::turn::{HistoryActivity, TurnError, TurnOptions, TurnOutcome};
use crate::usage::{TurnUsage, UsageTotals};
use crate::write_journal::WriteRecord;
use crate::{reservation, session_model};

/// Retained history bytes of a new session whose model declares no context
/// window, or whose estimator cannot convert weight into bytes.
const DEFAULT_MAX_HISTORY_BYTES: usize = 256 * 1024;
/// Former default per-turn tool-call limit, still hashed into host-request
/// fingerprints so requests recorded before its removal stay recognizable.
const LEGACY_FINGERPRINT_MAX_TOOL_CALLS: usize = 8;

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
    output: OutputMode,
    system_prompt: Option<String>,
}

impl Session {
    /// Selects a registered model for subsequent turns of an idle session.
    ///
    /// Adopts the registration's context budget and estimator, replacing any
    /// estimator set with [`Harness::context_estimator`], discards the
    /// in-memory calibration of the previous model's estimates, and fences
    /// older handles, including when switching back to a previous
    /// registration. Stored history stays as each model wrote it: when a
    /// later request goes to a model of another reasoning family (all
    /// `OpenAI` Responses models form one family, each OpenAI-compatible
    /// server model its own, and every other provider one), turns an earlier
    /// family produced replay their reasoning as short notes, and switching
    /// back replays that reasoning natively again. Every switch, including
    /// one within a provider, restarts the history projection, so the new
    /// model's first request shares no cached prompt prefix with the old
    /// one.
    /// A registered model has no prices, so later runs report no
    /// [`crate::Cost`] and their turns count as unpriced, no catalog
    /// concurrency cap, so later requests no longer take its permits, and
    /// no cataloged maximum output for budget estimates.
    /// Ordinary completed messages and tool groups remain intact. Recovery
    /// lookup remains usable on stale handles; new execution requires a fresh
    /// handle.
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
        self.output
            .check_model(registration.model().as_ref())
            .map_err(TurnError::from)?;
        let generation = reservation::switch_model(
            Arc::clone(&self.database),
            self.id.clone(),
            self.model_generation,
            registration.clone(),
        )
        .await?;
        let config = self.harness.config_mut();
        config.concurrency = None;
        config.max_output = None;
        config.model = registration.model();
        config.model_id = None;
        config.pricing = None;
        config.context_budget = registration.context_budget();
        config.context_calibration = ContextCalibration::default();
        config.context_estimator = Arc::new(registration.capabilities().profile.estimator());
        if config.execution_identity.is_none()
            || config.model_registration.as_ref().is_some_and(|previous| {
                config.execution_identity.as_ref() == Some(previous.identity())
            })
        {
            config.execution_identity = Some(registration.identity().clone());
        }
        config.model_registration = Some(registration);
        self.model_generation = generation;

        Ok(())
    }

    /// Loads all command intents and outcomes, including interrupted turns.
    ///
    /// # Errors
    /// Returns a store failure if command records cannot be loaded.
    pub async fn commands(&self) -> Result<Vec<crate::bash::CommandRecord>, SessionError> {
        self.database.load_commands(&self.id).await
    }

    /// Explicitly asserts that a stopped turn's command no longer prevents safe
    /// execution. The host must first stop or otherwise account for its
    /// effects. This never runs a command, erases its unknown outcome, or
    /// rolls back writes. Prefer retained `TurnControl::retry_settlement`
    /// while a control is available.
    ///
    /// # Errors
    /// Rejects another session/store/owner or a still-live turn.
    pub async fn reconcile_command(
        &self,
        record: &crate::bash::CommandRecord,
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

    /// Generates and publishes a compaction checkpoint covering the oldest
    /// uncovered finished turns, replacing covered turns with a structured
    /// summary in later outgoing requests.
    ///
    /// Generation runs the session's current model through the shared engine
    /// with every tool denied, outside store writer transactions, and bounded
    /// by the effective context budget: the previous checkpoint's summary and
    /// the oldest uncovered turns that fit render, as JSON, into its source.
    /// The checkpoint covers exactly the rendered turns, so turns left out for
    /// size stay uncovered until a later call summarizes them. Dropping the
    /// returned future cancels generation before anything is published.
    /// Publication is atomic and only succeeds while the model selection and
    /// existing coverage are still current. On any failure the previous
    /// checkpoint stays usable and projection falls back to bounded recent
    /// history. Canonical messages, host requests, model provenance, and
    /// write journals always remain intact.
    ///
    /// Returns `None` without a model call when no finished turn is
    /// uncovered.
    ///
    /// # Errors
    /// Returns [`SessionError`] for a stale handle, model or validation
    /// failure, a summary violating the bounded checkpoint schema or no
    /// smaller than its source, or a publication rejected as
    /// [`SessionError::CheckpointStale`].
    pub async fn compact(&mut self) -> Result<Option<SessionCheckpoint>, SessionError> {
        let loaded = self.database.load_session(&self.id).await?;
        crate::session_model::check_generation(
            &self.id,
            loaded.model_generation,
            self.model_generation,
        )?;
        let source = self
            .database
            .load_compaction_source(&self.id, loaded.max_history_bytes)
            .await?;
        if source.turns.is_empty() {
            self.checkpoint = source.checkpoint;

            return Ok(None);
        }
        let schema = compaction::summary_schema().map_err(SessionError::Schema)?;
        let options = TurnOptions::new(schema, ToolPolicy::default());
        let (input, covered_through, source_bytes) = self.compaction_input(&source, &options)?;
        let outcome = self.harness.run_compaction(input, &options).await?;
        compaction::accept_summary(outcome.output(), MAX_SUMMARY_BYTES, source_bytes)?;
        let checkpoint = SessionCheckpoint::new(
            covered_through,
            self.model_generation,
            loaded.provider,
            loaded.model,
            outcome.output().clone(),
        )?;
        self.database
            .publish_checkpoint(&self.id, &checkpoint)
            .await?;
        self.checkpoint = Some(checkpoint.clone());

        Ok(Some(checkpoint))
    }

    /// Returns the checkpoint this handle currently projects ahead of recent
    /// turns. Acquisition refreshes it together with replayed history.
    pub fn checkpoint(&self) -> Option<&SessionCheckpoint> {
        self.checkpoint.as_ref()
    }

    /// Renders the bounded generation source: the previous summary plus the
    /// oldest uncovered turns that fit the effective context budget, dropping
    /// the newest while the source does not fit. Returns the source with the
    /// position of the newest rendered turn, which the checkpoint covers
    /// through, and the source's rendered bytes.
    fn compaction_input(
        &self,
        source: &CompactionSource,
        options: &TurnOptions,
    ) -> Result<(TurnInput, i64, usize), SessionError> {
        let previous = source.checkpoint.as_ref().map(SessionCheckpoint::summary);
        let mut turns: Vec<(i64, Vec<ModelMessage>)> = source
            .turns
            .iter()
            .map(|(position, turn)| (*position, turn.replay()))
            .collect();

        loop {
            let rendered = compaction::render_source(previous, &turns);
            let source_bytes = rendered.len();
            let input = TurnInput::text(rendered);
            match self.harness.admit_mandatory_content(
                Some(compaction::GENERATION_INSTRUCTIONS),
                &input,
                options,
            ) {
                Ok(_) => {
                    let covered_through = turns.last().map_or(-1, |(position, _)| *position);

                    return Ok((input, covered_through, source_bytes));
                }
                Err(_) if turns.len() > 1 => {
                    turns.pop();
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

    /// Returns the settled turns `range` selects, oldest first, with their
    /// input, instructions, items, and final output.
    ///
    /// Reads every selected turn in full, including turns the history
    /// budget or a compaction checkpoint keeps out of later requests, and
    /// never a turn still running. Loads only the selected turns.
    ///
    /// # Errors
    ///
    /// Returns an [`crate::ErrorKind::NotFound`] failure for a missing
    /// session, and an [`crate::ErrorKind::Store`] failure when the store
    /// cannot load the turns or does not support transcripts.
    pub async fn transcript(&self, range: TurnRange) -> Result<Vec<TurnRecord>, HarnessError> {
        let turns = self.database.load_transcript(&self.id, &range).await?;

        Ok(turns.into_iter().map(TurnRecord::from_stored).collect())
    }

    /// Returns the tokens and cost summed over every settled turn: completed,
    /// failed, including turns stopped by a run limit, and interrupted ones,
    /// never a turn still running.
    ///
    /// Each turn's usage is recorded when the session admits it and as its
    /// model requests complete, so a stopped turn keeps the usage of its
    /// completed requests; a failed usage write stops the turn with
    /// [`TurnError::UsageJournal`]. Turns recorded before usage was stored
    /// take their unpriced usage from a stored outcome, kept for turns run
    /// with a host request ID, and otherwise count as
    /// [`UsageTotals::unknown_turns`].
    ///
    /// # Errors
    ///
    /// Returns an [`crate::ErrorKind::NotFound`] failure for a missing
    /// session, and an [`crate::ErrorKind::Store`] failure when the store
    /// cannot load the usage or does not support it.
    pub async fn usage(&self) -> Result<UsageTotals, HarnessError> {
        let turns = self.database.load_usage(&self.id).await?;

        Ok(UsageTotals::from_turns(&turns))
    }

    /// Runs one turn with the session defaults and records it durably.
    ///
    /// Plain strings are text input; use [`TurnInput`] for ordered text and
    /// images. Shorthand for `self.turn(input).await`; use [`Self::turn`] for
    /// per-turn options, host IDs, or cancellation.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError`] when the model turn or persistence operation
    /// fails.
    pub async fn send(&mut self, input: impl Into<TurnInput>) -> Result<TurnOutcome, SessionError> {
        self.turn(input).await
    }

    /// Configures one durable turn. Await the result to run it, or call
    /// [`SessionTurn::start`] to keep a cancellation control.
    ///
    /// Without [`SessionTurn::options`] the turn resolves the stored session
    /// schema and the captured permission defaults afresh; earlier explicit
    /// overrides are not inherited.
    pub fn turn(&mut self, input: impl Into<TurnInput>) -> SessionTurn<'_> {
        SessionTurn {
            host_id: None,
            input: input.into(),
            options: None,
            session: self,
        }
    }

    /// Starts one durable run of `request`; await the returned [`Run`] for
    /// its [`crate::RunOutcome`].
    ///
    /// Unset request options use the stored session output mode and the
    /// captured default tool policy; request options never change the
    /// stored ones.
    /// The run executes on its own task with a copy of this handle, like
    /// [`SessionTurn::start`], and owns its cancellation. Its turn joins the
    /// session history that later runs and turns replay.
    pub fn run(&self, request: RunRequest) -> Run {
        let budget = request.ledger();
        let turn = request.into_turn(Some(&self.output), self.harness.inner.policy);
        let journal = RunJournal::new(
            Arc::clone(&self.database),
            self.id.clone(),
            &turn,
            self.harness.inner.pricing,
        );
        let mut session = self.detached();
        if let Some(budget) = budget {
            session.harness.config_mut().budget = budget;
        }

        Run::new(
            ControlledTurn::new(move |control| async move {
                let (input, options) = turn?;

                Ok(session
                    .send_observed(input, options, Some(control), None)
                    .await?)
            }),
            journal,
        )
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

    /// Copies this handle for a controlled turn that runs on its own task.
    fn detached(&self) -> Self {
        Self {
            checkpoint: self.checkpoint.clone(),
            database: Arc::clone(&self.database),
            harness: self.harness.clone(),
            history: SessionHistory::new(self.history.max_bytes),
            id: self.id.clone(),
            model_generation: self.model_generation,
            output: self.output.clone(),
            system_prompt: self.system_prompt.clone(),
        }
    }

    fn host_request(
        &self,
        id: String,
        input: &TurnInput,
        options: &TurnOptions,
    ) -> Result<HostRequest, SessionError> {
        let config = &self.harness.inner;
        let identity = config
            .execution_identity
            .as_ref()
            .ok_or(SessionError::ExecutionIdentityRequired)?;
        let metadata = config.model.metadata();
        let repository = config.repository.as_ref().map(|repository| {
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
        // Removed settings keep their former defaults so requests recorded
        // before their removal stay recognizable.
        let mut configuration = json!({
            "version": 1,
            "input": input_identity,
            "options": {
                "schema": options.schema().map(OutputSchema::value),
                "permissions": options.tool_policy(),
                "max_tool_calls": LEGACY_FINGERPRINT_MAX_TOOL_CALLS,
                "comparison": options.comparison_base().map(crate::ComparisonBase::identity),
            },
            "repository": repository,
            "system_prompt": self.system_prompt,
            "reasoning": config.model_reasoning_effort,
            "execution_identity": identity,
            "model": metadata.as_ref().map(|metadata| (metadata.provider(), metadata.model())),
            "max_history_bytes": self.history.max_bytes,
        });
        if let Some(registration) = &config.model_registration {
            let capabilities = registration.capabilities();
            configuration["model_registration"] = json!({
                "identity": registration.identity(),
                "native_continuation": false,
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
        // Later options join only when set, so earlier recorded requests
        // stay recognizable.
        if let Some(instructions) = options.instructions() {
            configuration["options"]["instructions"] = json!(instructions);
        }
        if !options.generation().is_default() {
            configuration["options"]["generation"] = json!(options.generation());
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
            self.harness.inner.lifecycle.start_turn()
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
        // Recorded requests return before checks that only gate new
        // execution; the context budget is not part of their fingerprint.
        if let Some(request) = request
            && let Some(record) =
                reservation::recorded_request(self.database.as_ref(), &self.id, request).await?
        {
            return record.into_outcome();
        }
        self.harness.check_input_capability(&input)?;
        self.harness.check_output_capability(options)?;
        let available_history_weight = self
            .harness
            .admit_mandatory_content(self.system_prompt.as_deref(), &input, options)
            .map_err(SessionError::from)?;
        self.check_image_history_budget(&input)?;
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
            projection,
            turns,
        } = match acquired {
            HostTurnAcquisition::Acquired(acquired) => acquired,
            HostTurnAcquisition::Recorded(record) => return record.into_outcome(),
        };
        self.checkpoint = checkpoint;
        if let Some(control) = control {
            control.record_turn_position(guard.owner().turn_position);
        }
        let host_request = request.is_some();
        if host_request {
            *turn = self.harness.inner.lifecycle.start_turn();
        }
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        self.history.replace(&turns, projection);
        let (request, retained_messages, history, replayed) =
            self.build_request(input, options, available_history_weight);
        guard
            .record_admitted_usage(TurnUsage::new(self.harness.inner.pricing))
            .await?;
        let journal = guard.write_journal();
        let engine = self.harness.controlled_engine(options, control, effects);
        let started = self.started_event(guard.owner().turn_position);
        let result = tokio::select! {
            biased;
            () = cancelled(control) => return Err(TurnError::Cancelled.into()),
            error = guard.ownership_failure() => {
                guard.mark_interrupted();

                return Err(error);
            }
            result = async {
                engine.publish(started).await;

                engine.run(request, turn_id, Some(journal)).await
            } => result,
        };
        let FinishedTurn {
            history: sent_history,
            mut messages,
            mut outcome,
        } = match result {
            Ok(result) => result,
            Err(EngineFailure {
                error,
                interrupted,
                report,
            }) => {
                if let (Some(control), Some(report)) = (control, report.as_deref()) {
                    control.record_report(report);
                }
                if let Some(turn) = turn.as_mut() {
                    turn.set_failure_report(report);
                }

                return Err(guard.settle_failure(error, interrupted).await);
            }
        };
        outcome.set_history(history);
        let turn = messages.split_off(retained_messages);
        // Earlier exchanges were appended as they finished; the final output
        // commits atomically with completion, with the projection the next
        // turn continues.
        let output = turn.last().map_or(&[][..], std::slice::from_ref);
        let projection = replayed.after_turn(&sent_history);
        let persistence = if host_request {
            guard.complete_request(output, &outcome, projection).await
        } else {
            guard.complete(output, projection).await
        };
        if let Err(error) = persistence {
            guard.mark_interrupted();
            if let Some(control) = control {
                control.record_report(outcome.report());
            }

            return Err(error);
        }

        Ok(outcome)
    }

    /// Describes the admitted turn at `turn_position` for a streamed run.
    fn started_event(&self, turn_position: i64) -> RunEventKind {
        RunEventKind::Started {
            model: self.harness.inner.model_id.clone(),
            session: SessionId::new(self.id.clone()),
            turn: TurnIndex::from_position(turn_position),
        }
    }

    /// Projects the checkpoint summary and loaded uncovered history into one
    /// request under the effective context budget, keeping the count of
    /// retained messages that precede new output, the observable projection
    /// facts, and the replayed turns with the projection they follow.
    fn build_request(
        &self,
        input: TurnInput,
        options: &TurnOptions,
        available_history_weight: u64,
    ) -> (ModelRequest, usize, HistoryActivity, ReplayedHistory) {
        let estimator = self.harness.inner.context_estimator.as_ref();
        let checkpoint_message = self
            .checkpoint
            .as_ref()
            .map(SessionCheckpoint::history_message);
        let loaded_turns = self.history.turns().len();
        // The summary is admitted ahead of recent turns; when even it cannot
        // fit, projection falls back to recent history alone.
        let (checkpoint_message, remaining_weight) = match checkpoint_message {
            Some(message) => {
                let weight = estimator.message_weight(&message);
                if weight <= available_history_weight {
                    (Some(message), available_history_weight - weight)
                } else {
                    (None, available_history_weight)
                }
            }
            None => (None, available_history_weight),
        };
        let (turns, projection) = context::project_history(
            self.history.turns(),
            self.history.projection,
            &HistoryLimit::weight(remaining_weight, estimator),
        );
        let evicted_turns = loaded_turns.saturating_sub(turns.len());
        let history =
            HistoryActivity::new(checkpoint_message.is_some(), evicted_turns, turns.len());
        let mut messages: Vec<ModelMessage> = self
            .system_prompt
            .iter()
            .map(|system_prompt| ModelMessage::System(system_prompt.clone()))
            .chain(checkpoint_message)
            .collect();
        // Turns another reasoning family produced replay their reasoning as
        // notes; stored turns stay native.
        let model = &self.harness.inner.model;
        let mut target = ReplayTarget::new(model.metadata().as_ref(), self.model_generation);
        // Only projected turns depend on how the model replays reasoning.
        if !turns.iter().all(|(_, turn)| target.replays_natively(turn)) {
            target = target.with_reasoning_text(model.replays_reasoning_text());
        }
        let mut history_turn_starts = Vec::with_capacity(turns.len());
        for (_, turn) in &turns {
            history_turn_starts.push(messages.len());
            messages.extend(session_model::project_turn(turn, &target).replay());
        }
        let retained_messages = messages.len();
        let request = ModelRequest::with_history(messages, input, options.output().clone())
            .with_instructions(options.instructions())
            .with_generation(options.generation())
            .with_history_turn_starts(history_turn_starts)
            .with_cache_key(self.cache_key());

        (
            request,
            retained_messages,
            history,
            ReplayedHistory {
                projection,
                target,
                turns,
            },
        )
    }

    /// Returns the prompt-cache key shared by every turn request of this
    /// session, stable across handles and resume; a fork's own ID gives it a
    /// key of its own. Hashing bounds its length
    /// and keeps the host-chosen session ID off the provider wire.
    fn cache_key(&self) -> String {
        let digest = Sha256::digest(self.id.as_bytes());

        format!("ag-session-{}", hex::encode(&digest[..16]))
    }

    /// Rejects image-bearing input the history budget would evict at once,
    /// together with every earlier turn, instead of losing context silently.
    /// Context-budget admission bounds request weight, not retained bytes.
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
            reservation::acquire_request(
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
            reservation::acquire(
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
    output: OutputMode,
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
        let harness_config = &self.harness.inner;
        let max_history_bytes =
            harness_config.session_history_bytes(self.system_prompt.as_deref(), &self.output);
        let config = NewSession::new(self.id, self.output)
            .with_optional_system_prompt(self.system_prompt)
            .with_registration_identity(
                harness_config
                    .model_registration
                    .as_ref()
                    .map(|registration| registration.identity().clone()),
            );
        database
            .create_session(&config, harness_config.model.metadata(), max_history_bytes)
            .await?;
        let history = SessionHistory::new(max_history_bytes);

        Ok(Session {
            checkpoint: None,
            database,
            harness: self.harness,
            history,
            id: config.id().to_string(),
            model_generation: 0,
            output: config.output().clone(),
            system_prompt: config.system_prompt().map(str::to_string),
        })
    }
}

/// Finished history replayed by the next turn, with the projection it
/// follows. Every turn acquisition and session resume replaces it with the
/// turns and projection the store loads, so a live session and a resumed
/// one replay the same history.
pub(crate) struct SessionHistory {
    max_bytes: usize,
    projection: HistoryProjection,
    turns: Vec<(i64, HistoryTurn)>,
}

impl SessionHistory {
    pub(crate) fn new(max_bytes: usize) -> Self {
        Self {
            max_bytes,
            projection: HistoryProjection::default(),
            turns: Vec::new(),
        }
    }

    pub(crate) fn turns(&self) -> &[(i64, HistoryTurn)] {
        &self.turns
    }

    /// Replaces the history with loaded `turns`, oldest first with their
    /// positions, projected within the byte limit from `projection`, as the
    /// store loads them.
    fn replace(&mut self, turns: &[(i64, HistoryTurn)], projection: HistoryProjection) {
        (self.turns, self.projection) =
            context::project_history(turns, projection, &HistoryLimit::bytes(self.max_bytes));
    }
}

/// One durable turn being configured, created by [`Session::turn`].
///
/// Await it to run the turn, or call [`Self::start`] to run it with a
/// [`TurnControl`](crate::TurnControl).
#[must_use = "turns do not run until awaited or started"]
pub struct SessionTurn<'a> {
    host_id: Option<String>,
    input: TurnInput,
    options: Option<TurnOptions>,
    session: &'a mut Session,
}

impl<'a> SessionTurn<'a> {
    /// Runs the turn with exactly these options instead of the session
    /// defaults, without changing the defaults.
    ///
    /// Permission downgrades retain earlier tool results. Options are persisted
    /// before execution; completion is reported only after the resulting
    /// messages are committed.
    pub fn options(mut self, options: TurnOptions) -> Self {
        self.options = Some(options);

        self
    }

    /// Makes the turn idempotent under a host-assigned request ID.
    ///
    /// A retry with the same ID and effective request returns the recorded
    /// output and activity without running the model or tools; active or
    /// stopped retries fail with errors containing their recovery records, and
    /// a changed request under a used ID fails with a conflict. New attempts
    /// need new IDs. Requires [`Harness::execution_identity`] or a registered
    /// model; read results later with [`Session::recover`].
    pub fn host_id(mut self, host_id: impl Into<String>) -> Self {
        self.host_id = Some(host_id.into());

        self
    }

    /// Prepares the turn with a retained cancellation control.
    ///
    /// The returned future starts execution when polled, and dropping it
    /// requests cancellation. Its control observes settlement of persistence,
    /// writes, and commands even after the future is dropped. Cancellation may
    /// race a successful terminal commit; inspect stored history, or
    /// [`Session::recover`] a host ID, after settlement.
    ///
    /// # Errors
    ///
    /// The future returns the same errors as awaiting the turn, including
    /// [`TurnError::Cancelled`] when cancellation stops its waiter.
    pub fn start(self) -> ControlledTurn<'a, SessionError> {
        let (options, request) = self.prepare();
        let input = self.input;
        let mut session = self.session.detached();

        ControlledTurn::new(move |control| async move {
            session
                .send_observed(input, options, Some(control), request?)
                .await
        })
    }

    fn prepare(&self) -> (TurnOptions, Result<Option<HostRequest>, SessionError>) {
        let options = self.options.clone().unwrap_or_else(|| {
            self.session
                .harness
                .default_options(self.session.output.clone())
        });
        let request = self
            .host_id
            .clone()
            .map(|host_id| self.session.host_request(host_id, &self.input, &options))
            .transpose();

        (options, request)
    }
}

impl<'a> IntoFuture for SessionTurn<'a> {
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    type Output = Result<TurnOutcome, SessionError>;

    fn into_future(self) -> Self::IntoFuture {
        let (options, request) = self.prepare();
        let Self { input, session, .. } = self;

        Box::pin(async move { session.send_observed(input, options, None, request?).await })
    }
}

/// One storage-free turn being configured, created by [`Harness::turn`].
///
/// Await it to run the turn, or call [`Self::start`] to run it with a
/// [`TurnControl`](crate::TurnControl).
#[must_use = "turns do not run until awaited or started"]
pub struct OneShotTurn<'a> {
    harness: &'a Harness,
    input: TurnInput,
    options: TurnOptions,
}

impl<'a> OneShotTurn<'a> {
    /// Prepares the turn with a retained cancellation control.
    ///
    /// Dropping the returned future requests cancellation. Observe
    /// settlement of writes and commands through the retained control; it
    /// does not wait for remote provider work.
    ///
    /// # Errors
    ///
    /// The future returns [`TurnError`], including [`TurnError::Cancelled`].
    pub fn start(self) -> ControlledTurn<'a, TurnError> {
        let harness = self.harness.clone();
        let Self { input, options, .. } = self;

        ControlledTurn::new(move |control| async move {
            tokio::select! {
                biased;
                () = control.cancelled() => Err(TurnError::Cancelled),
                result = harness.run_once_observed(input, options, control.effects.clone()) => result,
            }
        })
    }
}

impl<'a> IntoFuture for OneShotTurn<'a> {
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;
    type Output = Result<TurnOutcome, TurnError>;

    fn into_future(self) -> Self::IntoFuture {
        let Self {
            harness,
            input,
            options,
        } = self;

        Box::pin(harness.run_once_observed(input, options, Effects::default()))
    }
}

/// Application-facing harness for one complete model turn.
///
/// A turn advertises policy-approved tools, executes validated native calls,
/// returns tool results to the model, and finishes with locally validated
/// structured output.
/// Session builders and resumed sessions capture this configuration; later
/// reconfiguration affects only newly obtained handles.
///
/// A harness is a cheap, shareable handle: cloning it shares one immutable
/// configuration, one lazily opened SQLite pool, and one in-memory context
/// calibration, so a host can hand clones to many concurrent sessions and
/// turns across tasks. Prefer [`Harness::builder`] for construction; see
/// [`HarnessBuilder::build`] for what it checks up front.
#[derive(Clone)]
pub struct Harness {
    inner: Arc<HarnessConfig>,
}

impl Harness {
    /// Starts a model-first harness configuration, checked by
    /// [`HarnessBuilder::build`].
    ///
    /// This is the preferred construction path; [`Self::new`] and
    /// [`Self::from_registry`] remain available.
    pub fn builder() -> HarnessBuilder {
        HarnessBuilder {
            budget: None,
            catalog: None,
            context_budget: None,
            context_estimator: None,
            context_summaries: ContextSummaryPolicy::Disabled,
            execution_identity: None,
            file_system: None,
            lifecycle: LifecycleEmitter::default(),
            max_history_bytes: None,
            model: None,
            model_reasoning_effort: None,
            model_selector: None,
            policy: ToolPolicy::default(),
            repository: None,
            storage: None,
        }
    }

    /// Creates a deny-by-default harness backed by the local filesystem.
    ///
    /// `context_budget` bounds every provider request of every turn; declare
    /// it from the model's context window. Prefer [`Self::builder`], which
    /// can derive the budget from the model's declared [`Model::profile`].
    pub fn new(model: impl Model + 'static, context_budget: ContextBudget) -> Self {
        Self::from_shared_model(Arc::new(model), context_budget)
    }

    /// Creates a harness using a registered model and its execution identity,
    /// weighing requests with the registered profile's estimator.
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

        Ok(Self::from_registration(registration))
    }

    /// Returns the captured registration, or `None` for direct construction.
    pub fn model_registration(&self) -> Option<&ModelRegistration> {
        self.inner.model_registration.as_ref()
    }

    /// Identifies all execution configuration not captured by explicit options.
    /// Required for host-ID submission, including injected models/filesystems.
    /// Hosts must revise it when their behavior or configuration changes.
    /// For registered models, this overrides the host execution assertion but
    /// retains the selected registration in request fingerprints.
    #[must_use]
    pub fn execution_identity(mut self, identity: ExecutionIdentity) -> Self {
        self.config_mut().execution_identity = Some(identity);

        self
    }

    /// Configures the SQLite database used by durable sessions.
    ///
    /// The first create or resume initializes one shared connection pool and
    /// runs migrations. Reconfiguring the path starts a new lazy pool for
    /// future handles; existing handles retain their shared database.
    #[cfg(feature = "sqlite")]
    #[must_use]
    pub fn database(mut self, path: impl Into<PathBuf>) -> Self {
        let config = self.config_mut();
        config.store = None;
        config.database = Arc::new(OnceCell::new());
        config.database_path = Some(path.into());

        self
    }

    /// Selects a host-provided session store, replacing SQLite configuration.
    ///
    /// Builders and sessions retain their selected store when this harness is
    /// reconfigured. `run_once` never accesses the store. Implementations must
    /// satisfy the atomic lifecycle contract of [`SessionStore`].
    #[must_use]
    pub fn store(mut self, store: Arc<dyn SessionStore>) -> Self {
        let config = self.config_mut();
        config.store = Some(store);
        #[cfg(feature = "sqlite")]
        {
            config.database = Arc::new(OnceCell::new());
            config.database_path = None;
        }

        self
    }

    /// Configures the validated repository root and Git executable used by
    /// tools.
    #[must_use]
    pub fn repository(mut self, repository: Repository) -> Self {
        self.config_mut().repository = Some(repository);

        self
    }

    /// Allows one built-in tool by default for `run_once` and `Session::send`.
    ///
    /// Explicit turn options replace this default policy completely.
    #[must_use]
    pub fn allow(mut self, tool: Tool) -> Self {
        let config = self.config_mut();
        config.policy = config.policy.allow(tool);

        self
    }

    /// Replaces the local filesystem implementation.
    #[must_use]
    pub fn file_system(mut self, file_system: impl FileSystem + 'static) -> Self {
        self.config_mut().file_system = Arc::new(file_system);

        self
    }

    /// Sends metadata-only turn, model, and tool events to `observer`.
    ///
    /// This observer owns model events for requests made through the harness.
    /// [`HarnessBuilder::lifecycle_observer`] is the preferred spelling.
    #[must_use]
    pub fn with_lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.config_mut().lifecycle = LifecycleEmitter::new(observer);

        self
    }

    /// Overrides the retained chat-history payload budget of new sessions.
    ///
    /// Without an override, a session created for a model with a declared
    /// context window, through its [`ModelProfile`] or registration, keeps
    /// the history bytes that window leaves after the reserved output, the
    /// fixed request cost, the system prompt, and the default policy's tool
    /// definitions, as the context estimator converts them; other sessions
    /// keep 256 KiB. Sessions store their budget and keep it on resume.
    /// Complete oldest turns are evicted when the budget is exceeded, so
    /// native tool-call and tool-result messages are never split.
    #[must_use]
    pub fn max_history_bytes(mut self, max_history_bytes: NonZeroUsize) -> Self {
        self.config_mut().history_budget = HistoryBudget::Fixed(max_history_bytes.get());

        self
    }

    /// Sets the reasoning depth for model calls that do not specify one.
    #[must_use]
    pub fn model_reasoning_effort(mut self, reasoning_effort: ReasoningEffort) -> Self {
        self.config_mut().model_reasoning_effort = Some(reasoning_effort);

        self
    }

    /// Replaces the approximate content estimator used by context projection.
    ///
    /// Requests keep the most recent complete turns that fit the effective
    /// [`ContextBudget`], and mandatory content that cannot fit fails with
    /// [`TurnError::ContextBudgetExceeded`] before acquisition. Estimates are
    /// deterministic approximations, never exact provider token counts;
    /// provider-reported input tokens calibrate them in memory, and replacing
    /// the estimator discards that calibration.
    #[must_use]
    pub fn context_estimator(mut self, estimator: impl ContextEstimator + 'static) -> Self {
        let config = self.config_mut();
        config.context_estimator = Arc::new(estimator);
        config.context_calibration = ContextCalibration::default();

        self
    }

    /// Selects whether turns may summarize their earlier tool rounds with an
    /// extra tool-free model request once a request outgrows the
    /// [`ContextBudget`]; disabled by default.
    ///
    /// Summaries replace rounds only in the turn's later requests and count
    /// toward any provider-call budget of the model, so hosts with a tight
    /// per-run call budget should leave them disabled. Sessions keep the
    /// policy of the harness that built them.
    #[must_use]
    pub fn context_summaries(mut self, policy: ContextSummaryPolicy) -> Self {
        self.config_mut().context_summaries = policy;

        self
    }

    /// Runs one turn with the harness defaults and no stored history.
    ///
    /// Plain strings are text input; use [`TurnInput`] for ordered text and
    /// images. Shorthand for `self.turn(input, defaults).await`, where the
    /// default permissions come from [`Self::allow`].
    ///
    /// # Errors
    ///
    /// Returns [`TurnError`] when the model fails, requests a denied tool,
    /// exceeds the context budget, or a requested repository operation fails.
    pub async fn run_once(
        &self,
        input: impl Into<TurnInput>,
        output: impl Into<OutputMode>,
    ) -> Result<TurnOutcome, TurnError> {
        self.turn(input, self.default_options(output.into())).await
    }

    /// Starts one stateless run of `request`, the run-level counterpart of
    /// [`Self::run_once`]; await the returned [`Run`] for its
    /// [`crate::RunOutcome`].
    ///
    /// Unlike [`Session::run`], nothing is stored or replayed and no schema
    /// is inherited: the request must set [`RunRequest::output`], and unset
    /// tool options use the harness defaults. The run records its turn in a
    /// fresh in-memory session, never in the configured store, so its
    /// outcome lists the run's files and commands like a durable session's.
    pub fn run(&self, request: RunRequest) -> Run {
        let budget = request.ledger();
        let turn = request.into_turn(None, self.inner.policy);
        let store: Arc<dyn SessionStore> = Arc::new(MemoryStore::new());
        let id = Self::turn_cache_key();
        let journal = RunJournal::new(Arc::clone(&store), id.clone(), &turn, self.inner.pricing);
        // Nothing replays the single turn, so no history budget applies.
        let mut harness = self
            .clone()
            .store(store)
            .max_history_bytes(NonZeroUsize::MAX);
        if let Some(budget) = budget {
            harness.config_mut().budget = budget;
        }

        Run::new(
            ControlledTurn::new(move |control| async move {
                let (input, options) = turn?;
                let mut session = harness
                    .session(id, options.output().clone())
                    .create()
                    .await?;

                Ok(session
                    .send_observed(input, options, Some(control), None)
                    .await?)
            }),
            journal,
        )
    }

    /// Configures one turn with exactly `options` and no stored history.
    ///
    /// Options replace the harness schema and permission defaults; an empty
    /// policy denies all tools. This never opens a store. Await the result to
    /// run it, or call [`OneShotTurn::start`] to keep a cancellation control.
    pub fn turn(&self, input: impl Into<TurnInput>, options: TurnOptions) -> OneShotTurn<'_> {
        OneShotTurn {
            harness: self,
            input: input.into(),
            options,
        }
    }

    /// Builds a durable session with `output`, usually a JSON
    /// [`OutputSchema`], as its default output mode.
    ///
    /// Explicit turn options and run requests may select a different mode
    /// without changing the stored default. Captures current configuration
    /// without opening the database.
    pub fn session(&self, id: impl Into<String>, output: impl Into<OutputMode>) -> SessionBuilder {
        SessionBuilder {
            harness: self.clone(),
            id: id.into(),
            output: output.into(),
            system_prompt: None,
        }
    }

    /// Resumes a durable session and restores its bounded finished history.
    ///
    /// Captures current harness defaults while retaining the stored output
    /// mode, system prompt, and history budget.
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
        history.replace(&loaded.turns, loaded.projection);

        Ok(Session {
            checkpoint: loaded.checkpoint,
            database,
            harness: self.clone(),
            history,
            id: id.to_string(),
            model_generation: loaded.model_generation,
            output: loaded.output,
            system_prompt: loaded.system_prompt,
        })
    }

    /// Creates session `new` from session `from`'s settled turns through
    /// index `at`, and resumes it.
    ///
    /// The fork keeps the source's output mode, system prompt, history
    /// budget, and model selection, and gets the prompt-cache key of its own
    /// identifier, so the two sessions never share a provider cache slot. It
    /// keeps the source's compaction checkpoint only when that covers no turn
    /// after `at`, and replays its history from the start of its budget. The
    /// source is left unchanged, so both sessions continue independently.
    ///
    /// Host request records of the copied turns are carried over: retrying
    /// a host request ID the source used through `at` on the fork returns
    /// its recorded outcome rather than starting a turn.
    ///
    /// The fork is created first and then resumed with this harness's
    /// model, which validates the model selection it copied. When that
    /// resume fails, for example on a source recorded with another model,
    /// `new` is already stored: resume it with a matching harness rather
    /// than forking to it again, which would fail as a conflict.
    ///
    /// # Errors
    ///
    /// Returns an [`crate::ErrorKind::NotFound`] failure when `from` or its
    /// settled turn `at` does not exist, an [`crate::ErrorKind::Conflict`]
    /// failure when `new` exists, an [`crate::ErrorKind::Config`] failure
    /// for a blank `new` identifier, storage that is not configured, or a
    /// fork recorded with another model, and an
    /// [`crate::ErrorKind::Store`] failure when the store fails or does not
    /// support forking.
    pub async fn fork(
        &self,
        from: &SessionId,
        at: TurnIndex,
        new: SessionId,
    ) -> Result<Session, HarnessError> {
        if new.as_str().trim().is_empty() {
            return Err(HarnessError::config(SessionError::InvalidData {
                reason: "session identifier must not be empty".to_string(),
            }));
        }
        let database = self.open_database().await?;
        let through = i64::try_from(at.get()).map_err(|_| SessionError::TurnNotFound {
            id: from.as_str().to_string(),
            turn: at.get(),
        })?;
        database
            .fork_session(from.as_str(), through, new.as_str())
            .await?;

        Ok(self.resume(new.as_str()).await?)
    }

    fn from_shared_model(model: Arc<dyn Model>, context_budget: ContextBudget) -> Self {
        Self::from_config(HarnessConfig::new(model, context_budget))
    }

    fn from_registration(registration: ModelRegistration) -> Self {
        Self::from_config(HarnessConfig::registered(registration))
    }

    fn from_config(config: HarnessConfig) -> Self {
        Self {
            inner: Arc::new(config),
        }
    }

    /// Returns this handle's configuration for reconfiguration, copying it
    /// first when other handles share it so they keep their configuration.
    fn config_mut(&mut self) -> &mut HarnessConfig {
        Arc::make_mut(&mut self.inner)
    }

    async fn run_once_observed(
        &self,
        input: TurnInput,
        options: TurnOptions,
        effects: Effects,
    ) -> Result<TurnOutcome, TurnError> {
        self.check_input_capability(&input)?;
        self.check_output_capability(&options)?;
        self.admit_mandatory_content(None, &input, &options)?;
        let _effects = effects.retain();
        let request = ModelRequest::new(input, options.output().clone())
            .with_instructions(options.instructions())
            .with_generation(options.generation())
            .with_cache_key(Self::turn_cache_key());
        let turn = self.inner.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let mut engine = self.engine(&options);
        engine.effects = effects;
        let result = engine.run(request, turn_id, None).await;

        Self::finish_turn(turn, result)
    }

    /// Returns a fresh prompt-cache key for one stateless turn, so its tool
    /// loop routes together without pinning unrelated turns to one backend.
    fn turn_cache_key() -> String {
        static NEXT_TURN: AtomicU64 = AtomicU64::new(0);
        let turn = NEXT_TURN.fetch_add(1, Ordering::Relaxed);

        format!("ag-turn-{:016x}", RandomState::new().hash_one(turn))
    }

    /// Admits mandatory content against the context budget scaled by the
    /// provider-reported calibration shared with this harness's sessions,
    /// and returns the calibrated weight left for history.
    fn admit_mandatory_content(
        &self,
        system_prompt: Option<&str>,
        input: &TurnInput,
        options: &TurnOptions,
    ) -> Result<u64, TurnError> {
        let config = &self.inner;
        let (budget, calibration) = config
            .context_calibration
            .effective_budget(config.context_budget);

        context::admit_mandatory_content(
            config.context_estimator.as_ref(),
            budget,
            system_prompt,
            input,
            options,
        )
        .map_err(|error| {
            ContextCalibration::declared_failure(config.context_budget, calibration, error)
        })
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
            options.output().clone(),
        );
        let turn = self.inner.lifecycle.start_turn();
        let turn_id = turn.as_ref().map(TurnLifecycle::id);
        let engine = self.engine(options);
        let result = engine.run(request, turn_id, None).await;

        Self::finish_turn(turn, result)
    }

    /// Reports a standalone turn's end to its lifecycle, with what a failed
    /// turn did before failing, and returns its outcome.
    fn finish_turn(
        turn: Option<TurnLifecycle>,
        result: Result<FinishedTurn, EngineFailure>,
    ) -> Result<TurnOutcome, TurnError> {
        match result {
            Ok(FinishedTurn { outcome, .. }) => {
                if let Some(turn) = turn {
                    turn.completed();
                }

                Ok(outcome)
            }
            Err(EngineFailure { error, report, .. }) => {
                if let Some(mut turn) = turn {
                    turn.set_failure_report(report);
                    turn.failed(error.error_type());
                }

                Err(error)
            }
        }
    }

    /// Returns the host store, else the configured SQLite database, opened
    /// once and shared by clones.
    #[cfg(feature = "sqlite")]
    async fn open_database(&self) -> Result<Arc<dyn SessionStore>, SessionError> {
        let config = &self.inner;
        if let Some(store) = &config.store {
            return Ok(Arc::clone(store));
        }
        let path = config
            .database_path
            .as_deref()
            .ok_or(SessionError::StorageRequired)?;

        config
            .database
            .get_or_try_init(|| Database::open(path))
            .await
            .map(|database| Arc::new(database.clone()) as Arc<dyn SessionStore>)
    }

    /// Returns the host store; without the `sqlite` feature it is the only
    /// durable storage.
    #[cfg(not(feature = "sqlite"))]
    fn open_database(&self) -> Ready<Result<Arc<dyn SessionStore>, SessionError>> {
        ready(
            self.inner
                .store
                .clone()
                .ok_or(SessionError::StorageRequired),
        )
    }

    fn validate_session_model(&self, id: &str, loaded: &LoadedSession) -> Result<(), SessionError> {
        if loaded.registration_identity.as_ref()
            != self
                .inner
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
        let metadata = self.inner.model.metadata();
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

    fn check_input_capability(&self, input: &TurnInput) -> Result<(), TurnError> {
        self.inner.model.validate_input(input)?;
        if input.has_images()
            && let Some(registration) = &self.inner.model_registration
            && !registration.capabilities().image_input
        {
            return Err(TurnError::Model(ModelError::UnsupportedImageInput {
                reason: "registered model does not declare image input support".to_string(),
            }));
        }

        Ok(())
    }

    /// Rejects a turn before execution when the model cannot answer in its
    /// output mode or apply its generation settings.
    fn check_output_capability(&self, options: &TurnOptions) -> Result<(), TurnError> {
        let model = self.inner.model.as_ref();
        options.output().require_text_support_if_text(model)?;
        let generation = options.generation();
        if !generation.is_default() {
            model.validate_generation(&generation)?;
        }

        Ok(())
    }

    fn default_options(&self, output: OutputMode) -> TurnOptions {
        TurnOptions::new(output, self.inner.policy)
    }

    /// Returns an engine for a durable turn that reports to `control`, and
    /// streams events when its run does.
    fn controlled_engine<'a>(
        &'a self,
        options: &'a TurnOptions,
        control: Option<&'a TurnControl>,
        effects: Effects,
    ) -> Engine<'a> {
        Engine {
            control,
            effects,
            events: control.and_then(TurnControl::events),
            ..self.engine(options)
        }
    }

    fn engine<'a>(&'a self, options: &'a TurnOptions) -> Engine<'a> {
        let config = &self.inner;

        Engine {
            budget: config.budget.as_ref(),
            concurrency: config.concurrency.as_deref(),
            context_budget: config.context_budget,
            context_calibration: &config.context_calibration,
            context_estimator: config.context_estimator.as_ref(),
            context_summaries: config.context_summaries,
            control: None,
            effects: Effects::default(),
            events: None,
            file_system: &config.file_system,
            lifecycle: &config.lifecycle,
            max_output: config.max_output,
            model: &config.model,
            model_reasoning_effort: config.model_reasoning_effort,
            options,
            pricing: config.pricing,
            repository: config.repository.as_ref(),
        }
    }
}

/// Model-first configuration of a [`Harness`].
///
/// Start with [`Harness::builder`], select a model with [`Self::model`], and
/// finish with [`Self::build`]. Each setter replaces its earlier value; unset
/// options keep the [`Harness::new`] defaults: deny-by-default tools, the
/// local filesystem, and no store. Registered models still use
/// [`Harness::from_registry`]. Instead of a model, a host may give a
/// [`ModelCatalog`] and a [`ModelSelector`].
#[must_use = "a builder does nothing until built"]
pub struct HarnessBuilder {
    budget: Option<Arc<dyn BudgetLedger>>,
    catalog: Option<Arc<ModelCatalog>>,
    context_budget: Option<ContextBudget>,
    context_estimator: Option<Arc<dyn ContextEstimator>>,
    context_summaries: ContextSummaryPolicy,
    execution_identity: Option<ExecutionIdentity>,
    file_system: Option<Arc<dyn FileSystem>>,
    lifecycle: LifecycleEmitter,
    max_history_bytes: Option<NonZeroUsize>,
    model: Option<Arc<dyn Model>>,
    model_reasoning_effort: Option<ReasoningEffort>,
    model_selector: Option<ModelSelector>,
    policy: ToolPolicy,
    repository: Option<Repository>,
    storage: Option<StorageSource>,
}

impl HarnessBuilder {
    /// Selects the model every turn uses, taking precedence over a
    /// [`Self::catalog`]. Setting both this and [`Self::model_selector`]
    /// fails the build.
    ///
    /// Without [`Self::context_budget`], the request budget and estimator
    /// come from the model's declared [`Model::profile`].
    pub fn model(mut self, model: Arc<dyn Model>) -> Self {
        self.model = Some(model);

        self
    }

    /// Selects the model from `catalog` when no [`Self::model`] is set,
    /// through [`Self::model_selector`] or else [`Role::SMART`].
    ///
    /// Without [`Self::context_budget`], the request budget and estimator
    /// come from the resolved [`crate::catalog::ModelHandle::profile`].
    ///
    /// Runs report a [`crate::Cost`] priced with the resolved entry's
    /// [`crate::catalog::Pricing`], which a host replaces with
    /// [`crate::catalog::CatalogBuilder::pricing`] or declares for its own
    /// models with [`crate::catalog::ModelSpec::with_pricing`]. A model set
    /// with [`Self::model`] has no prices and reports no cost.
    ///
    /// The resolved entry's [`crate::catalog::ModelSpec::max_concurrency`]
    /// bounds the model requests in flight at once across every session
    /// and run of every harness built from `catalog`; it limits requests,
    /// not runs, so a run waits for a permit before each request and holds
    /// none between them. A model set with [`Self::model`] has no cap.
    pub fn catalog(mut self, catalog: Arc<ModelCatalog>) -> Self {
        self.catalog = Some(catalog);

        self
    }

    /// Draws every model request of every run, turn, and session of the
    /// harness from `ledger`; the default, [`Unlimited`], never refuses.
    /// Hand one [`crate::budget::SharedBudget`] to several harnesses to cap
    /// their total spend.
    ///
    /// [`RunRequest::budget`] replaces `ledger` for one run: that run never
    /// consults it, so it is a ceiling only for runs without their own
    /// ledger. To stack caps, give the run a ledger that reserves from both.
    pub fn budget(mut self, ledger: Arc<dyn BudgetLedger>) -> Self {
        self.budget = Some(ledger);

        self
    }

    /// Chooses which [`Self::catalog`] model every turn uses. Building
    /// fails without a catalog or together with an explicit [`Self::model`].
    pub fn model_selector(mut self, selector: impl Into<ModelSelector>) -> Self {
        self.model_selector = Some(selector.into());

        self
    }

    /// Bounds every provider request with `context_budget`, overriding the
    /// budget the model's profile declares.
    pub fn context_budget(mut self, context_budget: ContextBudget) -> Self {
        self.context_budget = Some(context_budget);

        self
    }

    /// Stores durable sessions in the SQLite database at `path`, like
    /// [`Harness::database`]. Replaces an earlier [`Self::store`].
    ///
    /// Clones of the built harness share one lazily opened connection pool.
    #[cfg(feature = "sqlite")]
    pub fn sqlite(mut self, path: impl Into<PathBuf>) -> Self {
        self.storage = Some(StorageSource::Sqlite(path.into()));

        self
    }

    /// Stores durable sessions in a host-provided store, like
    /// [`Harness::store`]. Replaces an earlier [`Self::sqlite`].
    pub fn store(mut self, store: Arc<dyn SessionStore>) -> Self {
        self.storage = Some(StorageSource::Store(store));

        self
    }

    /// Configures the repository used by tools, like [`Harness::repository`].
    pub fn repository(mut self, repository: Repository) -> Self {
        self.repository = Some(repository);

        self
    }

    /// Allows one built-in tool by default, like [`Harness::allow`].
    pub fn allow(mut self, tool: Tool) -> Self {
        self.policy = self.policy.allow(tool);

        self
    }

    /// Replaces the local filesystem, like [`Harness::file_system`].
    pub fn file_system(mut self, file_system: impl FileSystem + 'static) -> Self {
        self.file_system = Some(Arc::new(file_system));

        self
    }

    /// Sends metadata-only events to `observer`, like
    /// [`Harness::with_lifecycle_observer`].
    pub fn lifecycle_observer(mut self, observer: impl LifecycleObserver + 'static) -> Self {
        self.lifecycle = LifecycleEmitter::new(observer);

        self
    }

    /// Overrides the retained chat-history payload budget of new sessions,
    /// which otherwise derives from the model's declared context window, like
    /// [`Harness::max_history_bytes`].
    pub fn max_history_bytes(mut self, max_history_bytes: NonZeroUsize) -> Self {
        self.max_history_bytes = Some(max_history_bytes);

        self
    }

    /// Sets the default reasoning depth, like
    /// [`Harness::model_reasoning_effort`].
    pub fn model_reasoning_effort(mut self, reasoning_effort: ReasoningEffort) -> Self {
        self.model_reasoning_effort = Some(reasoning_effort);

        self
    }

    /// Replaces the content estimator, like [`Harness::context_estimator`],
    /// including one derived from the model's profile or registration.
    pub fn context_estimator(mut self, estimator: impl ContextEstimator + 'static) -> Self {
        self.context_estimator = Some(Arc::new(estimator));

        self
    }

    /// Selects in-turn round summaries, like [`Harness::context_summaries`].
    pub fn context_summaries(mut self, policy: ContextSummaryPolicy) -> Self {
        self.context_summaries = policy;

        self
    }

    /// Identifies the execution configuration, like
    /// [`Harness::execution_identity`].
    pub fn execution_identity(mut self, identity: ExecutionIdentity) -> Self {
        self.execution_identity = Some(identity);

        self
    }

    /// Checks the configuration and returns a shareable harness.
    ///
    /// `build` checks only that a model is selected or resolves from the
    /// catalog, that a budget is set or the model declares a valid profile,
    /// and that a repository is set when the default policy allows a
    /// repository tool. Storage is not touched: the SQLite database or host
    /// store is first opened by a session's `create` or `resume`, which
    /// reports its failures as [`SessionError`].
    ///
    /// # Errors
    ///
    /// Returns a [`HarnessError`] of kind [`crate::ErrorKind::Config`]
    /// wrapping the failed [`HarnessBuildError`] check.
    pub fn build(self) -> Result<Harness, HarnessError> {
        let (model, profile, cataloged) = match (self.model, self.catalog, self.model_selector) {
            (Some(_), _, Some(_)) => {
                return Err(HarnessBuildError::ConflictingModelSelection.into());
            }
            (Some(model), _, None) => {
                let profile = model.profile();

                (model, profile, None)
            }
            (None, Some(catalog), selector) => {
                let selector = selector.unwrap_or(ModelSelector::Role(Role::SMART));
                let handle = catalog
                    .resolve(&selector)
                    .map_err(HarnessBuildError::Catalog)?;

                (handle.model(), handle.profile(), Some(handle))
            }
            (None, None, Some(_)) => return Err(HarnessBuildError::CatalogRequired.into()),
            (None, None, None) => return Err(HarnessBuildError::ModelRequired.into()),
        };
        let repository_tool_allowed = [Tool::Bash, Tool::Read, Tool::Write]
            .into_iter()
            .any(|tool| self.policy.allows(tool));
        if repository_tool_allowed && self.repository.is_none() {
            return Err(HarnessBuildError::RepositoryRequired.into());
        }
        let mut config = Self::direct_config(model, profile, self.context_budget)?;
        if let Some(context_estimator) = self.context_estimator {
            config.context_estimator = context_estimator;
        }
        if let Some(identity) = self.execution_identity {
            config.execution_identity = Some(identity);
        }
        if let Some(file_system) = self.file_system {
            config.file_system = file_system;
        }
        if let Some(max_history_bytes) = self.max_history_bytes {
            config.history_budget = HistoryBudget::Fixed(max_history_bytes.get());
        }
        match self.storage {
            #[cfg(feature = "sqlite")]
            Some(StorageSource::Sqlite(path)) => config.database_path = Some(path),
            Some(StorageSource::Store(store)) => config.store = Some(store),
            None => {}
        }
        if let Some(budget) = self.budget {
            config.budget = budget;
        }
        if let Some(handle) = cataloged {
            config.concurrency = handle.concurrency();
            config.max_output = handle.spec().max_output();
            config.model_id = Some(handle.id().clone());
            config.pricing = handle.spec().pricing();
        }
        config.context_summaries = self.context_summaries;
        config.lifecycle = self.lifecycle;
        config.model_reasoning_effort = self.model_reasoning_effort;
        config.policy = self.policy;
        config.repository = self.repository;

        Ok(Harness::from_config(config))
    }

    /// Returns the configuration of a directly supplied model, bounded by
    /// `context_budget` or else by its declared `profile`, whose estimator
    /// it adopts.
    fn direct_config(
        model: Arc<dyn Model>,
        profile: Option<ModelProfile>,
        context_budget: Option<ContextBudget>,
    ) -> Result<HarnessConfig, HarnessBuildError> {
        let context_budget = match (context_budget, profile) {
            (Some(context_budget), _) => context_budget,
            (None, Some(profile)) => profile
                .context_budget()
                .map_err(HarnessBuildError::InvalidProfile)?,
            (None, None) => return Err(HarnessBuildError::ContextBudgetRequired),
        };
        let mut config = HarnessConfig::new(model, context_budget);
        if let Some(profile) = profile {
            config.context_estimator = Arc::new(profile.estimator());
            config.history_budget = HistoryBudget::Window;
        }

        Ok(config)
    }
}

/// Invalid [`HarnessBuilder`] configuration, reported by
/// [`HarnessBuilder::build`] inside a [`HarnessError`].
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum HarnessBuildError {
    /// No model was selected.
    #[error("harness requires a model; call `model` or `catalog`")]
    ModelRequired,
    /// A model selector was set without a catalog to resolve it.
    #[error("model selector requires a catalog; call `catalog`")]
    CatalogRequired,
    /// The catalog could not resolve the model selector.
    #[error("cannot select the model from the catalog")]
    Catalog(#[source] CatalogError),
    /// Both an explicit model and a model selector were set.
    #[error("set either `model` or `model_selector`, not both")]
    ConflictingModelSelection,
    /// No budget was set and the model declares no context profile.
    #[error("model declares no context profile; call `context_budget`")]
    ContextBudgetRequired,
    /// The model's declared profile leaves no request capacity.
    #[error("model's declared context profile is invalid")]
    InvalidProfile(#[source] ContextBudgetError),
    /// The default policy allows a repository tool but no repository is set.
    #[error("allowed repository tools require a repository; call `repository`")]
    RepositoryRequired,
}

/// Session storage selected on a [`HarnessBuilder`].
enum StorageSource {
    /// SQLite database path.
    #[cfg(feature = "sqlite")]
    Sqlite(PathBuf),
    /// Host-provided store.
    Store(Arc<dyn SessionStore>),
}

/// Configuration shared by clones of one [`Harness`]; a reconfiguring
/// method copies it first when other handles still share it.
#[derive(Clone)]
struct HarnessConfig {
    /// Ledger that reserves every model request; a run's own ledger
    /// replaces it in that run's copy of the configuration.
    budget: Arc<dyn BudgetLedger>,
    /// Concurrency cap of [`Self::model`], from the catalog entry the
    /// builder selected and shared with every harness built from that
    /// catalog; `None` for any other model.
    concurrency: Option<Arc<Semaphore>>,
    context_budget: ContextBudget,
    context_calibration: ContextCalibration,
    context_estimator: Arc<dyn ContextEstimator>,
    context_summaries: ContextSummaryPolicy,
    #[cfg(feature = "sqlite")]
    database: Arc<OnceCell<Database>>,
    #[cfg(feature = "sqlite")]
    database_path: Option<PathBuf>,
    execution_identity: Option<ExecutionIdentity>,
    file_system: Arc<dyn FileSystem>,
    history_budget: HistoryBudget,
    lifecycle: LifecycleEmitter,
    /// Cataloged maximum output of [`Self::model`], which bounds each
    /// request's budget estimate; `None` for any other model.
    max_output: Option<u64>,
    model: Arc<dyn Model>,
    /// Catalog identifier of [`Self::model`] when the builder selected it
    /// from a [`ModelCatalog`].
    model_id: Option<ModelId>,
    model_reasoning_effort: Option<ReasoningEffort>,
    model_registration: Option<ModelRegistration>,
    policy: ToolPolicy,
    /// Prices of [`Self::model`], from the catalog entry the builder
    /// selected; `None` for any other model.
    pricing: Option<Pricing>,
    repository: Option<Repository>,
    store: Option<Arc<dyn SessionStore>>,
}

impl HarnessConfig {
    /// Returns the deny-by-default configuration for a directly supplied
    /// model, backed by the local filesystem.
    fn new(model: Arc<dyn Model>, context_budget: ContextBudget) -> Self {
        Self {
            budget: Arc::new(Unlimited),
            concurrency: None,
            context_budget,
            context_calibration: ContextCalibration::default(),
            context_estimator: Arc::new(HeuristicContextEstimator::DEFAULT),
            context_summaries: ContextSummaryPolicy::Disabled,
            #[cfg(feature = "sqlite")]
            database: Arc::new(OnceCell::new()),
            #[cfg(feature = "sqlite")]
            database_path: None,
            execution_identity: None,
            file_system: Arc::new(LocalFileSystem),
            history_budget: HistoryBudget::Fixed(DEFAULT_MAX_HISTORY_BYTES),
            lifecycle: LifecycleEmitter::default(),
            max_output: None,
            model,
            model_id: None,
            model_reasoning_effort: None,
            model_registration: None,
            policy: ToolPolicy::default(),
            pricing: None,
            repository: None,
            store: None,
        }
    }

    /// Returns the configuration for a captured registration: its model,
    /// budget, profile estimator, and execution identity.
    fn registered(registration: ModelRegistration) -> Self {
        let mut config = Self::new(registration.model(), registration.context_budget());
        config.context_estimator = Arc::new(registration.capabilities().profile.estimator());
        config.execution_identity = Some(registration.identity().clone());
        config.history_budget = HistoryBudget::Window;
        config.model_registration = Some(registration);

        config
    }

    /// Returns the retained history bytes of a new session with
    /// `system_prompt` and default `schema`: the configured budget, or the
    /// one derived from the model's context window, which falls back to
    /// [`DEFAULT_MAX_HISTORY_BYTES`] when the estimator cannot convert
    /// weight into bytes.
    fn session_history_bytes(&self, system_prompt: Option<&str>, output: &OutputMode) -> usize {
        match self.history_budget {
            HistoryBudget::Fixed(max_history_bytes) => max_history_bytes,
            HistoryBudget::Window => context::window_history_bytes(
                self.context_estimator.as_ref(),
                self.context_budget,
                system_prompt,
                &TurnOptions::new(output.clone(), self.policy),
            )
            .unwrap_or(DEFAULT_MAX_HISTORY_BYTES),
        }
    }
}

/// How a [`Harness`] chooses the retained history bytes of new sessions.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum HistoryBudget {
    /// A host override, or the default for a model without a declared
    /// context window.
    Fixed(usize),
    /// Derived from the model's declared context window when the session is
    /// created.
    Window,
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
