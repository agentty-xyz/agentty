//! In-process [`AgentChannel`] adapter backed by durable `ag-harness`
//! sessions.
//!
//! The harness session shares the Agentty session id and is canonical for
//! model context. A session without harness history restarts from Agentty's
//! replay transcript. Session turns return a versioned instruction key and
//! the digest of the turn input that carried the full contract as their
//! provider conversation id. A follow-up session turn sends only the refresh
//! reminder while the host passes that current id back and the harness still
//! replays that input; history eviction re-sends the full contract.

use std::collections::{BTreeMap, HashMap};
use std::future::poll_fn;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_contracts::{
    AgentChannel, AgentError, AgentFuture, ContextCompaction, ReasoningLevel, SessionRef,
    StartSessionRequest, TurnEvent, TurnRequest, TurnResult,
};
use ag_harness::provider::chatgpt_is_signed_in;
use ag_harness::transcript::TranscriptInput;
use ag_harness::{
    CancelToken, ErrorKind, EventStream, Harness, HarnessError, Repository, Run, RunOutcome,
    Session, SessionError, TurnRange, TurnRecord, Unsettled,
};
use ag_protocol::ProtocolRequestProfile;
use ag_session::AgentKind;
use ag_telemetry::{Context, FutureExt as _};
use futures_core::Stream as _;
use tokio::sync::{mpsc, oneshot};

use crate::agent::native::{
    ActivityBridge, HarnessContext, ModelSelection, NativeHarnessConfig, NativePrompt,
    NativeTurnError, OutputProjector, RunOptions, cleanup_failure, failure, output_schema,
    parse_output, reject_attachments, repository, running_context_window, system_prompt,
    turn_notice,
};
use crate::agent::{
    InstructionDeliveryMode, apply_response_style_prompt, execution_policy,
    instruction_bootstrap_key, plan_instruction_delivery,
};

/// Started runs whose settlement a shutdown or the session's next turn must
/// still observe, by session.
type ActiveTurns = Arc<Mutex<HashMap<String, ActiveRun>>>;

/// [`AgentChannel`] adapter that runs harness turns in process.
pub(crate) struct NativeAgentChannel {
    active: ActiveTurns,
    config: NativeHarnessConfig,
}

impl NativeAgentChannel {
    /// Creates a channel storing session history under `config`.
    pub(crate) fn new(config: NativeHarnessConfig) -> Self {
        Self {
            active: Arc::default(),
            config,
        }
    }
}

impl AgentChannel for NativeAgentChannel {
    /// Returns immediately; the harness session opens on the first turn.
    fn start_session(
        &self,
        req: StartSessionRequest,
    ) -> AgentFuture<Result<SessionRef, AgentError>> {
        let session_id = req.session_id;

        Box::pin(async move { Ok(SessionRef { session_id }) })
    }

    /// Runs one durable harness turn.
    ///
    /// Dropping the future cancels the turn; [`Self::shutdown_session`] then
    /// waits for its cleanup.
    ///
    /// # Errors
    /// Returns [`AgentError::InterruptedByUser`] when the harness cancelled
    /// the turn, or [`AgentError::Backend`] for unsupported controls,
    /// configuration, provider, settlement, or protocol failures.
    fn run_turn(
        &self,
        session_id: String,
        req: TurnRequest,
        events: mpsc::UnboundedSender<TurnEvent>,
    ) -> AgentFuture<Result<TurnResult, AgentError>> {
        let active = Arc::clone(&self.active);
        let config = self.config.clone();

        Box::pin(async move {
            run_session_turn(&config, &active, session_id, req, events)
                .await
                .map_err(AgentError::from)
        })
    }

    /// Cancels the session's started turn and waits for its cleanup.
    ///
    /// A failed or abandoned cleanup keeps the turn registered so the
    /// session's next turn retries it.
    fn shutdown_session(&self, session_id: String) -> AgentFuture<Result<(), AgentError>> {
        let active = Arc::clone(&self.active);

        Box::pin(async move {
            let run = lock(&active).get(&session_id).cloned();
            if let Some(run) = run {
                run.cancel.cancel();
            }

            settle_previous_turn(&active, &session_id)
                .await
                .map_err(AgentError::Runtime)
        })
    }
}

