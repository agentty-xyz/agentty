//! Turn reservation lifecycle: process-local admission, lease ownership, and
//! abandoned-owner recovery from acquisition through cleanup.
//!
//! One registry entry per store and session holds every process-local fact
//! about a reservation: the admission lock, the host-request acquisition lock,
//! and owners whose cleanup has not yet been acknowledged. Every acquisition
//! and model switch recovers those owners before admission, for every store
//! adapter.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::cancellation::{Settlement, SettlementLease};
use crate::effect::Effects;
use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelMetadata};
use crate::recovery::{HostRequest, HostTurnAcquisition, HostTurnRecord};
use crate::session::{AcquiredTurn, LoadedSession, NewSession, StoreIdentity, TurnOwner};
use crate::store::{SessionStore, WriteRecord};
use crate::{SessionError, TurnError, TurnOptions, TurnOutcome};

pub(crate) const TURN_LEASE_SECONDS: i64 = 300;
pub(crate) const TURN_LEASE_RENEWAL_INTERVAL_SECONDS: u64 = 100;

type SessionKey = (StoreIdentity, String);

/// Switches an idle session's model under admission, after recovering its
/// abandoned owners.
pub(crate) async fn switch_model(
    store: Arc<dyn SessionStore>,
    id: String,
    generation: i64,
    registration: crate::model::ModelRegistration,
) -> Result<i64, SessionError> {
    recover_session(store.identity(), &id).await?;
    let admission = admit(store.identity(), &id)?;
    tokio::spawn(async move {
        let _admission = admission;
        store
            .switch_model(
                &id,
                generation,
                registration.identity(),
                registration.metadata(),
                registration.history_capabilities(),
            )
            .await
    })
    .await
    .map_err(|source| SessionError::Store {
        operation: "switch session model",
        source: Box::new(source),
    })?
}

/// Reserves a turn under admission. The store finishes acquisition even when
/// the caller disappears; the returned guard then interrupts the abandoned
/// owner without running a model.
pub(crate) async fn acquire(
    store: Arc<dyn SessionStore>,
    selection: (String, i64),
    input: TurnInput,
    options: TurnOptions,
    settlement: Option<Settlement>,
    effects: Effects,
) -> Result<AcquiredTurn, SessionError> {
    let (session_id, generation) = selection;
    let store = AdmittedStore::admit(store, &session_id, settlement, &effects).await?;
    tokio::spawn(async move {
        store
            .begin_turn(
                Arc::clone(&store),
                &session_id,
                &input,
                &options,
                generation,
            )
            .await
    })
    .await
    .map_err(|source| SessionError::Store {
        operation: "acquire session turn",
        source: Box::new(source),
    })?
}

/// Returns a recorded host request, or reserves a turn bound to it under
/// admission.
pub(crate) async fn acquire_request(
    store: Arc<dyn SessionStore>,
    selection: (String, i64),
    input: TurnInput,
    options: TurnOptions,
    request: HostRequest,
    settlement: Option<Settlement>,
    effects: Effects,
) -> Result<HostTurnAcquisition, SessionError> {
    let (session_id, generation) = selection;
    if let Some(record) = store.load_request(&session_id, request.id()).await? {
        record.check_request(&request)?;

        return Ok(HostTurnAcquisition::Recorded(record));
    }
    // Serialize local acquisition only, never the model execution. This also
    // lets a retry wait for a reservation that has not yet been committed.
    let acquisition = shared_lock(store.identity(), &session_id, |slot| &mut slot.request)
        .lock_owned()
        .await;
    if let Some(record) = store.load_request(&session_id, request.id()).await? {
        record.check_request(&request)?;

        return Ok(HostTurnAcquisition::Recorded(record));
    }
    let store = AdmittedStore::admit(store, &session_id, settlement, &effects).await?;
    tokio::spawn(async move {
        let _acquisition = acquisition;

        store
            .begin_request(
                Arc::clone(&store),
                &session_id,
                &input,
                &options,
                &request,
                generation,
            )
            .await
    })
    .await
    .map_err(|source| SessionError::Store {
        operation: "acquire host request",
        source: Box::new(source),
    })?
}

/// Retries cleanup of one abandoned owner through its retained store handle.
/// An owner that is no longer registered has already been recovered.
pub(crate) async fn recover_owner(owner: &TurnOwner) -> Result<(), SessionError> {
    let Some(store) = retained_store(owner) else {
        return Ok(());
    };
    store.interrupt(owner).await?;
    forget(owner);

    Ok(())
}

