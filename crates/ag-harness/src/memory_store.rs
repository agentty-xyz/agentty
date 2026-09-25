//! Process-local session storage with atomic lifecycle and journal mutations.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::Instant;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelMetadata};
use crate::recovery::{HostRequest, HostTurnAcquisition, HostTurnRecord, HostTurnStatus};
use crate::reservation::TURN_LEASE_SECONDS;
use crate::store::{
    AcquiredTurn, LoadedSession, NewSession, SessionStore, StoreIdentity, StoredTurnOptions,
    TurnOwner, WriteRecord, WriteStatus,
};
use crate::write_journal::content_hash;
use crate::{SessionError, TurnError, TurnOptions, TurnOutcome};

/// In-memory session history and write journals, with no restart durability.
///
/// Clones share backing state and identity. Independently constructed stores
/// are isolated, even when their session identifiers match. Inject with
/// [`crate::Harness::store`]; one-shot turns never access this store.
/// Canonical records remain in memory for the backing state's lifetime; the
/// history budget limits replay, not total memory consumption.
#[derive(Clone)]
pub struct MemoryStore {
    identity: StoreIdentity,
    state: Arc<Mutex<State>>,
}

impl MemoryStore {
    /// Creates an empty store with a unique process-local backing identity.
    pub fn new() -> Self {
        Self {
            identity: StoreIdentity::unique(),
            state: Arc::default(),
        }
    }

    fn acquire(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: Option<&HostRequest>,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError> {
        if store.identity() != self.identity() {
            return Err(SessionError::InvalidData {
                reason: "acquisition store has a different backing identity".to_string(),
            });
        }
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        session.recover();
        if let Some(request) = request
            && let Some(record) = session.request(request.id())
        {
            record.check_request(request)?;

            return Ok(HostTurnAcquisition::Recorded(record));
        }
        crate::session_model::check_generation(
            session_id,
            session.configuration.model_generation,
            generation,
        )?;
        if session
            .turns
            .last()
            .is_some_and(|turn| turn.status == Status::Running)
        {
            return Err(SessionError::Busy {
                id: session_id.to_string(),
            });
        }
        if session
            .commands
            .iter()
            .any(crate::bash::CommandRecord::blocks_admission)
        {
            return Err(SessionError::Busy {
                id: session_id.into(),
            });
        }
        let previous = session
            .turns
            .iter()
            .rev()
            .find(|turn| turn.status == Status::Completed);
        let compatible = previous
            .map(|turn| StoredTurnOptions::decode(&turn.options))
            .transpose()?
            .is_some_and(|snapshot| snapshot.continuation_compatible(options));
        let continuation = session
            .configuration
            .provider_session_id
            .clone()
            .filter(|_| compatible);
        let position = session.next_turn;
        let next_position = position
            .checked_add(1)
            .ok_or_else(|| SessionError::InvalidData {
                reason: "session turn position exceeds integer range".to_string(),
            })?;
        let owner = TurnOwner::new(
            self.identity.clone(),
            session_id.to_string(),
            position,
            position.to_le_bytes().to_vec(),
        );
        let deadline = Instant::now() + Duration::from_secs(TURN_LEASE_SECONDS.unsigned_abs());
        let acquired = AcquiredTurn::new(
            store,
            owner.clone(),
            deadline,
            session.history(),
            continuation.clone(),
        )?
        .with_checkpoint(session.configuration.checkpoint.clone());
        session.turns.push(TurnRecord {
            model: session.configuration.recorded_model(),
            deadline,
            error_type: None,
            messages: vec![input.clone().into_user_message()],
            options: StoredTurnOptions::encode(options),
            outcome: None,
            owner,
            request: request.cloned(),
            status: Status::Running,
        });
        session.next_turn = next_position;
        session.configuration.provider_session_id = continuation;
        drop(state);

        acquired.activate().map(HostTurnAcquisition::Acquired)
    }

    fn complete(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
        outcome: Option<&TurnOutcome>,
    ) -> Result<(), SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let session = state.owned_session(owner)?;
        let turn = session.live_turn(owner)?;
        if turn.request.is_some() && outcome.is_none() {
            return Err(SessionError::InvalidData {
                reason: "host turn completion requires its terminal outcome".into(),
            });
        }
        turn.outcome = outcome.cloned();
        turn.messages.extend_from_slice(messages);
        turn.status = Status::Completed;
        session.configuration.provider_session_id = provider_session_id.map(str::to_string);

        Ok(())
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn validate_identity(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        if owner.store_identity() != &self.identity {
            return Err(lost(owner));
        }

        Ok(())
    }
}

impl Default for MemoryStore {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl SessionStore for MemoryStore {
    async fn load_commands(
        &self,
        session_id: &str,
    ) -> Result<Vec<crate::bash::CommandRecord>, SessionError> {
        self.lock()
            .sessions
            .get(session_id)
            .map(|session| session.commands.clone())
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.into(),
            })
    }

