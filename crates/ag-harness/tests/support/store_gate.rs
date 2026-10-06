//! Deterministic acquisition and cleanup barriers around a public store.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ag_harness::model::{ModelMessage, ModelMetadata};
use ag_harness::recovery::HostTurnRecord;
use ag_harness::store::{
    LoadedSession, ModelSwitch, NewSession, Reservation, SessionStore, StoreIdentity,
    TurnAdmission, TurnOwner, WriteRecord,
};
use ag_harness::{SessionError, TurnError, TurnOutcome};
use async_trait::async_trait;
use tokio::sync::Notify;
use tokio::time::Instant;

pub(crate) struct Gate {
    pub(crate) after_commit: bool,
    pub(crate) entered: Notify,
    pub(crate) fail_cleanup: AtomicBool,
    pub(crate) identity: StoreIdentity,
    pub(crate) interrupted: Notify,
    pub(crate) panic_acquire: AtomicBool,
    pub(crate) panic_switch: bool,
    pub(crate) pause_switch: bool,
    pub(crate) release: Notify,
    pub(crate) reserve_session: Option<String>,
    pub(crate) store: Arc<dyn SessionStore>,
}

impl Gate {
    pub(crate) fn new(store: Arc<dyn SessionStore>, after_commit: bool) -> Self {
        Self {
            after_commit,
            entered: Notify::new(),
            fail_cleanup: AtomicBool::new(false),
            identity: store.identity().clone(),
            interrupted: Notify::new(),
            panic_acquire: AtomicBool::new(false),
            panic_switch: false,
            pause_switch: false,
            release: Notify::new(),
            reserve_session: None,
            store,
        }
    }

    async fn pause(&self) {
        self.entered.notify_one();
        self.release.notified().await;
    }
}

#[async_trait]
impl SessionStore for Gate {
    fn identity(&self) -> &StoreIdentity {
        &self.identity
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

    async fn publish_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &ag_harness::store::SessionCheckpoint,
    ) -> Result<(), SessionError> {
        self.store.publish_checkpoint(session_id, checkpoint).await
    }

    async fn switch_model(&self, id: &str, switch: &ModelSwitch) -> Result<i64, SessionError> {
        if self.panic_switch {
            std::panic::resume_unwind(Box::new("injected switch panic"));
        }
        if self.pause_switch && !self.after_commit {
            self.pause().await;
        }
        let result = self.store.switch_model(id, switch).await;
        if self.pause_switch && self.after_commit {
            self.pause().await;
        }
        result
    }

    async fn reserve_turn(
        &self,
        id: &str,
        admission: &TurnAdmission,
    ) -> Result<Reservation, SessionError> {
        let id = self.reserve_session.as_deref().unwrap_or(id);
        if admission.host_id().is_some() {
            return self.store.reserve_turn(id, admission).await;
        }
        if self.panic_acquire.load(Ordering::SeqCst) {
            std::panic::resume_unwind(Box::new("injected acquisition panic"));
        }
        if !self.after_commit {
            self.pause().await;
        }
        let reserved = self.store.reserve_turn(id, admission).await?;
        if self.after_commit {
            self.pause().await;
        }

        Ok(reserved)
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
        self.store.complete_request(owner, messages, outcome).await
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.store.renew(owner).await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError> {
        self.store.complete_turn(owner, messages).await
    }

    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError> {
        self.store.fail_turn(owner, error).await
    }

    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError> {
        self.interrupted.notify_one();
        if self.fail_cleanup.load(Ordering::SeqCst) {
            return Err(SessionError::Store {
                operation: "interrupt",
                source: Box::new(std::io::Error::other("injected cleanup failure")),
            });
        }

        self.store.interrupt(owner).await
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