/// Opens the durable session, runs one turn, and waits for it to settle.
async fn run_session_turn(
    config: &NativeHarnessConfig,
    active: &ActiveTurns,
    session_id: String,
    req: TurnRequest,
    events: mpsc::UnboundedSender<TurnEvent>,
) -> Result<TurnResult, NativeTurnError> {
    execution_policy::validate(AgentKind::Harness, &req.execution_policy)
        .map_err(|error| error.to_string())?;
    let environment = config.environment();
    let selection = ModelSelection::resolve(&req.model, &environment)?;
    let (compacts, summary_model) = match &req.context_compaction {
        ContextCompaction::Off => (false, None),
        ContextCompaction::Automatic { summary_model } => (
            true,
            selection.summary_model(summary_model.as_deref(), &environment, chatgpt_is_signed_in),
        ),
    };
    let profile = req.request_kind.protocol_profile();
    let options = RunOptions::new(profile, req.permission_mode, &environment)?;
    let repository = repository(&req.folder, &environment)?;
    reject_attachments(&req.prompt.attachments)?;
    let prompt_text = apply_response_style_prompt(req.prompt.clone(), profile, req.response_style)
        .map_err(failure("Failed to apply the response style"))?
        .agent_text();
    let continuation = req.continuation.into_parts();
    let replay_transcript = continuation.replay_transcript.or_else(|| {
        continuation
            .live_transcript
            .and_then(|transcript| transcript.replay_text())
    });
    let bootstrap = BootstrapMarker::parse(continuation.provider_conversation_id.as_deref());
    settle_previous_turn(active, &session_id).await?;
    let running_context_window = running_context_window(&selection, &environment).await?;
    let database = config.session_database(&session_id).await?;
    let opener = SessionOpener {
        compacts,
        database: &database,
        environment: &environment,
        events: &events,
        profile,
        provider_call_budget: req.provider_call_budget.as_ref(),
        reasoning_level: req.reasoning_level,
        repository: &repository,
        running_context_window,
        session_id: &session_id,
        summary_model: summary_model.as_ref(),
    };
    let (session, history) = opener.open(&selection).await?;
    // Turns since the last full contract, counting its own turn; `None` once
    // the harness no longer loads it.
    let bootstrap_age = bootstrap
        .as_ref()
        .and_then(|bootstrap| history.age_of(bootstrap.input_digest));
    let delivery = plan_instruction_delivery(
        &req.request_kind,
        bootstrap_age.is_some().then_some(session_id.as_str()),
        bootstrap.as_ref().map(|bootstrap| bootstrap.key.as_str()),
        !history.has_finished_turn && replay_transcript.is_some(),
    );
    let input = NativePrompt {
        delivery,
        agent_profile_prompt: req.agent_profile.current(),
        agent_profile_update: req.agent_profile.update(),
        prompt: &prompt_text,
        protocol_profile: profile,
        replay_transcript: replay_transcript.as_deref(),
        workspace_root: &req.folder,
    }
    .render()?;
    let input_digest = input_digest(&input);
    let (run, stream) = ActiveRun::start(session.run(options.request(input)));
    // Dropping this turn cancels the run, which keeps settling on its task.
    let _cancel_on_drop = CancelOnDrop(run.cancel.clone());
    lock(active).insert(session_id.clone(), run.clone());
    let ((), settled) = tokio::join!(forward_output(stream, &events), run.settle());
    // A failed settlement stays registered so the next turn retries it.
    let outcome = settled?;
    lock(active).remove(&session_id);
    // Only a concurrent shutdown, which cancelled the run, observes its
    // outcome first.
    let outcome = outcome.ok_or(NativeTurnError::Stopped)?;
    let output = outcome.result.map_err(NativeTurnError::from_run)?;
    let assistant_message = parse_output(&output, profile)?;
    // An unversioned id makes the next session turn re-send the session
    // contract: a utility or review turn bootstrapped its own, or the
    // context budget dropped the turn that carried it from this request.
    let bootstrap_replayed =
        bootstrap_age.is_some_and(|age| outcome.report.history().replayed_turns() >= age);
    let provider_conversation_id = if profile != ProtocolRequestProfile::SessionTurn {
        Some(session_id)
    } else if delivery != InstructionDeliveryMode::DeltaOnly {
        BootstrapMarker::encode(&session_id, input_digest)
    } else if bootstrap_replayed {
        continuation.provider_conversation_id
    } else {
        Some(session_id)
    };

    Ok(TurnResult {
        assistant_message,
        context_reset: false,
        harness_report: serde_json::to_string(&outcome.report).ok(),
        input_tokens: outcome.usage.input,
        output_tokens: outcome.usage.output,
        provider_conversation_id,
    })
}

