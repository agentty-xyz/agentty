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
use std::hash::{DefaultHasher, Hash, Hasher};
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use ag_contracts::{
    AgentChannel, AgentError, AgentFuture, ReasoningLevel, SessionRef, StartSessionRequest,
    TurnEvent, TurnRequest, TurnResult,
};
use ag_harness::model::ModelMessage;
use ag_harness::store::{HistoryTurn, LoadedSession, SessionStore, SqliteStore};
use ag_harness::{Harness, Repository, Session, SessionError, TurnControl};
use ag_protocol::ProtocolRequestProfile;
use ag_session::AgentKind;
use tokio::sync::mpsc;

use crate::agent::native::{
    ActivityBridge, HarnessContext, ModelSelection, NativeHarnessConfig, NativePrompt, failure,
    output_schema, parse_outcome, reject_attachments, repository, settle, system_prompt,
    token_usage, turn_options,
};
use crate::agent::{
    InstructionDeliveryMode, apply_response_style_prompt, execution_policy,
    instruction_bootstrap_key, plan_instruction_delivery,
};

/// Started turns whose settlement a shutdown or the session's next turn must
/// still observe, by session.
type ActiveTurns = Arc<Mutex<HashMap<String, TurnControl>>>;

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
    /// Returns [`AgentError::Backend`] for unsupported controls,
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
                .map_err(AgentError::Backend)
        })
    }

    /// Cancels the session's started turn and waits for its cleanup.
    ///
    /// A failed or abandoned cleanup keeps the turn registered so the
    /// session's next turn retries it.
    fn shutdown_session(&self, session_id: String) -> AgentFuture<Result<(), AgentError>> {
        let active = Arc::clone(&self.active);

        Box::pin(async move {
            let control = lock(&active).get(&session_id).cloned();
            if let Some(control) = control {
                control.cancel();
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
) -> Result<TurnResult, String> {
    execution_policy::validate(AgentKind::Harness, &req.execution_policy)
        .map_err(|error| error.to_string())?;
    let environment = config.environment();
    let selection = ModelSelection::resolve(&req.model)?;
    let profile = req.request_kind.protocol_profile();
    let options = turn_options(profile, req.permission_mode, &environment)?;
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
    let database = config.session_database(&session_id).await?;
    let opener = SessionOpener {
        database: &database,
        environment: &environment,
        events: &events,
        profile,
        reasoning_level: req.reasoning_level,
        repository: &repository,
        session_id: &session_id,
    };
    let (mut session, history) = opener.open(&selection).await?;
    // Turns since the last full contract, counting its own turn; `None` once
    // the harness no longer loads it.
    let bootstrap_age = bootstrap
        .as_ref()
        .and_then(|bootstrap| history.age_of(bootstrap.input_digest));
    let delivery = plan_instruction_delivery(
        &req.request_kind,
        bootstrap_age.is_some().then_some(session_id.as_str()),
        bootstrap.as_ref().map(|bootstrap| bootstrap.key.as_str()),
        !history.has_finished_turn && replay_transcript.is_some() && req.request_kind.is_resume(),
    );
    let input = NativePrompt {
        delivery,
        personality_prompt: req.personality.current(),
        personality_update: req.personality.update(),
        prompt: &prompt_text,
        protocol_profile: profile,
        replay_transcript: replay_transcript.as_deref(),
        workspace_root: &req.folder,
    }
    .render()?;
    let input_digest = input_digest(&input);
    let turn = session.turn(input).options(options).start();
    let control = turn.control();
    lock(active).insert(session_id.clone(), control.clone());
    let outcome = turn.await;
    // A failed settlement stays registered so the next turn retries it.
    settle(&control).await?;
    lock(active).remove(&session_id);
    let outcome = outcome.map_err(|error| format!("Harness turn failed: {error}"))?;
    let assistant_message = parse_outcome(&outcome, profile)?;
    let (input_tokens, output_tokens) = token_usage(outcome.report());
    // An unversioned id makes the next session turn re-send the session
    // contract: a utility or review turn bootstrapped its own, or the
    // context budget dropped the turn that carried it from this request.
    let bootstrap_replayed =
        bootstrap_age.is_some_and(|age| outcome.report().history().replayed_turns() >= age);
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
        input_tokens,
        output_tokens,
        provider_conversation_id,
    })
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
    let control = lock(active).get(session_id).cloned();
    if let Some(control) = control {
        settle(&control).await?;
        lock(active).remove(session_id);
    }

    Ok(())
}

/// Locks the started-turn registry; its map stays valid after a panic.
fn lock(active: &ActiveTurns) -> MutexGuard<'_, HashMap<String, TurnControl>> {
    active.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Inputs needed to create, resume, or switch one durable harness session.
struct SessionOpener<'a> {
    database: &'a Path,
    environment: &'a BTreeMap<String, String>,
    events: &'a mpsc::UnboundedSender<TurnEvent>,
    profile: ProtocolRequestProfile,
    reasoning_level: ReasoningLevel,
    repository: &'a Repository,
    session_id: &'a str,
}

