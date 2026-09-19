//! Host-implementable transactional boundary used by session execution.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::time::Instant;

use crate::input::TurnInput;
use crate::model::{ModelMessage, ModelMetadata};
use crate::session::{
    AcquiredTurn, LoadedSession, NewSession, SessionError, StoreIdentity, TurnOwner,
};
use crate::{
    HostRequest, HostTurnAcquisition, HostTurnRecord, TurnError, TurnOptions, TurnOutcome,
    WriteRecord,
};

/// Owner-scoped mutations validate ownership in the transaction applying their
/// effects. Acquisition must settle its commit before reporting failure;
/// dropping its waiter retains responsibility for any eventual reservation.
/// History returned by load/acquire contains only complete turns within the
/// stored byte budget. Implementations preserve the existing options codec and
/// fingerprints.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Independent handles for the same backing store share this identity.
    fn identity(&self) -> &StoreIdentity;

    /// Atomically creates a session, rejecting an existing identifier.
    /// Initialize generation zero and retain
    /// [`NewSession::registration_identity`], including its absence.
    async fn create_session(
        &self,
        config: &NewSession,
        metadata: Option<ModelMetadata>,
        max_history_bytes: usize,
    ) -> Result<(), SessionError>;
    /// Loads configuration and bounded completed history; recovers expired
    /// turns. Return the current registration identity in [`LoadedSession`];
    /// a load or turn must never assign or switch that identity.
    async fn load_session(&self, id: &str) -> Result<LoadedSession, SessionError>;
    /// Switch an idle session atomically with turn admission. Recover expired
    /// owners, reject active owners and a mismatched generation, validate all
    /// completed history with the target capabilities, then update identity,
    /// increment the generation, and clear continuation in the same mutation.
    /// Rejections leave model selection unchanged. Never rewrite turn
    /// snapshots. Reject provider-specific reasoning explicitly: it has no
    /// cross-model portability contract. Validate canonical history, including
    /// completed turns outside the replay budget.
    async fn switch_model(
        &self,
        id: &str,
        generation: i64,
        identity: &crate::ExecutionIdentity,
        metadata: Option<ModelMetadata>,
        capabilities: crate::ModelCapabilities,
    ) -> Result<i64, SessionError>;

    /// Bind the reservation to `store` before commit, including abandoned
    /// acquisition cleanup. Decorators forward this handle unchanged; it must
    /// have the same backing identity as the acquiring implementation.
    ///
    /// Construct [`AcquiredTurn`] before committing and call its `activate`
    /// method only after acknowledgment. Errors must be definitive: no later
    /// reservation may appear without a retained cleanup owner. A dropped
    /// future must retain that responsibility. Clear incompatible continuation
    /// atomically, using [`crate::StoredTurnOptions`]. Return only completed
    /// history bounded by the stored payload budget, revalidated at
    /// acquisition. Validate `generation` atomically before reserving a turn;
    /// persist the selected model as immutable turn provenance. Persist the
    /// validated input through the shared message codec, preserving block
    /// order and image content.
    async fn begin_turn(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        generation: i64,
    ) -> Result<AcquiredTurn, SessionError>;

    /// Atomically classify `request` before checking session busy state, or
    /// bind it to a new reservation. Compare fingerprints before status or
    /// generation. Use the same retained acquisition/cleanup rules as
    /// `begin_turn`; a separate lookup followed by insertion is
    /// insufficient across independent handles.
    async fn begin_request(
        &self,
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: &HostRequest,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError>;

    /// Recover expired reservations and return the canonical request outcome
    /// plus its current journal. Never execute a model or tool. Missing host
    /// IDs return `None`; missing sessions return `NotFound`.
    async fn load_request(
        &self,
        session_id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError>;

    /// Commit the complete result together with messages and terminal state
    /// under live ownership. An acknowledgment failure must not overwrite a
    /// committed result during subsequent interrupt/cleanup.
    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError>;

    /// Renew only an unexpired owner; return a conservative confirmed deadline.
    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError>;
    /// Atomically commits messages and continuation under an unexpired owner.
    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError>;
    /// Marks an unexpired owned turn failed and clears its continuation.
    async fn fail_turn(&self, owner: &TurnOwner, error: &TurnError) -> Result<(), SessionError>;

    /// Idempotent cleanup must never clear a successor's continuation.
    async fn interrupt(&self, owner: &TurnOwner) -> Result<(), SessionError>;
    /// Returns all write records, including failed, interrupted, and evicted
    /// turns.
    async fn load_writes(&self, session_id: &str) -> Result<Vec<WriteRecord>, SessionError>;
    /// Commits intent under live ownership before any filesystem replacement.
    /// Store native paths losslessly and hash content with SHA-256 hexadecimal.
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
    /// Validate its original owner even after a successor starts.
    /// Atomically accept only pending records or retries with the same outcome;
    /// reject conflicting settlements without changing the retained record.
    async fn finish_write(
        &self,
        owner: &TurnOwner,
        id: i64,
        applied: bool,
    ) -> Result<(), SessionError>;
}
