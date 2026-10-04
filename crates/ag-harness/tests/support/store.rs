//! Independent, test-only backend implemented exclusively through public APIs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use ag_harness::model::{ModelMessage, ModelMetadata};
use ag_harness::recovery::{HostTurnRecord, HostTurnStatus};
use ag_harness::store::{
    Admission, AdmissionState, LoadedSession, ModelSwitch, NewSession, RejectedContent,
    Reservation, ReservedTurn, SessionCheckpoint, SessionStore, StoppedTurn, StoreIdentity,
    TurnAdmission, TurnOwner, WriteRecord, WriteStatus,
};
use ag_harness::{SessionError, TurnError, TurnOutcome};
use async_trait::async_trait;
use sha2::{Digest as _, Sha256};
use tokio::sync::Notify;
use tokio::time::Instant;

pub(crate) struct ExternalStore {
    pub(crate) renewed: Notify,
    pub(crate) renewals: AtomicUsize,
    identity: StoreIdentity,
    initial_lease: Duration,
    renewal_lease: Duration,
    sessions: Mutex<HashMap<String, Record>>,
}

struct Record {
    loaded: LoadedSession,
    next_turn: i64,
    notes: Vec<Option<ModelMessage>>,
    owner: Option<(TurnOwner, Instant)>,
    pending: Vec<ModelMessage>,
    positions: Vec<i64>,
    requests: Vec<HostTurnRecord>,
    snapshot: Option<String>,
    writes: Vec<(Vec<u8>, WriteRecord)>,
}

impl Record {
    fn admission_state(&self, host_id: Option<&str>) -> AdmissionState {
        AdmissionState {
            active_turn: self.owner.is_some(),
            latest_options: self.snapshot.clone(),
            model_generation: self.loaded.model_generation,
            provider_session_id: self.loaded.provider_session_id.clone(),
            request: host_id.and_then(|host_id| self.request(host_id)),
            unresolved_commands: false,
        }
    }

    fn set_status(&mut self, owner: &TurnOwner, status: HostTurnStatus) {
        if let Some(record) = self
            .requests
            .iter_mut()
            .find(|record| record.turn_position == owner.turn_position())
        {
            record.status = status;
        }
    }

    fn request(&self, id: &str) -> Option<HostTurnRecord> {
        let mut record = self
            .requests
            .iter()
            .find(|record| record.request.id() == id)?
            .clone();
        record.writes = self
            .writes
            .iter()
            .filter(|(_, write)| write.turn_position == record.turn_position)
            .map(|(_, write)| write.clone())
            .collect();

        Some(record)
    }

    fn bounded(&self) -> LoadedSession {
        let mut loaded = self.loaded.clone();
        let boundary = loaded
            .checkpoint
            .as_ref()
            .map_or(-1, SessionCheckpoint::covered_through);
        loaded.latest_replayable_turn = self.positions.iter().copied().max();
        let bytes =
            |turn: &[ModelMessage]| turn.iter().map(ModelMessage::retained_bytes).sum::<usize>();
        let mut remaining = loaded.max_history_bytes;
        let mut turns = Vec::new();
        for (turn, note) in self
            .positions
            .iter()
            .zip(loaded.turns.iter().zip(&self.notes))
            .filter(|(position, _)| **position > boundary)
            .map(|(_, turn)| turn)
            .rev()
        {
            // A stopped turn that does not fit whole replays only its input
            // and note; the note counts against the budget.
            let note_bytes = note.as_ref().map_or(0, ModelMessage::retained_bytes);
            let mut turn = if bytes(turn) + note_bytes <= remaining {
                turn.clone()
            } else if note.is_some() && bytes(&turn[..1]) + note_bytes <= remaining {
                turn[..1].to_vec()
            } else {
                break;
            };
            remaining -= bytes(&turn) + note_bytes;
            turn.extend(note.clone());
            turns.push(turn);
        }
        turns.reverse();
        loaded.turns = turns;

        loaded
    }

