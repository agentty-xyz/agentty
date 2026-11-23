//! Session store decorator that fails selected operations of a memory store,
//! for deterministic persistence-failure tests that need no SQLite fault
//! injection.

use std::collections::HashSet;
use std::io;
use std::path::Path;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use async_trait::async_trait;
use tokio::sync::Notify;
use tokio::time::Instant;

use crate::bash::{CommandIntent, CommandOutcome, CommandRecord};
use crate::model::{ModelMessage, ModelMetadata};
use crate::recovery::HostTurnRecord;
use crate::store::{
    CompactionSource, HistoryProjection, LoadedSession, MemoryStore, ModelSwitch, NewSession,
    Reservation, SessionCheckpoint, StoreBackend, StoreIdentity, StoredTurn, TurnAdmission,
    TurnOwner, TurnUsage, WriteRecord,
};
use crate::transcript::TurnRange;
use crate::{SessionError, TurnError, TurnOutcome};

/// Store operation a [`FaultStore`] fails while the fault is injected.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) enum Fault {
    CommandIntent,
    CompleteTurn,
    FailTurn,
    FinishCommand,
    /// Loads report a provider without its model name.
    IncompleteModel,
    /// Publishing a compaction checkpoint fails.
    PublishCheckpoint,
    /// Turn lease renewals fail, so the turn loses its lease.
    Renew,
    WriteIntent,
}

/// Memory store whose injected operations fail with a store error.
pub(crate) struct FaultStore {
    faults: Mutex<HashSet<Fault>>,
    inner: MemoryStore,
    /// Notified when a checkpoint publication starts, which then never
    /// finishes, if set.
    publish_stall: Mutex<Option<Arc<Notify>>>,
}

impl FaultStore {
    pub(crate) fn new() -> Self {
        Self {
            faults: Mutex::new(HashSet::new()),
            inner: MemoryStore::new(),
            publish_stall: Mutex::new(None),
        }
    }

    /// Makes every later checkpoint publication notify `started` and never
    /// finish.
    pub(crate) fn stall_publication(&self, started: Arc<Notify>) {
        *self
            .publish_stall
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(started);
    }

    /// Fails `fault`'s operation until it is repaired.
    pub(crate) fn inject(&self, fault: Fault) {
        self.faults().insert(fault);
    }

    /// Lets `fault`'s operation reach the memory store again.
    pub(crate) fn repair(&self, fault: Fault) {
        self.faults().remove(&fault);
    }

    fn faults(&self) -> MutexGuard<'_, HashSet<Fault>> {
        self.faults.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn check(&self, fault: Fault, operation: &'static str) -> Result<(), SessionError> {
        if self.faults().contains(&fault) {
            return Err(SessionError::Store {
                operation,
                source: io::Error::other("injected store fault").into(),
            });
        }

        Ok(())
    }
}

#[async_trait]
impl StoreBackend for FaultStore {
    async fn load_commands(&self, session_id: &str) -> Result<Vec<CommandRecord>, SessionError> {
        self.inner.load_commands(session_id).await
    }

    async fn command_intent(
        &self,
        owner: &TurnOwner,
        intent: &CommandIntent,
    ) -> Result<i64, SessionError> {
        self.check(Fault::CommandIntent, "record command intent")?;

        self.inner.command_intent(owner, intent).await
    }

    async fn finish_command(
        &self,
        owner: &TurnOwner,
        id: i64,
        outcome: &CommandOutcome,
    ) -> Result<(), SessionError> {
        self.check(Fault::FinishCommand, "record command outcome")?;

        self.inner.finish_command(owner, id, outcome).await
    }

    async fn reconcile_command(&self, owner: &TurnOwner, id: i64) -> Result<(), SessionError> {
        self.inner.reconcile_command(owner, id).await
    }

    async fn load_transcript(
        &self,
        session_id: &str,
        range: &TurnRange,
    ) -> Result<Vec<StoredTurn>, SessionError> {
        self.inner.load_transcript(session_id, range).await
    }

    async fn fork_session(
        &self,
        source: &str,
        through: i64,
        target: &str,
    ) -> Result<(), SessionError> {
        self.inner.fork_session(source, through, target).await
    }

    fn identity(&self) -> &StoreIdentity {
        self.inner.identity()
    }

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError> {
        self.inner
            .create_session(config, metadata, max_history_bytes)
            .await
    }

    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError> {
        let mut loaded = self.inner.load_session(id).await?;
        if self.faults().contains(&Fault::IncompleteModel) {
            loaded.model = None;
        }

        Ok(loaded)
    }

    async fn switch_model(
        &self,
        session_id: &str,
        switch: &ModelSwitch,
    ) -> Result<i64, SessionError> {
        self.inner.switch_model(session_id, switch).await
    }

    async fn reserve_turn(
        &self,
        session_id: &str,
        admission: &TurnAdmission,
    ) -> Result<Reservation, SessionError> {
        self.inner.reserve_turn(session_id, admission).await
    }

    async fn publish_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &SessionCheckpoint,
    ) -> Result<(), SessionError> {
        self.check(Fault::PublishCheckpoint, "publish checkpoint")?;
        // A stalled publication never reaches the memory store.
        let stall = self
            .publish_stall
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        if let Some(started) = stall {
            started.notify_one();
            std::future::pending::<()>().await;
        }

        self.inner.publish_checkpoint(session_id, checkpoint).await
    }

    async fn load_compaction_source(
        &self,
        session_id: &str,
        max_history_bytes: usize,
    ) -> Result<CompactionSource, SessionError> {
        self.inner
            .load_compaction_source(session_id, max_history_bytes)
            .await
    }

    async fn load_request(
        &self,
        session_id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError> {
        self.inner.load_request(session_id, host_id).await
    }

    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        outcome: &TurnOutcome,
        projection: HistoryProjection,
    ) -> Result<(), SessionError> {
        self.inner
            .complete_request(owner, messages, outcome, projection)
            .await
    }

    async fn record_usage(&self, owner: &TurnOwner, usage: &TurnUsage) -> Result<(), SessionError> {
        self.inner.record_usage(owner, usage).await
    }

    async fn load_usage(&self, session_id: &str) -> Result<Vec<Option<TurnUsage>>, SessionError> {
        self.inner.load_usage(session_id).await
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.check(Fault::Renew, "renew turn lease")?;
        self.inner.renew(owner).await
    }

    async fn append_messages(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        self.inner.append_messages(owner, messages).await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        projection: HistoryProjection,
    ) -> Result<(), SessionError> {
        self.check(Fault::CompleteTurn, "complete turn")?;

        self.inner.complete_turn(owner, messages, projection).await
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        self.check(Fault::FailTurn, "fail turn")?;

        self.inner.fail_turn(owner, error).await
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        self.inner.interrupt(owner).await
    }

    async fn load_writes(&self, session_id: &str) -> Result<Vec<WriteRecord>, SessionError> {
        self.inner.load_writes(session_id).await
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
        self.check(Fault::WriteIntent, "record write intent")?;

        self.inner
            .write_intent(owner, call_id, root, path, expected, resulting)
            .await
    }

    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError> {
        self.inner.finish_write(owner, id, applied).await
    }
}