    async fn command_intent(
        &self,
        owner: &TurnOwner,
        intent: &crate::bash::CommandIntent,
    ) -> Result<i64, SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let id = state
            .next_command
            .checked_add(1)
            .ok_or_else(|| lost(owner))?;
        state.next_command = id;
        let session = state.owned_session(owner)?;
        session.live_turn(owner)?;
        session.commands.push(crate::bash::CommandRecord::pending(
            id,
            owner.clone(),
            intent.clone(),
        ));

        Ok(id)
    }

    async fn finish_command(
        &self,
        owner: &TurnOwner,
        id: i64,
        outcome: &crate::bash::CommandOutcome,
    ) -> Result<(), SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let session = state.owned_session(owner)?;
        let command = session
            .commands
            .iter_mut()
            .find(|command| command.id == id && command.owner == *owner)
            .ok_or_else(|| lost(owner))?;
        if command
            .outcome
            .as_ref()
            .is_some_and(|previous| previous != outcome)
        {
            return Err(lost(owner));
        }
        command.outcome = Some(outcome.clone());

        Ok(())
    }

    async fn reconcile_command(&self, owner: &TurnOwner, id: i64) -> Result<(), SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let session = state.owned_session(owner)?;
        session.recover();
        if session
            .turns
            .iter()
            .any(|turn| turn.owner == *owner && turn.status == Status::Running)
        {
            return Err(lost(owner));
        }
        let command = session
            .commands
            .iter_mut()
            .find(|command| command.id == id && command.owner == *owner)
            .ok_or_else(|| lost(owner))?;
        command.reconciled = true;

        Ok(())
    }

    fn identity(&self) -> &StoreIdentity {
        &self.identity
    }

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError> {
        if config.id().trim().is_empty() {
            return Err(SessionError::InvalidData {
                reason: "session identifier must not be empty".to_string(),
            });
        }
        let mut state = self.lock();
        if state.sessions.contains_key(config.id()) {
            return Err(SessionError::AlreadyExists {
                id: config.id().to_string(),
            });
        }
        state.sessions.insert(
            config.id().to_string(),
            Record {
                commands: Vec::new(),
                configuration: LoadedSession {
                    checkpoint: None,
                    latest_completed_turn: None,
                    model_generation: 0,
                    max_history_bytes,
                    model: metadata.as_ref().map(|value| value.model().to_string()),
                    provider: metadata.as_ref().map(|value| value.provider().to_string()),
                    provider_session_id: None,
                    registration_identity: config.registration_identity().cloned(),
                    schema: config.schema().clone(),
                    system_prompt: config.system_prompt().map(str::to_string),
                    turns: Vec::new(),
                },
                next_turn: 0,
                turns: Vec::new(),
                writes: Vec::new(),
            },
        );

        Ok(())
    }

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
        let mut loaded = session.configuration.clone();
        loaded.latest_completed_turn = session.latest_completed_turn();
        loaded.turns = session.history();

        Ok(loaded)
    }

    async fn publish_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &crate::store::SessionCheckpoint,
    ) -> Result<(), SessionError> {
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        session.recover();
        let outdated = session.configuration.model_generation != checkpoint.model_generation()
            || session
                .latest_completed_turn()
                .is_none_or(|completed| checkpoint.covered_through() > completed)
            || session
                .configuration
                .checkpoint
                .as_ref()
                .is_some_and(|existing| existing.covered_through() > checkpoint.covered_through());
        if outdated {
            return Err(SessionError::CheckpointStale {
                id: session_id.to_string(),
            });
        }
        session.configuration.checkpoint = Some(checkpoint.clone());

        Ok(())
    }

    async fn switch_model(
        &self,
        id: &str,
        generation: i64,
        identity: &crate::recovery::ExecutionIdentity,
        metadata: Option<ModelMetadata>,
        capabilities: crate::model::ModelCapabilities,
    ) -> Result<i64, SessionError> {
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
        crate::session_model::check_generation(
            id,
            session.configuration.model_generation,
            generation,
        )?;
        if session
            .turns
            .last()
            .is_some_and(|turn| turn.status == Status::Running)
            || session
                .commands
                .iter()
                .any(crate::bash::CommandRecord::blocks_admission)
        {
            return Err(SessionError::Busy { id: id.to_string() });
        }
        crate::session_model::check_history(
            session
                .turns
                .iter()
                .filter(|turn| turn.status == Status::Completed)
                .flat_map(|turn| turn.messages.iter()),
            capabilities,
        )?;
        let next = crate::session_model::next_generation(generation)?;
        session.configuration.model_generation = next;
        session.configuration.registration_identity = Some(identity.clone());
        session.configuration.provider =
            metadata.as_ref().map(|value| value.provider().to_string());
        session.configuration.model = metadata.as_ref().map(|value| value.model().to_string());
        session.configuration.provider_session_id = None;

        Ok(next)
    }

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        generation: i64,
    ) -> Result<AcquiredTurn, SessionError> {
        let HostTurnAcquisition::Acquired(acquired) =
            self.acquire(store, session_id, input, options, None, generation)?
        else {
            return Err(SessionError::HostTurnConflict);
        };

        Ok(acquired)
    }

    async fn begin_request(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: &HostRequest,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError> {
        self.acquire(store, session_id, input, options, Some(request), generation)
    }

    async fn load_request(
        &self,
        session_id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        let mut state = self.lock();
        let session = state
            .sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        session.recover();

        Ok(session.request(host_id))
    }

    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        self.complete(owner, messages, continuation, Some(outcome))
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let turn = state.owned_session(owner)?.live_turn(owner)?;
        turn.deadline = Instant::now() + Duration::from_secs(TURN_LEASE_SECONDS.unsigned_abs());

        Ok(turn.deadline)
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
    ) -> Result<(), SessionError> {
        self.complete(owner, messages, continuation, None)
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let session = state.owned_session(owner)?;
        let turn = session.live_turn(owner)?;
        turn.status = Status::Failed;
        turn.error_type = Some(format!("{:?}", error.error_type()));
        session.configuration.provider_session_id = None;

        Ok(())
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        if let Some(session) = state.sessions.get_mut(owner.session_id())
            && let Some(turn) = session.turns.last_mut()
            && turn.owner == *owner
            && turn.status == Status::Running
        {
            turn.status = Status::Interrupted;
            turn.error_type = Some(owner.interruption_error_type().to_string());
            session.configuration.provider_session_id = None;
        }

        Ok(())
    }

    async fn load_writes(&self, session_id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        Ok(self
            .lock()
            .sessions
            .get(session_id)
            .map_or_else(Vec::new, |session| session.writes.clone()))
    }

    async fn write_intent(
        &self,
        owner: &TurnOwner,
        call_id: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let id = state
            .next_write
            .checked_add(1)
            .ok_or_else(|| SessionError::InvalidData {
                reason: "write identifier exceeds integer range".to_string(),
            })?;
        let session = state.owned_session(owner)?;
        session.live_turn(owner)?;
        session.writes.push(WriteRecord {
            call_id: call_id.to_string(),
            expected_hash: expected.map(content_hash),
            id,
            path: path.to_string(),
            repository_root: root.to_path_buf(),
            resulting_hash: content_hash(resulting),
            status: WriteStatus::Pending,
            turn_position: owner.turn_position(),
        });
        state.next_write = id;

        Ok(id)
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        self.validate_identity(owner)?;
        let mut state = self.lock();
        let session = state.owned_session(owner)?;
        if !session.turns.iter().any(|turn| turn.owner == *owner) {
            return Err(lost(owner));
        }
        let status = if applied {
            WriteStatus::Applied
        } else {
            WriteStatus::Failed
        };
        let record = session
            .writes
            .iter_mut()
            .find(|record| {
                record.id == id
                    && record.turn_position == owner.turn_position()
                    && (record.status == WriteStatus::Pending || record.status == status)
            })
            .ok_or_else(|| lost(owner))?;
        record.status = status;

        Ok(())
    }
}

