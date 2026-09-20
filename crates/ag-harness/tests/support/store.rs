//! Independent, test-only backend implemented exclusively through public APIs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::{
    AcquiredTurn, HostRequest, HostTurnAcquisition, HostTurnRecord, HostTurnStatus, LoadedSession,
    ModelMessage, ModelMetadata, NewSession, SessionError, SessionStore, StoreIdentity,
    StoredTurnOptions, TurnError, TurnInput, TurnOptions, TurnOutcome, TurnOwner, WriteRecord,
    WriteStatus,
};
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
    owner: Option<(TurnOwner, Instant)>,
    pending: Vec<ModelMessage>,
    requests: Vec<HostTurnRecord>,
    snapshot: Option<String>,
    writes: Vec<(Vec<u8>, WriteRecord)>,
}

impl Record {
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
        while loaded
            .turns
            .iter()
            .flatten()
            .map(ModelMessage::retained_bytes)
            .sum::<usize>()
            > loaded.max_history_bytes
        {
            loaded.turns.remove(0);
        }

        loaded
    }

    fn recover(&mut self) {
        if self
            .owner
            .as_ref()
            .is_some_and(|(_, deadline)| *deadline <= Instant::now())
        {
            if let Some((owner, _)) = self.owner.clone() {
                self.set_status(
                    &owner,
                    HostTurnStatus::Interrupted {
                        error_type: "interrupted".into(),
                    },
                );
            }
            self.owner = None;
            self.loaded.provider_session_id = None;
        }
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
    fn acquire(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: Option<&HostRequest>,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
        if let Some(request) = request
            && let Some(record) = session.request(request.id())
        {
            record.check_request(request)?;

            return Ok(HostTurnAcquisition::Recorded(record));
        }
        if session.loaded.model_generation != generation {
            return Err(SessionError::StaleModel { id: id.to_string() });
        }
        if session.owner.is_some() {
            return Err(SessionError::Busy { id: id.to_string() });
        }
        let compatible = session
            .snapshot
            .as_deref()
            .map(StoredTurnOptions::decode)
            .transpose()?
            .is_some_and(|previous| previous.continuation_compatible(options));
        let continuation = session
            .loaded
            .provider_session_id
            .clone()
            .filter(|_| compatible);
        let position = session.next_turn;
        let owner = TurnOwner::new(
            self.identity.clone(),
            id.to_string(),
            position,
            position.to_le_bytes().to_vec(),
        );
        let deadline = Instant::now() + self.initial_lease;
        let acquired = AcquiredTurn::new(
            store,
            owner.clone(),
            deadline,
            session.bounded().turns,
            continuation.clone(),
        )?;
        if let Some(request) = request {
            session.requests.push(HostTurnRecord {
                commands: Vec::new(),
                model: Some(session.loaded.recorded_model()),
                request: request.clone(),
                status: HostTurnStatus::InProgress,
                turn_position: position,
                writes: Vec::new(),
            });
        }
        session.next_turn += 1;
        session.owner = Some((owner, deadline));
        session.pending = vec![input.clone().into_user_message()];
        session.snapshot = Some(StoredTurnOptions::encode(options));
        session.loaded.provider_session_id = continuation;

        acquired.activate().map(HostTurnAcquisition::Acquired)
    }

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
        session.pending.extend_from_slice(messages);
        session
            .loaded
            .turns
            .push(std::mem::take(&mut session.pending));
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
                owner: None,
                pending: Vec::new(),
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

    async fn switch_model(
        &self,
        id: &str,
        generation: i64,
        identity: &ag_harness::ExecutionIdentity,
        metadata: Option<ModelMetadata>,
        capabilities: ag_harness::ModelCapabilities,
    ) -> Result<i64, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
        if session.loaded.model_generation != generation {
            return Err(SessionError::StaleModel { id: id.to_string() });
        }
        if session.owner.is_some() {
            return Err(SessionError::Busy { id: id.to_string() });
        }
        for message in session.loaded.turns.iter().flatten() {
            match message {
                ModelMessage::AssistantToolCall(_)
                | ModelMessage::AssistantToolCalls(_)
                | ModelMessage::ToolResult { .. }
                    if !capabilities.tool_calls =>
                {
                    return Err(SessionError::UnsupportedModelHistory {
                        reason: "tool history",
                    });
                }
                ModelMessage::AssistantReasoning { .. } => {
                    return Err(SessionError::UnsupportedModelHistory {
                        reason: "provider reasoning",
                    });
                }
                ModelMessage::AssistantToolCall(call) if call.reasoning_content().is_some() => {
                    return Err(SessionError::UnsupportedModelHistory {
                        reason: "provider reasoning",
                    });
                }
                ModelMessage::AssistantToolCalls(calls)
                    if calls.iter().any(|call| call.reasoning_content().is_some()) =>
                {
                    return Err(SessionError::UnsupportedModelHistory {
                        reason: "provider reasoning",
                    });
                }
                ModelMessage::UserInput(input)
                    if input.has_images() && !capabilities.image_input =>
                {
                    return Err(SessionError::UnsupportedModelHistory {
                        reason: "image history",
                    });
                }
                _ => {}
            }
        }
        let next = generation
            .checked_add(1)
            .ok_or_else(|| SessionError::InvalidData {
                reason: "generation overflow".into(),
            })?;
        session.loaded.model_generation = next;
        session.loaded.registration_identity = Some(identity.clone());
        session.loaded.provider = metadata.as_ref().map(|value| value.provider().to_string());
        session.loaded.model = metadata.as_ref().map(|value| value.model().to_string());
        session.loaded.provider_session_id = None;
        Ok(next)
    }

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        generation: i64,
    ) -> Result<AcquiredTurn, SessionError> {
        let HostTurnAcquisition::Acquired(turn) =
            self.acquire(store, id, input, options, None, generation)?
        else {
            std::panic::resume_unwind(Box::new("legacy acquisition"))
        };

        Ok(turn)
    }

    async fn begin_request(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: &HostRequest,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError> {
        self.acquire(store, id, input, options, Some(request), generation)
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
        session.set_status(
            owner,
            HostTurnStatus::Failed {
                error_type: format!("{:?}", error.error_type()),
            },
        );
        session.owner = None;
        session.loaded.provider_session_id = None;

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
            session.set_status(
                owner,
                HostTurnStatus::Interrupted {
                    error_type: owner.interruption_error_type().into(),
                },
            );
            session.owner = None;
            session.loaded.provider_session_id = None;
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