/// Forwards the output a run streams as [`TurnEvent::Output`] events, and
/// its compaction attempts worth noting as [`TurnEvent::Notice`] events,
/// until the run's stream ends.
async fn forward_output(mut stream: EventStream, events: &mpsc::UnboundedSender<TurnEvent>) {
    let mut projector = OutputProjector::default();
    while let Some(event) = poll_fn(|context| Pin::new(&mut stream).poll_next(context)).await {
        if let Some(notice) = turn_notice(&event.kind) {
            let _ = events.send(TurnEvent::Notice(notice));
        }
        for output in projector.project(event.kind) {
            let _ = events.send(TurnEvent::Output(output));
        }
    }
}

/// One session's started run, shared by its turn and a concurrent shutdown.
#[derive(Clone)]
struct ActiveRun {
    cancel: CancelToken,
    settlement: Arc<tokio::sync::Mutex<Settlement>>,
}

impl ActiveRun {
    /// Diagnostic for a settlement task that panicked before reporting.
    const TASK_PANICKED: &str = "Harness run stopped without reporting its outcome";

    /// Starts `run` on its own task, so a dropped turn still settles it, and
    /// returns it with its event stream.
    fn start(run: Run) -> (Self, EventStream) {
        let (stream, handle) = run.into_parts();
        let cancel = handle.cancel_token();
        let mut settlement = Settlement::default();
        settlement.spawn(async move { Attempt::Finished(Box::new(handle.outcome().await)) });

        (
            Self {
                cancel,
                settlement: Arc::new(tokio::sync::Mutex::new(settlement)),
            },
            stream,
        )
    }

    /// Waits until the run settles, or until a retry of cleanup that failed
    /// finishes, and returns the outcome to the first caller that observes
    /// it. The run and each retry execute on their own tasks, so a dropped
    /// caller loses neither.
    ///
    /// # Errors
    /// Returns the cleanup that still failed, which the next call retries,
    /// or a failure when a settlement task panicked.
    async fn settle(&self) -> Result<Option<RunOutcome>, String> {
        let mut settlement = self.settlement.lock().await;
        if settlement.pending.is_none()
            && let Some(unsettled) = settlement.unsettled.take()
        {
            settlement.spawn(async move { Attempt::Retried(unsettled.retry().await) });
        }
        let Some(receiver) = settlement.pending.as_mut() else {
            return Ok(None);
        };
        let attempt = receiver.await;
        settlement.pending = None;
        let (outcome, cleanup) = match attempt.ok().ok_or(Self::TASK_PANICKED)? {
            Attempt::Finished(mut outcome) => {
                let cleanup = std::mem::replace(&mut outcome.settlement, Ok(()));

                (Some(*outcome), cleanup)
            }
            Attempt::Retried(cleanup) => (None, cleanup),
        };
        cleanup.map_err(|unsettled| settlement.keep(unsettled))?;

        Ok(outcome)
    }
}

/// What an [`ActiveRun`] still needs to settle.
#[derive(Default)]
struct Settlement {
    /// Report of the run's task, or of the latest cleanup retry, until
    /// observed.
    pending: Option<oneshot::Receiver<Attempt>>,
    /// Cleanup that failed, retried by the next settlement.
    unsettled: Option<Unsettled>,
}

impl Settlement {
    /// Runs `attempt` on its own task in the current trace context and waits
    /// for its report next.
    fn spawn(&mut self, attempt: impl Future<Output = Attempt> + Send + 'static) {
        let (sender, receiver) = oneshot::channel();
        tokio::spawn(
            async move {
                let _ = sender.send(attempt.await);
            }
            .with_context(Context::current()),
        );
        self.pending = Some(receiver);
    }

    /// Keeps failed cleanup for the next settlement and describes it.
    fn keep(&mut self, unsettled: Unsettled) -> String {
        let error = cleanup_failure(unsettled.error());
        self.unsettled = Some(unsettled);

        error
    }
}