    fn recover(&mut self) {
        if let Some((owner, deadline)) = self.owner.clone()
            && deadline <= Instant::now()
        {
            self.stop(&owner, StoppedTurn::Interrupted, "interrupted".into());
        }
    }

    /// Retains a stopped turn's input and progress for replay with its note.
    fn stop(&mut self, owner: &TurnOwner, stopped: StoppedTurn, error_type: String) {
        self.loaded.turns.push(std::mem::take(&mut self.pending));
        self.notes.push(Some(stopped.note(&error_type)));
        self.positions.push(owner.turn_position());
        self.set_status(
            owner,
            match stopped {
                StoppedTurn::Failed => HostTurnStatus::Failed { error_type },
                StoppedTurn::Interrupted => HostTurnStatus::Interrupted { error_type },
            },
        );
        self.owner = None;
        self.loaded.provider_session_id = None;
    }

    fn validate(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        if self
            .owner
            .as_ref()
            .is_some_and(|(active, deadline)| active == owner && *deadline > Instant::now())
        {
            return Ok(());
        }

        Err(SessionError::OwnershipLost {
            id: owner.session_id().to_string(),
            turn_position: owner.turn_position(),
        })
    }
}

impl ExternalStore {
    fn complete(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: Option<&TurnOutcome>,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        session.pending.truncate(1);
        session.pending.extend_from_slice(messages);
        session
            .loaded
            .turns
            .push(std::mem::take(&mut session.pending));
        session.notes.push(None);
        session.positions.push(owner.turn_position());
        if let Some(outcome) = outcome {
            session.set_status(owner, HostTurnStatus::Completed(outcome.clone()));
        }
        session.owner = None;
        session.loaded.provider_session_id = continuation.map(str::to_string);

        Ok(())
    }

    pub(crate) fn new() -> Self {
        Self::with_leases(Duration::from_secs(300), Duration::from_secs(300))
    }

    pub(crate) fn with_leases(initial_lease: Duration, renewal_lease: Duration) -> Self {
        Self {
            renewed: Notify::new(),
            renewals: AtomicUsize::new(0),
            identity: StoreIdentity::unique(),
            initial_lease,
            renewal_lease,
            sessions: Mutex::default(),
        }
    }
}

#[async_trait]
impl SessionStore for ExternalStore {
    fn identity(&self) -> &StoreIdentity {
        &self.identity
    }

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        if sessions.contains_key(config.id()) {
            return Err(SessionError::AlreadyExists {
                id: config.id().to_string(),
            });
        }
        sessions.insert(
            config.id().to_string(),
            Record {
                loaded: LoadedSession {
                    checkpoint: None,
                    latest_replayable_turn: None,
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
                notes: Vec::new(),
                owner: None,
                pending: Vec::new(),
                positions: Vec::new(),
                requests: Vec::new(),
                snapshot: None,
                writes: Vec::new(),
            },
        );

        Ok(())
    }

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();

