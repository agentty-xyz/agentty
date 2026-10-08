//! Turn reservation lifecycle: process-local admission, lease ownership, and
//! abandoned-owner recovery from acquisition through cleanup.
//!
//! One registry entry per store and session holds every process-local fact
//! about a reservation: the admission lock, the host-request acquisition lock,
//! and owners whose cleanup has not yet been acknowledged. Every acquisition
//! and model switch recovers those owners before admission, for every store
//! adapter. Stores only record admission decisions; [`AcquiredTurn`] is the
//! one place a reservation becomes a lease.

use std::collections::HashMap;
use std::future::Future;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard, oneshot};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::admission::{ModelSwitch, Reservation, ReservedTurn, TurnAdmission};
use crate::cancellation::{Settlement, SettlementLease};
use crate::compaction::SessionCheckpoint;
use crate::context::HistoryTurn;
use crate::effect::Effects;
use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelMetadata};
use crate::recovery::{HostRequest, HostTurnAcquisition, HostTurnRecord};
use crate::session::{LoadedSession, NewSession, StoreIdentity, TurnOwner};
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
    let switch = ModelSwitch::new(
        registration.identity().clone(),
        registration.metadata(),
        registration.history_capabilities(),
        generation,
    );
    tokio::spawn(async move {
        let _admission = admission;
        store.switch_model(&id, &switch).await
    })
    .await
    .map_err(|source| SessionError::Store {
        operation: "switch session model",
        source: Box::new(source),
    })?
}

/// Reserves a turn under admission. The reservation finishes even when the
/// caller disappears; the acquired turn then interrupts the abandoned owner
/// without running a model.
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
    let admission = TurnAdmission::new(input, options, None, generation);

    reserve(store, session_id, admission, None, "acquire session turn")
        .await?
        .into_acquired()
}

/// Returns the record already stored under `request`'s host ID after checking
/// that it denotes the same effective request.
pub(crate) async fn recorded_request(
    store: &dyn SessionStore,
    session_id: &str,
    request: &HostRequest,
) -> Result<Option<HostTurnRecord>, SessionError> {
    let Some(record) = store.load_request(session_id, request.id()).await? else {
        return Ok(None);
    };
    record.check_request(request)?;

    Ok(Some(record))
}

/// Returns a host request recorded since the caller's lookup, or reserves a
/// turn bound to it under admission.
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
    // Serialize local acquisition only, never the model execution. This also
    // lets a retry wait for a reservation that has not yet been committed.
    let acquisition = shared_lock(store.identity(), &session_id, |slot| &mut slot.request)
        .lock_owned()
        .await;
    if let Some(record) = recorded_request(store.as_ref(), &session_id, &request).await? {
        return Ok(HostTurnAcquisition::Recorded(record));
    }
    let store = AdmittedStore::admit(store, &session_id, settlement, &effects).await?;
    let admission = TurnAdmission::new(input, options, Some(request), generation);

    reserve(
        store,
        session_id,
        admission,
        Some(acquisition),
        "acquire host request",
    )
    .await
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

/// A reserved turn whose lease the harness owns. Dropping it interrupts only
/// its owner. The Tokio runtime must remain driven until cleanup completes;
/// persistence settlement does not settle filesystem effects.
pub struct AcquiredTurn {
    pub(crate) checkpoint: Option<SessionCheckpoint>,
    pub(crate) guard: TurnGuard,
    pub(crate) turns: Vec<HistoryTurn>,
}

impl AcquiredTurn {
    /// Reserves a turn through `store` under the harness admission rules and
    /// starts lease renewal. Reservation finishes even when this future is
    /// dropped; the abandoned turn is then interrupted without running a
    /// model.
    ///
    /// # Errors
    /// Returns the store's admission or persistence error,
    /// [`SessionError::OwnershipLost`] when the lease expired before
    /// acknowledgment, or [`SessionError::InvalidData`] when the store returns
    /// an owner for another store or session.
    pub async fn begin(
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        generation: i64,
    ) -> Result<Self, SessionError> {
        let admission = TurnAdmission::new(input.clone(), options.clone(), None, generation);

        reserve(
            store,
            session_id.to_string(),
            admission,
            None,
            "acquire session turn",
        )
        .await?
        .into_acquired()
    }