/// Returns a conservative local deadline for a lease granted now.
pub(crate) fn lease_deadline() -> Instant {
    // Stored timestamps round down to seconds; never promise the fractional
    // second.
    Instant::now() + Duration::from_secs(TURN_LEASE_SECONDS.unsigned_abs() - 1)
}

/// Owned journal access scoped to the turn that acquired it.
#[derive(Clone)]
pub(crate) struct WriteJournal {
    database: Arc<dyn SessionStore>,
    owner: TurnOwner,
}

impl WriteJournal {
    pub(crate) fn owner(&self) -> &TurnOwner {
        &self.owner
    }

    pub(crate) async fn reconcile_command(&self, id: i64) -> Result<(), SessionError> {
        self.database.reconcile_command(&self.owner, id).await
    }

    pub(crate) async fn command_intent(
        &self,
        intent: &crate::bash::CommandIntent,
    ) -> Result<i64, SessionError> {
        self.database.command_intent(&self.owner, intent).await
    }

    pub(crate) async fn finish_command(
        &self,
        id: i64,
        outcome: &crate::bash::CommandOutcome,
    ) -> Result<(), SessionError> {
        self.database.finish_command(&self.owner, id, outcome).await
    }

    pub(crate) async fn intent(
        &self,
        call_id: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError> {
        self.database
            .write_intent(&self.owner, call_id, root, path, expected, resulting)
            .await
    }

    pub(crate) async fn finish(&self, id: i64, applied: bool) -> Result<(), SessionError> {
        self.database.finish_write(&self.owner, id, applied).await
    }
}

pub(crate) struct TurnGuard {
    armed: bool,
    database: Arc<dyn SessionStore>,
    deadline: Arc<Mutex<Instant>>,
    finalization: Arc<AsyncMutex<()>>,
    owner: TurnOwner,
    ownership_failure: Option<oneshot::Receiver<SessionError>>,
    renewal_stop: Option<oneshot::Sender<()>>,
    renewal_task: Option<JoinHandle<()>>,
    runtime: tokio::runtime::Handle,
}

impl TurnGuard {
    pub(crate) fn owner(&self) -> &TurnOwner {
        &self.owner
    }

    pub(crate) fn write_journal(&self) -> WriteJournal {
        WriteJournal {
            database: Arc::clone(&self.database),
            owner: self.owner.clone(),
        }
    }

    /// Retain cleanup responsibility before committing a reservation. Activate
    /// only after the commit is acknowledged within the confirmed deadline.
    pub(crate) fn new(
        database: Arc<dyn SessionStore>,
        owner: TurnOwner,
        deadline: Instant,
    ) -> Self {
        Self {
            armed: true,
            database,
            deadline: Arc::new(Mutex::new(deadline)),
            finalization: Arc::new(AsyncMutex::new(())),
            owner,
            ownership_failure: None,
            renewal_stop: None,
            renewal_task: None,
            runtime: tokio::runtime::Handle::current(),
        }
    }

    /// Starts ownership monitoring once the reservation is acknowledged.
    /// Renewal is scheduled halfway through the remaining confirmed lease,
    /// capped at the renewal interval, and recalculated after every
    /// acknowledgment. Activating an active guard has no effect.
    ///
    /// # Errors
    /// An expired acknowledgment is interrupted rather than made executable.
    pub(crate) fn activate(&mut self) -> Result<(), SessionError> {
        if Instant::now() >= self.confirmed_deadline() {
            return Err(self.owner.lost());
        }
        if self.renewal_task.is_some() {
            return Ok(());
        }
        self.owner.interruption_error_type = "cancelled";
        let owner = self.owner.clone();
        let interval = Duration::from_secs(TURN_LEASE_RENEWAL_INTERVAL_SECONDS);
        let mut confirmed_at = Instant::now();
        let database = Arc::clone(&self.database);
        let deadline = Arc::clone(&self.deadline);
        let finalization = Arc::clone(&self.finalization);
        let (renewal_stop, mut stop_requested) = oneshot::channel();
        let (ownership_failed, ownership_failure) = oneshot::channel();
        self.renewal_stop = Some(renewal_stop);
        self.ownership_failure = Some(ownership_failure);
        self.renewal_task = Some(self.runtime.spawn(async move {
            let monitor = async {
                loop {
                    let confirmed = *deadline.lock().unwrap_or_else(PoisonError::into_inner);
                    let renew_at = confirmed_at
                        + interval.min(confirmed.saturating_duration_since(confirmed_at) / 2);
                    let renewal = async {
                        tokio::time::sleep_until(renew_at).await;
                        let _exclusive = finalization.lock().await;
                        let renewed = database.renew(&owner).await?;
                        *deadline.lock().unwrap_or_else(PoisonError::into_inner) = renewed;
                        confirmed_at = Instant::now();

                        Ok::<_, SessionError>(())
                    };
                    tokio::select! {
                        biased;
                        () = tokio::time::sleep_until(confirmed) => return owner.lost(),
                        result = renewal => if let Err(error) = result { return error; },
                    }
                }
            };
            tokio::select! {
                error = monitor => { let _ = ownership_failed.send(error); }
                _ = &mut stop_requested => {}
            }
        }));

        Ok(())
    }