        Ok(session.bounded())
    }

    async fn switch_model(&self, id: &str, switch: &ModelSwitch) -> Result<i64, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
        switch.check_history(session.loaded.turns.iter().flatten())?;
        let next = switch.admit(id, &session.admission_state(None))?;
        session.loaded.model_generation = next;
        session.loaded.registration_identity = Some(switch.identity().clone());
        session.loaded.provider = switch.metadata().map(|value| value.provider().to_string());
        session.loaded.model = switch.metadata().map(|value| value.model().to_string());
        session.loaded.provider_session_id = None;
        Ok(next)
    }

    async fn reserve_turn(
        &self,
        id: &str,
        admission: &TurnAdmission,
    ) -> Result<Reservation, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
        let turn = match admission.admit(id, session.admission_state(admission.host_id()))? {
            Admission::Recorded(record) => return Ok(Reservation::Recorded(record)),
            Admission::Reserve(turn) => turn,
        };
        let position = session.next_turn;
        let owner = TurnOwner::new(
            self.identity.clone(),
            id.to_string(),
            position,
            position.to_le_bytes().to_vec(),
        );
        let deadline = Instant::now() + self.initial_lease;
        if let Some(request) = turn.request() {
            session.requests.push(HostTurnRecord {
                commands: Vec::new(),
                model: Some(session.loaded.recorded_model()),
                request: request.clone(),
                status: HostTurnStatus::InProgress,
                turn_position: position,
                writes: Vec::new(),
            });
        }
        let bounded = session.bounded();
        session.next_turn += 1;
        session.owner = Some((owner.clone(), deadline));
        session.pending = vec![turn.message().clone()];
        session.snapshot = Some(turn.options().to_string());
        session.loaded.provider_session_id = turn.continuation().map(str::to_string);

        Ok(Reservation::Reserved(
            ReservedTurn::new(turn, owner, deadline, bounded.turns)
                .with_checkpoint(bounded.checkpoint),
        ))
    }

    async fn publish_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &SessionCheckpoint,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(session_id)
            .ok_or_else(|| SessionError::NotFound {
                id: session_id.to_string(),
            })?;
        session.recover();
        let latest_replayable = session.positions.iter().copied().max();
        let stale = session.loaded.model_generation != checkpoint.model_generation()
            || latest_replayable
                .is_none_or(|latest_replayable| checkpoint.covered_through() > latest_replayable)
            || session
                .loaded
                .checkpoint
                .as_ref()
                .is_some_and(|existing| existing.covered_through() > checkpoint.covered_through());
        if stale {
            return Err(SessionError::CheckpointStale {
                id: session_id.to_string(),
            });
        }
        session.loaded.checkpoint = Some(checkpoint.clone());

        Ok(())
    }

    async fn load_request(
        &self,
        id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.into() })?;
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
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        let deadline = Instant::now() + self.renewal_lease;
        session.owner = Some((owner.clone(), deadline));
        self.renewals.fetch_add(1, Ordering::SeqCst);
        self.renewed.notify_one();

        Ok(deadline)
    }

    async fn record_progress(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        session.pending.extend_from_slice(messages);

        Ok(())
    }

    async fn omit_rejected(
        &self,
        owner: &TurnOwner,
        rejected: RejectedContent,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        rejected.omit(&mut session.pending);

        Ok(())
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
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        session.stop(
            owner,
            StoppedTurn::Failed,
            format!("{:?}", error.error_type()),
        );

        Ok(())
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        if let Some(session) = sessions.get_mut(owner.session_id())
            && session
                .owner
                .as_ref()
                .is_some_and(|(active, _)| active == owner)
        {
            session.stop(
                owner,
                StoppedTurn::Interrupted,
                owner.interruption_error_type().into(),
            );
        }

        Ok(())
    }

    async fn load_writes(&self, id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        let sessions = self.sessions.lock().expect("sessions");

        Ok(sessions
            .get(id)
            .expect("session")
            .writes
            .iter()
            .map(|(_, record)| record.clone())
            .collect())
    }

    async fn write_intent(
        &self,
        owner: &TurnOwner,
        call: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        let id = i64::try_from(session.writes.len()).expect("id");
        session.writes.push((
            owner.token().to_vec(),
            WriteRecord {
                id,
                call_id: call.to_string(),
                expected_hash: expected.map(|bytes| hex::encode(Sha256::digest(bytes))),
                resulting_hash: hex::encode(Sha256::digest(resulting)),
                path: path.to_string(),
                repository_root: root.to_path_buf(),
                status: WriteStatus::Pending,
                turn_position: owner.turn_position(),
            },
        ));

        Ok(id)
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        let status = if applied {
            WriteStatus::Applied
        } else {
            WriteStatus::Failed
        };
        let record = session
            .writes
            .iter_mut()
            .find(|(token, record)| {
                token == owner.token()
                    && record.id == id
                    && record.turn_position == owner.turn_position()
                    && owner.store_identity() == &self.identity
                    && (record.status == WriteStatus::Pending || record.status == status)
            })
            .ok_or_else(|| SessionError::OwnershipLost {
                id: owner.session_id().to_string(),
                turn_position: owner.turn_position(),
            })?;
        record.1.status = status;

        Ok(())
    }
}