#[derive(Default)]
struct State {
    next_command: i64,
    next_write: i64,
    sessions: HashMap<String, Record>,
}

impl State {
    fn owned_session(&mut self, owner: &TurnOwner) -> Result<&mut Record, SessionError> {
        self.sessions
            .get_mut(owner.session_id())
            .ok_or_else(|| lost(owner))
    }
}

struct Record {
    commands: Vec<crate::bash::CommandRecord>,
    configuration: LoadedSession,
    next_turn: i64,
    turns: Vec<TurnRecord>,
    writes: Vec<WriteRecord>,
}

impl Record {
    fn request(&self, host_id: &str) -> Option<HostTurnRecord> {
        let turn = self.turns.iter().find(|turn| {
            turn.request
                .as_ref()
                .is_some_and(|request| request.id() == host_id)
        })?;
        let request = turn.request.clone()?;
        let status = match turn.status {
            Status::Running => HostTurnStatus::InProgress,
            Status::Completed => HostTurnStatus::Completed(turn.outcome.clone()?),
            Status::Failed => HostTurnStatus::Failed {
                error_type: turn.error_type.clone().unwrap_or_default(),
            },
            Status::Interrupted => HostTurnStatus::Interrupted {
                error_type: turn.error_type.clone().unwrap_or_default(),
            },
        };
        let turn_position = turn.owner.turn_position();

        Some(HostTurnRecord {
            commands: self
                .commands
                .iter()
                .filter(|command| command.owner.turn_position() == turn_position)
                .cloned()
                .collect(),
            model: Some(turn.model.clone()),
            request,
            status,
            turn_position,
            writes: self
                .writes
                .iter()
                .filter(|write| write.turn_position == turn_position)
                .cloned()
                .collect(),
        })
    }