/// What a settlement task reports.
enum Attempt {
    /// The run finished with its outcome.
    Finished(Box<RunOutcome>),
    /// A cleanup retry finished.
    Retried(Result<(), Unsettled>),
}

/// Cancels a run when the turn waiting for it is dropped; cancelling a
/// finished run has no effect.
struct CancelOnDrop(CancelToken);

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

/// Versioned instruction key and input digest of the session turn that last
/// carried the full contract, encoded in its provider conversation id.
struct BootstrapMarker {
    input_digest: u64,
    key: String,
}

impl BootstrapMarker {
    /// Separates the instruction key from the hexadecimal input digest.
    const DIGEST_SEPARATOR: char = '#';

    /// Parses a stored provider conversation id; ids without a digest, such
    /// as unversioned or earlier keys, yield `None`.
    fn parse(provider_conversation_id: Option<&str>) -> Option<Self> {
        let (key, digest) = provider_conversation_id?.rsplit_once(Self::DIGEST_SEPARATOR)?;

        Some(Self {
            input_digest: u64::from_str_radix(digest, 16).ok()?,
            key: key.to_string(),
        })
    }

    /// Encodes the provider conversation id for a turn whose input digest is
    /// `input_digest` and which carried the full contract.
    fn encode(session_id: &str, input_digest: u64) -> Option<String> {
        instruction_bootstrap_key(Some(session_id))
            .map(|key| format!("{key}{}{input_digest:016x}", Self::DIGEST_SEPARATOR))
    }
}

/// Settles the session's previous turn when its cleanup failed or its future
/// was dropped, so unresolved commands stop blocking admission.
///
/// # Errors
/// Returns an error when that cleanup still cannot be acknowledged.
async fn settle_previous_turn(active: &ActiveTurns, session_id: &str) -> Result<(), String> {
    let run = lock(active).get(session_id).cloned();
    if let Some(run) = run {
        run.settle().await?;
        lock(active).remove(session_id);
    }

    Ok(())
}

/// Locks the started-run registry; its map stays valid after a panic.
fn lock(active: &ActiveTurns) -> MutexGuard<'_, HashMap<String, ActiveRun>> {
    active.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Inputs needed to create, resume, or switch one durable harness session.
struct SessionOpener<'a> {
    compacts: bool,
    database: &'a Path,
    environment: &'a BTreeMap<String, String>,
    events: &'a mpsc::UnboundedSender<TurnEvent>,
    profile: ProtocolRequestProfile,
    provider_call_budget: Option<&'a ag_contracts::ProviderCallBudget>,
    reasoning_level: ReasoningLevel,
    repository: &'a Repository,
    running_context_window: Option<u64>,
    session_id: &'a str,
    summary_model: Option<&'a ModelSelection>,
}

