//! Where durable sessions live.
//!
//! [`SqliteStore`] is the default (configured with `Harness::database`),
//! [`MemoryStore`] keeps resumable sessions for one process, and custom
//! backends implement [`SessionStore`] and are injected with `Harness::store`.
//! The records returned by `Session::writes` and `Session::compact` are also
//! defined here.

use std::path::Path;

use async_trait::async_trait;
use tokio::time::Instant;

pub use crate::admission::{
    Admission, AdmissionState, ModelSwitch, NewTurn, Reservation, ReservedTurn, TurnAdmission,
};
pub use crate::compaction::{CheckpointError, MAX_SUMMARY_BYTES, SessionCheckpoint};
pub use crate::memory_store::MemoryStore;
use crate::model::{ModelMessage, ModelMetadata};
use crate::recovery::HostTurnRecord;
pub use crate::reservation::AcquiredTurn;
use crate::session::SessionError;
pub use crate::session::{
    Database as SqliteStore, LoadedSession, NewSession, SessionInfo, StoreIdentity, TurnOwner,
};
pub use crate::session_model::RecordedModel;
pub use crate::stopped_turn::{RejectedContent, StoppedTurn};
use crate::turn::{TurnError, TurnOutcome};
pub use crate::turn_options_snapshot::{StoredTurnOptions, StoredTurnOptionsError};
pub use crate::write_journal::{WriteRecord, WriteStatus};

/// Stores supply atomic record operations; the harness applies admission
/// rules and owns every reservation's lease. Owner-scoped mutations validate
/// ownership in the transaction applying their effects. History returned by
/// loads and reservations contains completed and stopped (failed or
/// interrupted) turns, never a running turn, within the stored byte budget
/// and, when a compaction checkpoint exists, only turns after its covered
/// boundary; both return the current checkpoint alongside that history. A
/// stopped turn replays its input and recorded progress followed by
/// [`StoppedTurn::note`] for its stored error type; the note counts against
/// the byte budget. Selection walks turns newest first and stops at the first
/// turn that does not fit, except that a stopped turn that does not fit whole
/// replays only its input and note when those fit, and older turns stay
/// eligible. Content replaced through [`SessionStore::omit_rejected`] replays
/// as its placeholder. Implementations preserve the existing options codec
/// and fingerprints.
#[async_trait]
pub trait SessionStore: Send + Sync {
    /// Returns command records independently of completed history. Stores that
    /// support Bash must also atomically block new acquisitions while any
    /// command has an unknown or unresolved outcome.
    async fn load_commands(
        &self,
        _session_id: &str,
    ) -> Result<Vec<crate::bash::CommandRecord>, SessionError> {
        Err(SessionError::InvalidData {
            reason: "command storage unsupported".into(),
        })
    }

    /// Commits command intent under a live owner before spawning. Unsupported
    /// stores fail closed. Duplicate host IDs must never call this again.
    async fn command_intent(
        &self,
        _owner: &TurnOwner,
        _intent: &crate::bash::CommandIntent,
    ) -> Result<i64, SessionError> {
        Err(SessionError::InvalidData {
            reason: "command storage unsupported".into(),
        })
    }

    /// Settles an existing command under its original owner, including after
    /// lease expiry. Accept only pending or identical outcomes.
    async fn finish_command(
        &self,
        _owner: &TurnOwner,
        _id: i64,
        _outcome: &crate::bash::CommandOutcome,
    ) -> Result<(), SessionError> {
        Err(SessionError::InvalidData {
            reason: "command storage unsupported".into(),
        })
    }