    fn recover(&mut self) {
        if let Some(turn) = self.turns.last_mut()
            && turn.status == Status::Running
            && turn.deadline <= Instant::now()
        {
            turn.status = Status::Interrupted;
            turn.error_type = Some("interrupted".to_string());
            self.configuration.provider_session_id = None;
        }
    }

    fn latest_completed_turn(&self) -> Option<i64> {
        self.turns
            .iter()
            .filter(|turn| turn.status == Status::Completed)
            .map(|turn| turn.owner.turn_position())
            .max()
    }

    fn history(&self) -> Vec<Vec<ModelMessage>> {
        let boundary = self
            .configuration
            .checkpoint
            .as_ref()
            .map_or(-1, crate::store::SessionCheckpoint::covered_through);
        let mut remaining = self.configuration.max_history_bytes;
        let mut turns = Vec::new();
        for turn in self.turns.iter().rev().filter(|turn| {
            turn.status == Status::Completed && turn.owner.turn_position() > boundary
        }) {
            let bytes = turn.messages.iter().fold(0_usize, |bytes, message| {
                bytes.saturating_add(message.retained_bytes())
            });
            if bytes > remaining {
                break;
            }
            remaining -= bytes;
            turns.push(turn.messages.clone());
        }
        turns.reverse();

        turns
    }

    fn live_turn(&mut self, owner: &TurnOwner) -> Result<&mut TurnRecord, SessionError> {
        self.turns
            .last_mut()
            .filter(|turn| {
                turn.owner == *owner
                    && turn.status == Status::Running
                    && turn.deadline > Instant::now()
            })
            .ok_or_else(|| lost(owner))
    }
}

struct TurnRecord {
    deadline: Instant,
    error_type: Option<String>,
    messages: Vec<ModelMessage>,
    model: crate::store::RecordedModel,
    options: String,
    outcome: Option<TurnOutcome>,
    owner: TurnOwner,
    request: Option<HostRequest>,
    status: Status,
}

#[derive(Eq, PartialEq)]
enum Status {
    Running,
    Completed,
    Failed,
    Interrupted,
}

fn lost(owner: &TurnOwner) -> SessionError {
    SessionError::OwnershipLost {
        id: owner.session_id().to_string(),
        turn_position: owner.turn_position(),
    }
}

#[cfg(test)]
#[path = "memory_store_test.rs"]
mod tests;