impl SessionOpener<'_> {
    /// Resumes the stored session on its recorded model and switches to
    /// `selection` when it differs, or creates the session when it does not
    /// exist. Returns the session with the history the harness loads for it.
    async fn open(
        &self,
        selection: &ModelSelection,
    ) -> Result<(Session, LoadedHistory), NativeTurnError> {
        let harness = self.harness(selection)?;
        let session = match harness.resume(self.session_id).await {
            Ok(session) => session,
            Err(SessionError::RegistrationMismatch {
                stored: Some(identity),
                ..
            }) if let Some(stored_selection) = ModelSelection::from_key(identity.key())
                && stored_selection != *selection =>
            {
                self.switch(&stored_selection, selection).await?
            }
            Err(error) => {
                let error = HarnessError::from(error);
                if error.kind() != ErrorKind::NotFound {
                    return Err(NativeTurnError::session_operation(
                        "Failed to resume the harness session",
                    )(error));
                }
                let session = harness
                    .session(self.session_id, output_schema(self.profile)?)
                    .system_prompt(system_prompt())
                    .create()
                    .await
                    .map_err(NativeTurnError::session_operation(
                        "Failed to create the harness session",
                    ))?;

                return Ok((session, LoadedHistory::default()));
            }
        };
        let history = LoadedHistory::read(&session).await;

        Ok((session, history))
    }

    /// Resumes the session recorded on `stored_selection` and switches it to
    /// `selection`, needing only the target provider's credentials.
    async fn switch(
        &self,
        stored_selection: &ModelSelection,
        selection: &ModelSelection,
    ) -> Result<Session, NativeTurnError> {
        let registry = selection.registry(
            self.environment,
            self.provider_call_budget,
            self.running_context_window,
        )?;
        let harness = self.build_harness(|context, observer| {
            stored_selection.resume_only_harness(context, observer)
        })?;
        let mut session = self.resume(harness).await?;
        session
            .switch_model(&registry, &selection.key())
            .await
            .map_err(NativeTurnError::session_operation(
                "Failed to switch the harness model",
            ))?;
        // The switch drops the summary model the stored model had, so the
        // new model gets its own for this turn.
        self.context()
            .apply_summary_model(|summary| session.set_summary_model(summary));

        Ok(session)
    }

    /// Resumes the session with `harness`.
    async fn resume(&self, harness: Harness) -> Result<Session, NativeTurnError> {
        harness
            .resume(self.session_id)
            .await
            .map_err(NativeTurnError::session_operation(
                "Failed to resume the harness session",
            ))
    }

    /// Builds a harness for `selection` storing history in the session
    /// database.
    fn harness(&self, selection: &ModelSelection) -> Result<Harness, String> {
        self.build_harness(|context, observer| selection.harness(context, observer))
    }

    /// Builds a harness with this turn's context and activity forwarding,
    /// storing history in the session database.
    fn build_harness(
        &self,
        build: impl FnOnce(&HarnessContext<'_>, ActivityBridge) -> Result<Harness, String>,
    ) -> Result<Harness, String> {
        let events = self.events.clone();
        let harness = build(
            &self.context(),
            ActivityBridge::new(move |event| {
                let _ = events.send(event);
            }),
        )?;

        Ok(harness.database(self.database))
    }

    /// Returns this turn's harness context.
    fn context(&self) -> HarnessContext<'_> {
        HarnessContext {
            compacts: self.compacts,
            environment: self.environment,
            provider_call_budget: self.provider_call_budget,
            reasoning_level: self.reasoning_level,
            repository: self.repository,
            running_context_window: self.running_context_window,
            summary_model: self.summary_model,
        }
    }
}

/// Finished-turn state of the history the harness loads for a turn.
#[derive(Default)]
struct LoadedHistory {
    /// Whether the session holds any finished turn, loaded or not.
    has_finished_turn: bool,
    /// Input digests of the loaded turns, oldest first.
    input_digests: Vec<Option<u64>>,
}

impl LoadedHistory {
    /// Reads the turns `session` replays from its transcript, and whether
    /// it holds any finished turn when it replays none.
    ///
    /// The transcript decodes every stored field of those turns, which
    /// replay does not need. When it cannot, the resumed session is assumed
    /// to hold history whose bootstrap turn is not replayed, so the turn
    /// re-sends the full contract rather than failing on every later turn.
    async fn read(session: &Session) -> Self {
        let newest = match session.transcript(session.replayed_range()).await {
            Ok(turns) if !turns.is_empty() => return Self::new(true, &turns),
            Ok(_) => session.transcript(TurnRange::Last(1)).await,
            Err(error) => Err(error),
        };

        Self::new(!newest.is_ok_and(|newest| newest.is_empty()), &[])
    }

    /// Summarizes the replayed `turns`, oldest first.
    fn new(has_finished_turn: bool, turns: &[TurnRecord]) -> Self {
        let input_digests = turns
            .iter()
            .map(|turn| match turn.input.as_slice() {
                [TranscriptInput::Text { text }] => Some(input_digest(text)),
                _ => None,
            })
            .collect();

        Self {
            has_finished_turn,
            input_digests,
        }
    }

    /// Returns how many loaded turns, counted from the newest, reach the
    /// newest turn whose input has `digest`.
    fn age_of(&self, digest: u64) -> Option<usize> {
        self.input_digests
            .iter()
            .rev()
            .position(|input_digest| *input_digest == Some(digest))
            .map(|position| position + 1)
    }
}

/// Fingerprints one turn's rendered input text.
fn input_digest(input: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    input.hash(&mut hasher);

    hasher.finish()
}

#[cfg(test)]
#[path = "native_test.rs"]
mod tests;
