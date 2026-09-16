//! Internal transactional boundary used by durable execution.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::time::Instant;

use crate::model::{ModelMessage, ModelMetadata};
use crate::session::{
    AcquiredTurn, LoadedSession, NewSession, SessionError, StoreIdentity, TurnOwner,
};
use crate::{TurnError, TurnOptions, WriteRecord};

/// Owner-scoped mutations validate ownership in the transaction applying their
/// effects. Acquisition must settle its commit before reporting failure;
/// dropping its waiter retains responsibility for any eventual reservation.
/// History returned by load/acquire contains only complete turns within the
/// stored byte budget. Implementations preserve the existing options codec and
/// fingerprints.
#[async_trait]
pub(crate) trait SessionStore: Send + Sync {
    /// Independent handles for the same backing store share this identity.
    fn identity(&self) -> &StoreIdentity;

    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError>;
    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError>;
    /// Bind the reservation to `store` before commit, including abandoned
    /// acquisition cleanup. Decorators forward this handle unchanged; it must
    /// have the same backing identity as the acquiring implementation.
    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        prompt: &str,
        options: &TurnOptions,
    ) -> Result<AcquiredTurn, SessionError>;

    /// Renew only an unexpired owner; return a conservative confirmed deadline.
    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError>;
    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError>;
    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError>;

    /// Idempotent cleanup must never clear a successor's continuation.
    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError>;
    async fn load_writes(&self, session_id: &str) -> Result<Vec<WriteRecord>, SessionError>;
    async fn write_intent(
        &self,
        owner: &TurnOwner,
        call_id: &str,
        root: &Path,
        path: &str,
        expected: Option<&[u8]>,
        resulting: &[u8],
    ) -> Result<i64, SessionError>;

    /// An existing intent can settle after expiry or terminal transition.
    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError>;
}
