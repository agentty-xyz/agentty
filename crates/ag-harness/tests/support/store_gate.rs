//! Deterministic acquisition and cleanup barriers around a public store.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use ag_harness::{
    AcquiredTurn, LoadedSession, ModelMessage, ModelMetadata, NewSession, SessionError,
    SessionStore, StoreIdentity, TurnError, TurnOptions, TurnOwner, WriteRecord,
};
use async_trait::async_trait;
use tokio::sync::Notify;
use tokio::time::Instant;

pub(crate) struct Gate {
    pub(crate) after_commit: bool,
    pub(crate) entered: Notify,
    pub(crate) fail_cleanup: AtomicBool,
    pub(crate) interrupted: Notify,
    pub(crate) panic_acquire: AtomicBool,
    pub(crate) release: Notify,
    pub(crate) store: Arc<dyn SessionStore>,
}

impl Gate {
    pub(crate) fn new(store: Arc<dyn SessionStore>, after_commit: bool) -> Self {
        Self {
            after_commit,
            entered: Notify::new(),
            fail_cleanup: AtomicBool::new(false),
            interrupted: Notify::new(),
            panic_acquire: AtomicBool::new(false),
            release: Notify::new(),
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
        if self.panic_acquire.load(Ordering::SeqCst) {
            std::panic::resume_unwind(Box::new("injected acquisition panic"));
        }
        if !self.after_commit {
            self.pause().await;
        }
        let acquired = self.store.begin_turn(store, id, prompt, options).await?;
        if self.after_commit {
            self.pause().await;
        }

        Ok(acquired)
    }

    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError> {
        self.store.renew(owner).await
    }

    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_context: Option<&str>,
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError> {
        self.store
            .complete_turn(owner, messages, provider_context, provider_session_id)
            .await
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
