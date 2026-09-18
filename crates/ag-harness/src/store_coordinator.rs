//! Process-local admission retained across acquisition and owner cleanup.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex, OnceLock, Weak};

use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};
use tokio::time::Instant;

use crate::cancellation::{Settlement, SettlementLease};
use crate::effect::Effects;
use crate::session::{
    AcquiredTurn, LoadedSession, NewSession, StoreIdentity, TurnOwner, recover_abandoned,
};
use crate::{
    HostRequest, HostTurnAcquisition, HostTurnRecord, ModelMessage, ModelMetadata, SessionError,
    SessionStore, TurnError, TurnOptions, TurnOutcome, WriteRecord,
};

type Admissions = HashMap<(StoreIdentity, String), Weak<AsyncMutex<()>>>;

pub(crate) async fn acquire(
    store: Arc<dyn SessionStore>,
    session_id: String,
    prompt: String,
    options: TurnOptions,
    settlement: Option<Settlement>,
    effects: Effects,
) -> Result<AcquiredTurn, SessionError> {
    recover_abandoned(&store, &session_id).await?;
    let admission = Arc::new(admission(store.identity(), &session_id)?);
    effects.admit(Arc::clone(&admission));
    let store: Arc<dyn SessionStore> = Arc::new(AdmittedStore {
        store,
        admission: Mutex::new(Some(admission)),
        lease: Mutex::new(settlement.as_ref().map(Settlement::retain)),
        settlement,
    });
    // The backend finishes acquisition even when the caller disappears. Its
    // returned guard then interrupts the abandoned owner without running a
    // model.
    tokio::spawn(async move {
        store
            .begin_turn(Arc::clone(&store), &session_id, &prompt, &options)
            .await
    })
    .await
    .map_err(|source| SessionError::Store {
        operation: "acquire session turn",
        source: Box::new(source),
    })?
}

pub(crate) async fn acquire_request(
    store: Arc<dyn SessionStore>,
    session_id: String,
    prompt: String,
    options: TurnOptions,
    request: HostRequest,
    settlement: Option<Settlement>,
    effects: Effects,
) -> Result<HostTurnAcquisition, SessionError> {
    static REQUESTS: OnceLock<Mutex<Admissions>> = OnceLock::new();
    if let Some(record) = store.load_request(&session_id, request.id()).await? {
        record.check_request(&request)?;

        return Ok(HostTurnAcquisition::Recorded(record));
    }
    // Serialize local acquisition only, never the model execution. This also
    // lets a retry wait for a reservation that has not yet been committed.
    let mutex = admission_mutex(&REQUESTS, store.identity(), &session_id);
    let acquisition = mutex.lock_owned().await;
    if let Some(record) = store.load_request(&session_id, request.id()).await? {
        record.check_request(&request)?;

        return Ok(HostTurnAcquisition::Recorded(record));
    }
    recover_abandoned(&store, &session_id).await?;
    let admission = Arc::new(admission(store.identity(), &session_id)?);
    effects.admit(Arc::clone(&admission));
    let store: Arc<dyn SessionStore> = Arc::new(AdmittedStore {
        store,
        admission: Mutex::new(Some(admission)),
        lease: Mutex::new(settlement.as_ref().map(Settlement::retain)),
        settlement,
    });
    tokio::spawn(async move {
        let _acquisition = acquisition;

        store
            .begin_request(Arc::clone(&store), &session_id, &prompt, &options, &request)
            .await
    })
    .await
    .map_err(|source| SessionError::Store {
        operation: "acquire host request",
        source: Box::new(source),
    })?
}

fn admission(
    identity: &StoreIdentity,
    session_id: &str,
) -> Result<OwnedMutexGuard<()>, SessionError> {
    static ADMISSIONS: OnceLock<Mutex<Admissions>> = OnceLock::new();
    let mutex = admission_mutex(&ADMISSIONS, identity, session_id);

    mutex.try_lock_owned().map_err(|_| SessionError::Busy {
        id: session_id.to_string(),
    })
}

fn admission_mutex(
    registry: &OnceLock<Mutex<Admissions>>,
    identity: &StoreIdentity,
    session_id: &str,
) -> Arc<AsyncMutex<()>> {
    let mut admissions = registry
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    admissions.retain(|_, admission| admission.strong_count() != 0);
    let key = (identity.clone(), session_id.to_string());
    let mutex = admissions
        .get(&key)
        .and_then(Weak::upgrade)
        .unwrap_or_default();
    admissions.insert(key, Arc::downgrade(&mutex));

    mutex
}

// Acquisition must bind the guard to this decorator, so admission survives
// delayed commit acknowledgment, dropped callers, and failed cleanup.
struct AdmittedStore {
    admission: Mutex<Option<Arc<OwnedMutexGuard<()>>>>,
    lease: Mutex<Option<SettlementLease>>,
    settlement: Option<Settlement>,
    store: Arc<dyn SessionStore>,
}

impl AdmittedStore {
    fn settled(&self, result: Result<(), SessionError>) -> Result<(), SessionError> {
        let mut lease = self
            .lease
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if result.is_ok() {
            if let Some(settlement) = &self.settlement {
                settlement.recovered();
            }
            self.admission
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            lease.take();
        }

        result
    }
}

#[async_trait]
impl SessionStore for AdmittedStore {
    fn identity(&self) -> &StoreIdentity {
        self.store.identity()
    }

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        budget: usize,
    ) -> Result<(), SessionError> {
        self.store.create_session(config, metadata, budget).await
    }

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        self.store.load_session(id).await
    }

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        prompt: &str,
        options: &TurnOptions,
    ) -> Result<AcquiredTurn, SessionError> {
        self.store.begin_turn(store, id, prompt, options).await
    }

    async fn begin_request(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        prompt: &str,
        options: &TurnOptions,
        request: &HostRequest,
    ) -> Result<HostTurnAcquisition, SessionError> {
        self.store
            .begin_request(store, id, prompt, options, request)
            .await
    }

    async fn load_request(
        &self,
        id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        self.store.load_request(id, host_id).await
    }

    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        self.settled(
            self.store
                .complete_request(owner, messages, continuation, outcome)
                .await,
        )
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.store.renew(owner).await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
    ) -> Result<(), SessionError> {
        self.settled(
            self.store
                .complete_turn(owner, messages, continuation)
                .await,
        )
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        self.settled(self.store.fail_turn(owner, error).await)
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        let result = self.store.interrupt(owner).await;
        if let Err(error) = &result {
            let lease = self
                .lease
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if lease.is_some()
                && let Some(settlement) = &self.settlement
            {
                settlement.failed(owner, error);
            }
        }

        self.settled(result)
    }

    async fn load_writes(&self, id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        self.store.load_writes(id).await
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
        self.store
            .write_intent(owner, call, root, path, expected, resulting)
            .await
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        self.store.finish_write(owner, id, applied).await
    }
}

#[cfg(test)]
#[path = "store_coordinator_test.rs"]
mod tests;
