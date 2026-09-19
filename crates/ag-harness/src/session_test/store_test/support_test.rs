use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ag_harness::{HostRequest, HostTurnAcquisition, HostTurnRecord, TurnOutcome};
use async_trait::async_trait;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelMetadata};
use crate::session::tests::support::{schema, turn_options};
use crate::session::{
    AcquiredTurn, Database, LoadedSession, NewSession, SessionError, StoreIdentity, TurnOwner,
};
use crate::store::SessionStore;
use crate::{TurnError, TurnOptions, WriteRecord};

#[derive(Clone, Copy, Eq, PartialEq)]
pub(super) enum PauseAt {
    Renewal,
    RenewalAcknowledgement,
    RenewalAndCompletion,
    Completion,
    CompletionAcknowledgement,
    Failure,
}

pub(super) struct GatedStore {
    pub(super) database: Database,
    pub(super) entered: Notify,
    pub(super) interrupted: Notify,
    pub(super) pause_at: PauseAt,
    pub(super) release: Notify,
    pub(super) renewals: AtomicUsize,
    pub(super) write_intents: AtomicUsize,
    pub(super) write_outcomes: AtomicUsize,
}

impl GatedStore {
    pub(super) fn new(database: Database, pause_at: PauseAt) -> Self {
        Self {
            database,
            entered: Notify::new(),
            interrupted: Notify::new(),
            pause_at,
            release: Notify::new(),
            renewals: AtomicUsize::new(0),
            write_intents: AtomicUsize::new(0),
            write_outcomes: AtomicUsize::new(0),
        }
    }

    pub(super) async fn fixture(pause_at: PauseAt) -> (Arc<Self>, AcquiredTurn) {
        let database = Database::open_in_memory().await.expect("database");
        let store = Arc::new(Self::new(database, pause_at));
        store
            .create_session(&NewSession::new("session", schema()), None, 100_000)
            .await
            .expect("session");
        let backend: Arc<dyn SessionStore> = store.clone();
        let acquired = backend
            .begin_turn(
                Arc::clone(&backend),
                "session",
                &TurnInput::from("prompt"),
                &turn_options(),
                0,
            )
            .await
            .expect("turn");

        (store, acquired)
    }

    async fn pause(&self, phase: PauseAt) {
        if self.pause_at == phase
            || (self.pause_at == PauseAt::RenewalAndCompletion
                && matches!(phase, PauseAt::RenewalAcknowledgement | PauseAt::Completion))
        {
            self.entered.notify_one();
            self.release.notified().await;
        }
    }
}

#[async_trait]
impl SessionStore for GatedStore {
    fn identity(&self) -> &StoreIdentity {
        self.database.identity()
    }

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        budget: usize,
    ) -> Result<(), SessionError> {
        self.database.create_session(config, metadata, budget).await
    }

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        self.database.load_session(id).await
    }

    async fn switch_model(
        &self,
        id: &str,
        generation: i64,
        identity: &crate::ExecutionIdentity,
        metadata: Option<ModelMetadata>,
        capabilities: crate::ModelCapabilities,
    ) -> Result<i64, SessionError> {
        self.database
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
        self.database
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
        self.database
            .begin_request(store, id, input, options, request, generation)
            .await
    }

    async fn load_request(
        &self,
        id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        self.database.load_request(id, host_id).await
    }

    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError> {
        self.database
            .complete_request(owner, messages, continuation, outcome)
            .await
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.renewals.fetch_add(1, Ordering::SeqCst);
        self.pause(PauseAt::Renewal).await;
        let renewed = self.database.renew(owner).await?;
        self.pause(PauseAt::RenewalAcknowledgement).await;

        Ok(renewed)
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        continuation: Option<&str>,
    ) -> Result<(), SessionError> {
        self.pause(PauseAt::Completion).await;
        SessionStore::complete_turn(&self.database, owner, messages, continuation).await?;
        self.pause(PauseAt::CompletionAcknowledgement).await;

        Ok(())
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        self.pause(PauseAt::Failure).await;
        SessionStore::fail_turn(&self.database, owner, error).await
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        let result = self.database.interrupt(owner).await;
        self.interrupted.notify_one();

        result
    }

    async fn load_writes(&self, id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        self.database.load_writes(id).await
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
        self.write_intents.fetch_add(1, Ordering::SeqCst);
        self.database
            .write_intent(owner, call, root, path, expected, resulting)
            .await
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        self.write_outcomes.fetch_add(1, Ordering::SeqCst);
        self.database.finish_write(owner, id, applied).await
    }
}

pub(super) struct AcquisitionGate {
    pub(super) entered: Notify,
    pub(super) release: Notify,
}

#[async_trait]
impl crate::session::ReservationObserver for AcquisitionGate {
    async fn committed(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

pub(super) struct CommitGate {
    pub(super) entered: Notify,
    pub(super) fail: bool,
    pub(super) release: Notify,
}

#[async_trait]
impl crate::session::ReservationObserver for CommitGate {
    async fn committing(&self) {
        self.entered.notify_one();
        self.release.notified().await;
        assert!(!self.fail, "injected committer task failure");
    }

    async fn committed(&self) {}
}