    /// Returns the request recorded under `request`'s host ID, or reserves a
    /// turn bound to it like [`Self::begin`].
    ///
    /// # Errors
    /// Returns [`SessionError::HostTurnConflict`] when the host ID records a
    /// different request, and otherwise the errors of [`Self::begin`].
    pub async fn begin_request(
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: &HostRequest,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError> {
        let admission = TurnAdmission::new(
            input.clone(),
            options.clone(),
            Some(request.clone()),
            generation,
        );

        reserve(
            store,
            session_id.to_string(),
            admission,
            None,
            "acquire host request",
        )
        .await
    }

    /// Returns the reservation identity used for backend lifecycle operations.
    pub fn owner(&self) -> &TurnOwner {
        self.guard.owner()
    }

    /// Arms owner-scoped cleanup, then starts renewal; an expired
    /// acknowledgment is interrupted rather than made executable. An owner
    /// bound to another store or session is rejected unarmed, since its
    /// cleanup would be filed under the wrong session.
    fn arm(
        store: Arc<dyn SessionStore>,
        session_id: &str,
        reserved: ReservedTurn,
    ) -> Result<Self, SessionError> {
        let ReservedTurn {
            checkpoint,
            deadline,
            owner,
            turns,
        } = reserved;
        if owner.store_identity() != store.identity() || owner.session_id() != session_id {
            return Err(SessionError::InvalidData {
                reason: "reserved owner belongs to another store or session".into(),
            });
        }
        let mut guard = TurnGuard::new(store, owner, deadline);
        guard.activate()?;

        Ok(Self {
            checkpoint,
            guard,
            turns,
        })
    }
}

/// Runs one store reservation to completion even if the caller disappears,
/// holding `retained` until it settles.
async fn reserve(
    store: Arc<dyn SessionStore>,
    session_id: String,
    admission: TurnAdmission,
    retained: Option<OwnedMutexGuard<()>>,
    operation: &'static str,
) -> Result<HostTurnAcquisition, SessionError> {
    tokio::spawn(async move {
        let _retained = retained;

        match store.reserve_turn(&session_id, &admission).await? {
            Reservation::Reserved(reserved) => {
                AcquiredTurn::arm(store, &session_id, reserved).map(HostTurnAcquisition::Acquired)
            }
            Reservation::Recorded(record) => Ok(HostTurnAcquisition::Recorded(record)),
        }
    })
    .await
    .map_err(|source| SessionError::Store {
        operation,
        source: Box::new(source),
    })?
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

    pub(crate) async fn append_messages(
        &self,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        self.database.append_messages(&self.owner, messages).await
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

    /// Takes cleanup responsibility for an acknowledged reservation. Activate
    /// it before the confirmed deadline to make the turn executable.
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
    pub(crate) async fn complete(&mut self, messages: &[ModelMessage]) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.complete_turn(&owner, messages))
            .await
    }

    pub(crate) async fn complete_request(
        &mut self,
        messages: &[ModelMessage],
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        let database = Arc::clone(&self.database);
        let owner = self.owner.clone();
        self.finalize(database.complete_request(&owner, messages, outcome))
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

    async fn switch_model(&self, id: &str, switch: &ModelSwitch) -> Result<i64, SessionError> {
        self.store.switch_model(id, switch).await
    }

    async fn reserve_turn(
        &self,
        id: &str,
        admission: &TurnAdmission,
    ) -> Result<Reservation, SessionError> {
        self.store.reserve_turn(id, admission).await
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
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        self.settled(self.store.complete_request(owner, messages, outcome).await)
    }

    async fn append_messages(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        self.store.append_messages(owner, messages).await
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.store.renew(owner).await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        self.settled(self.store.complete_turn(owner, messages).await)
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