impl SessionOpener<'_> {
    /// Resumes the stored session on its recorded model and switches to
    /// `selection` when it differs, or creates the session when it does not
    /// exist. Returns the session with the history the harness loads for it.
    async fn open(&self, selection: &ModelSelection) -> Result<(Session, LoadedHistory), String> {
        let loaded = match self.load().await {
            Ok(loaded) => loaded,
            Err(SessionError::NotFound { .. }) => {
                let session = self
                    .harness(selection)?
                    .session(self.session_id, output_schema(self.profile)?)
                    .system_prompt(system_prompt())
                    .create()
                    .await
                    .map_err(failure("Failed to create the harness session"))?;

                return Ok((session, LoadedHistory::default()));
            }
            Err(error) => return Err(failure("Failed to load the harness session")(error)),
        };
        let history = LoadedHistory::new(loaded.latest_finished_turn.is_some(), &loaded.turns);
        let stored_selection = loaded
            .registration_identity
            .as_ref()
            .and_then(|identity| ModelSelection::from_key(identity.key()));
        let session = match stored_selection {
            Some(stored_selection) if stored_selection != *selection => {
                self.switch(&stored_selection, selection).await?
            }
            _ => self.resume(self.harness(selection)?).await?,
        };

        Ok((session, history))
    }

    /// Loads the stored session's model identity and finished-turn state.
    async fn load(&self) -> Result<LoadedSession, SessionError> {
        SqliteStore::open(self.database)
            .await?
            .load_session(self.session_id)
            .await
    }

    /// Resumes the session recorded on `stored_selection` and switches it to
    /// `selection`, needing only the target provider's credentials.
    async fn switch(
        &self,
        stored_selection: &ModelSelection,
        selection: &ModelSelection,
    ) -> Result<Session, String> {
        let registry = selection.registry(self.environment, None)?;
        let harness = self.build_harness(|context, observer| {
            stored_selection.resume_only_harness(context, observer)
        })?;
        let mut session = self.resume(harness).await?;
        session
            .switch_model(&registry, &selection.key())
            .await
            .map_err(failure("Failed to switch the harness model"))?;

        Ok(session)
    }

    /// Resumes the session with `harness`.
    async fn resume(&self, harness: Harness) -> Result<Session, String> {
        harness
            .resume(self.session_id)
            .await
            .map_err(failure("Failed to resume the harness session"))
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
            &HarnessContext {
                environment: self.environment,
                provider_call_budget: None,
                reasoning_level: self.reasoning_level,
                repository: self.repository,
            },
            ActivityBridge::new(move |event| {
                let _ = events.send(event);
            }),
        )?;

        Ok(harness.database(self.database))
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
    /// Summarizes the turns the store loads within the session's byte budget.
    fn new(has_finished_turn: bool, turns: &[HistoryTurn]) -> Self {
        let input_digests = turns
            .iter()
            .map(|turn| match turn.messages.first() {
                Some(ModelMessage::User(text)) => Some(input_digest(text)),
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