    pub(crate) async fn ownership_failure(&mut self) -> SessionError {
        if Instant::now() >= self.confirmed_deadline() {
            return self.owner.lost();
        }
        let Some(failure) = self.ownership_failure.as_mut() else {
            return self.owner.lost();
        };
        let result = failure.await.unwrap_or_else(|_| self.owner.lost());
        self.ownership_failure = None;

        result
    }

    /// Renewal and finalization cannot race their acknowledgements. Waiting
    /// for a stalled renewal remains bounded by its last confirmed deadline.
    pub(crate) async fn complete(
        &mut self,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.complete_turn(&owner, messages, provider_session_id))
            .await
    }

    pub(crate) async fn complete_request(
        &mut self,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.complete_request(&owner, messages, continuation, outcome))
            .await
    }

    pub(crate) async fn fail(&mut self, error: &TurnError) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.fail_turn(&owner, error)).await
    }

    async fn finalize(
        &mut self,
        persistence: impl Future<Output = Result<(), SessionError>>,
    ) -> Result<(), SessionError> {
        let finalization = Arc::clone(&self.finalization);
        let _exclusive = tokio::select! {
            biased;
            error = self.ownership_failure() => return Err(error),
            exclusive = finalization.lock() => exclusive,
        };
        self.stop_renewal();
        let deadline = self.confirmed_deadline();
        if Instant::now() >= deadline {
            return Err(self.owner.lost());
        }
        let result = tokio::time::timeout_at(deadline, persistence)
            .await
            .unwrap_or_else(|_| Err(self.owner.lost()));
        if result.is_ok() {
            self.disarm();
        }

        result
    }

    pub(crate) fn disarm(&mut self) {
        self.stop_renewal();
        self.armed = false;
    }

    pub(crate) fn mark_interrupted(&mut self) {
        self.owner.interruption_error_type = "interrupted";
    }

    fn confirmed_deadline(&self) -> Instant {
        *self.deadline.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn stop_renewal(&mut self) {
        if let Some(stop) = self.renewal_stop.take() {
            let _ = stop.send(());
        }
        if let Some(task) = self.renewal_task.take() {
            task.abort();
        }
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        self.stop_renewal();
        if !self.armed {
            return;
        }
        let owner = self.owner.clone();
        abandon(owner.clone(), Arc::clone(&self.database));
        let database = Arc::clone(&self.database);
        std::mem::drop(self.runtime.spawn(async move {
            if database.interrupt(&owner).await.is_ok() {
                forget(&owner);
            }
        }));
    }
}

/// Process-local reservation state for one store and session.
#[derive(Default)]
struct SessionSlot {
    abandoned: HashMap<TurnOwner, Arc<dyn SessionStore>>,
    admission: Weak<AsyncMutex<()>>,
    request: Weak<AsyncMutex<()>>,
}

impl SessionSlot {
    fn is_idle(&self) -> bool {
        self.abandoned.is_empty()
            && self.admission.strong_count() == 0
            && self.request.strong_count() == 0
    }
}

fn sessions() -> MutexGuard<'static, HashMap<SessionKey, SessionSlot>> {
    static SESSIONS: OnceLock<Mutex<HashMap<SessionKey, SessionSlot>>> = OnceLock::new();
    let mut sessions = SESSIONS
        .get_or_init(Mutex::default)
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    sessions.retain(|_, slot| !slot.is_idle());

    sessions
}

fn session_key(identity: &StoreIdentity, session_id: &str) -> SessionKey {
    (identity.clone(), session_id.to_string())
}

fn shared_lock(
    identity: &StoreIdentity,
    session_id: &str,
    lock: impl FnOnce(&mut SessionSlot) -> &mut Weak<AsyncMutex<()>>,
) -> Arc<AsyncMutex<()>> {
    let mut sessions = sessions();
    let registered = lock(
        sessions
            .entry(session_key(identity, session_id))
            .or_default(),
    );
    let mutex = registered.upgrade().unwrap_or_default();
    *registered = Arc::downgrade(&mutex);

    mutex
}

