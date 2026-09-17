//! Independent, test-only backend implemented exclusively through public APIs.

use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ag_harness::{
    AcquiredTurn, LoadedSession, ModelMessage, ModelMetadata, NewSession, SessionError,
    SessionStore, StoreIdentity, StoredTurnOptions, TurnError, TurnOptions, TurnOwner, WriteRecord,
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
    snapshot: Option<String>,
    writes: Vec<(Vec<u8>, WriteRecord)>,
}

impl Record {
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
                    max_history_bytes,
                    model: metadata.as_ref().map(|value| value.model().to_string()),
                    provider: metadata.as_ref().map(|value| value.provider().to_string()),
                    provider_context: None,
                    provider_session_id: None,
                    schema: config.schema().clone(),
                    system_prompt: config.system_prompt().map(str::to_string),
                    turns: Vec::new(),
                },
                next_turn: 0,
                owner: None,
                pending: Vec::new(),
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

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        prompt: &str,
        options: &TurnOptions,
    ) -> Result<AcquiredTurn, SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions
            .get_mut(id)
            .ok_or_else(|| SessionError::NotFound { id: id.to_string() })?;
        session.recover();
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
        )?
        .with_provider_context(session.loaded.provider_context.clone());
        session.next_turn += 1;
        session.owner = Some((owner, deadline));
        session.pending = vec![ModelMessage::User(prompt.to_string())];
        session.snapshot = Some(StoredTurnOptions::encode(options));
        session.loaded.provider_session_id = continuation;

        acquired.activate()
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
        provider_context: Option<&str>,
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
        session.pending.extend_from_slice(messages);
        session
            .loaded
            .turns
            .push(std::mem::take(&mut session.pending));
        session.owner = None;
        session.loaded.provider_context = provider_context.map(str::to_string);
        session.loaded.provider_session_id = provider_session_id.map(str::to_string);

        Ok(())
    }

    async fn fail_turn(&self, owner: &TurnOwner, _: &TurnError) -> Result<(), SessionError> {
        let mut sessions = self.sessions.lock().expect("sessions");
        let session = sessions.get_mut(owner.session_id()).expect("session");
        session.validate(owner)?;
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