    /// Records an explicit host assertion that conflicting execution is safe.
    /// Validate the original owner and reject live turns. Preserve the original
    /// outcome, including unknown status; never automatically rerun a command.
    async fn reconcile_command(&self, _owner: &TurnOwner, _id: i64) -> Result<(), SessionError> {
        Err(SessionError::InvalidData {
            reason: "command storage unsupported".into(),
        })
    }

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
    /// Switches an idle session's model in one atomic section: recover
    /// expired turns and validate every replayable canonical message with
    /// [`ModelSwitch::check_history`], including turns outside the replay
    /// budget, so unsupported history is reported before stale or busy
    /// admission. Then read [`AdmissionState`], apply [`ModelSwitch::admit`]
    /// for the generation to record, and record it with the switch's identity
    /// and metadata while clearing continuation, in the same mutation.
    /// Rejections leave model selection unchanged; never rewrite turn
    /// snapshots.
    async fn switch_model(
        &self,
        session_id: &str,
        switch: &ModelSwitch,
    ) -> Result<i64, SessionError>;

    /// Reserves a turn in one atomic section across every handle to the
    /// backing store: recover expired turns, read [`AdmissionState`] with the
    /// request recorded under [`TurnAdmission::host_id`], and apply
    /// [`TurnAdmission::admit`]. Return a recorded request unchanged.
    /// Otherwise persist the [`NewTurn`] as the running turn at the next
    /// position under a fresh owner token and lease, with the current model
    /// selection as immutable provenance, and set the session continuation to
    /// its `continuation`. Persist its message through the shared message
    /// codec, preserving block order and image content. Return a
    /// [`ReservedTurn`] whose owner names this store's identity and
    /// `session_id`, and whose deadline never exceeds the stored lease expiry;
    /// the harness rejects any other owner without executing.
    /// Errors must be definitive: a failed reservation leaves no running turn.
    async fn reserve_turn(
        &self,
        session_id: &str,
        admission: &TurnAdmission,
    ) -> Result<Reservation, SessionError>;

    /// Atomically publishes the session's compaction checkpoint, replacing an
    /// existing record. Validate before mutating: the checkpoint's generation
    /// must equal the session's current model generation, its boundary must
    /// not exceed the latest completed, failed, or interrupted turn, and
    /// coverage must never regress below an existing checkpoint. Reject stale
    /// publications with [`SessionError::CheckpointStale`] without changing
    /// state. Canonical messages, host requests, and journals remain intact.
    async fn publish_checkpoint(
        &self,
        session_id: &str,
        checkpoint: &crate::store::SessionCheckpoint,
    ) -> Result<(), SessionError>;

    /// Recover expired reservations and return the canonical request outcome
    /// plus its current journal. Never execute a model or tool. Missing host
    /// IDs return `None`; missing sessions return `NotFound`.
    async fn load_request(
        &self,
        session_id: &str,
        host_id: &str,
    ) -> Result<Option<HostTurnRecord>, SessionError>;

    /// Commit the complete result together with messages and terminal state
    /// under live ownership, replacing any recorded progress. An
    /// acknowledgment failure must not overwrite a committed result during
    /// subsequent interrupt/cleanup.
    async fn complete_request(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
        outcome: &TurnOutcome,
    ) -> Result<(), SessionError>;

    /// Renew only an unexpired owner; return a conservative confirmed deadline.
    async fn renew(&self, owner: &TurnOwner) -> Result<Instant, SessionError>;
    /// Atomically appends completed tool exchanges of a running turn under an
    /// unexpired owner, after the turn's last recorded message, so a turn that
    /// later stops replays the work it finished. Completion replaces recorded
    /// progress with the turn's complete messages.
    async fn record_progress(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
    ) -> Result<(), SessionError>;
    /// Atomically commits messages and continuation under an unexpired owner,
    /// replacing any progress recorded for the turn.
    async fn complete_turn(
        &self,
        owner: &TurnOwner,
        messages: &[ModelMessage],
        provider_session_id: Option<&str>,
    ) -> Result<(), SessionError>;
    /// Atomically replaces the [`RejectedContent`] of a running turn under an
    /// unexpired owner with omission placeholders, so a later replay does not
    /// resend content the provider rejected. Recorded tool calls keep their
    /// identifiers, tools, and paths.
    async fn omit_rejected(
        &self,
        owner: &TurnOwner,
        rejected: RejectedContent,
    ) -> Result<(), SessionError>;
    /// Marks an unexpired owned turn failed and clears its continuation,
    /// retaining its input and recorded progress for replay.
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