fn admit(identity: &StoreIdentity, session_id: &str) -> Result<OwnedMutexGuard<()>, SessionError> {
    shared_lock(identity, session_id, |slot| &mut slot.admission)
        .try_lock_owned()
        .map_err(|_| SessionError::Busy {
            id: session_id.to_string(),
        })
}

async fn recover_session(identity: &StoreIdentity, session_id: &str) -> Result<(), SessionError> {
    let abandoned: Vec<_> = sessions()
        .get(&session_key(identity, session_id))
        .map_or_default(|slot| {
            slot.abandoned
                .iter()
                .map(|(owner, store)| (owner.clone(), Arc::clone(store)))
                .collect()
        });
    for (owner, store) in abandoned {
        store.interrupt(&owner).await?;
        forget(&owner);
    }

    Ok(())
}

fn abandon(owner: TurnOwner, store: Arc<dyn SessionStore>) {
    let key = session_key(owner.store_identity(), owner.session_id());
    let replaced = sessions()
        .entry(key)
        .or_default()
        .abandoned
        .insert(owner, store);
    // Release a replaced handle outside the registry lock.
    drop(replaced);
}

fn retained_store(owner: &TurnOwner) -> Option<Arc<dyn SessionStore>> {
    sessions()
        .get(&session_key(owner.store_identity(), owner.session_id()))
        .and_then(|slot| slot.abandoned.get(owner))
        .map(Arc::clone)
}

fn forget(owner: &TurnOwner) {
    let removed = sessions()
        .get_mut(&session_key(owner.store_identity(), owner.session_id()))
        .and_then(|slot| slot.abandoned.remove(owner));
    // Release the retained handle outside the registry lock.
    drop(removed);
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
    /// Recovers abandoned owners, then binds admission and settlement to the
    /// handle every acquired guard retains.
    async fn admit(
        store: Arc<dyn SessionStore>,
        session_id: &str,
        settlement: Option<Settlement>,
        effects: &Effects,
    ) -> Result<Arc<dyn SessionStore>, SessionError> {
        recover_session(store.identity(), session_id).await?;
        let admission = Arc::new(admit(store.identity(), session_id)?);
        effects.admit(Arc::clone(&admission));

        Ok(Arc::new(Self {
            admission: Mutex::new(Some(admission)),
            lease: Mutex::new(settlement.as_ref().map(Settlement::retain)),
            settlement,
            store,
        }))
    }

    fn settled(&self, result: Result<(), SessionError>) -> Result<(), SessionError> {
        let mut lease = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
        if result.is_ok() {
            if let Some(settlement) = &self.settlement {
                settlement.recovered();
            }
            self.admission
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .take();
            lease.take();
        }

        result
    }
}

#[async_trait]
impl SessionStore for AdmittedStore {
    async fn load_commands(
        &self,
        session: &str,
    ) -> Result<Vec<crate::bash::CommandRecord>, SessionError> {
        self.store.load_commands(session).await
    }

    async fn command_intent(
        &self,
        owner: &TurnOwner,
        intent: &crate::bash::CommandIntent,
    ) -> Result<i64, SessionError> {
        self.store.command_intent(owner, intent).await
    }

    async fn finish_command(
        &self,
        owner: &TurnOwner,
        id: i64,
        outcome: &crate::bash::CommandOutcome,
    ) -> Result<(), SessionError> {
        self.store.finish_command(owner, id, outcome).await
    }

    async fn reconcile_command(&self, owner: &TurnOwner, id: i64) -> Result<(), SessionError> {
        self.store.reconcile_command(owner, id).await
    }

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

    async fn switch_model(
        &self,
        id: &str,
        generation: i64,
        identity: &crate::recovery::ExecutionIdentity,
        metadata: Option<ModelMetadata>,
        capabilities: crate::model::ModelCapabilities,
    ) -> Result<i64, SessionError> {
        self.store
            .switch_model(id, generation, identity, metadata, capabilities)
            .await
    }

    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        generation: i64,
    ) -> Result<AcquiredTurn, SessionError> {
        self.store
            .begin_turn(store, id, input, options, generation)
            .await
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
        self.store
            .begin_request(store, id, input, options, request, generation)
            .await
    }

    async fn publish_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &crate::store::SessionCheckpoint,
    ) -> Result<(), SessionError> {
        self.store.publish_checkpoint(session_id, checkpoint).await
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
            let lease = self.lease.lock().unwrap_or_else(PoisonError::into_inner);
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
#[path = "reservation_test.rs"]
mod tests;
