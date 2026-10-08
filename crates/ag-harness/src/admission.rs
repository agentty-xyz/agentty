//! Harness-owned admission rules that stores apply inside their atomic
//! reservation and model-switch sections.
//!
//! A store reads [`AdmissionState`] and hands it to [`TurnAdmission::admit`]
//! or [`ModelSwitch::admit`]; it only records the decision. The harness then
//! builds the lease for a [`ReservedTurn`].

use tokio::time::Instant;

use crate::compaction::SessionCheckpoint;
use crate::input::TurnInput;
use crate::model::{ModelCapabilities, ModelMessage, ModelMetadata};
use crate::recovery::{ExecutionIdentity, HostRequest, HostTurnRecord};
use crate::session::TurnOwner;
use crate::turn_options_snapshot::StoredTurnOptions;
use crate::{SessionError, TurnOptions};

/// Session facts a store reads in the same atomic section that records the
/// admission decision, after recovering turns whose lease expired.
#[derive(Clone, Debug, Default)]
pub struct AdmissionState {
    /// Whether a turn still holds an unexpired reservation.
    pub active_turn: bool,
    /// Options snapshot recorded by the latest completed turn, checked only
    /// to reject an undecodable snapshot; stores that keep no snapshots
    /// report `None`.
    pub latest_options: Option<String>,
    /// Current model selection generation.
    pub model_generation: i64,
    /// Request recorded under [`TurnAdmission::host_id`]; always `None` for
    /// model switches and plain turns.
    pub request: Option<HostTurnRecord>,
    /// Whether any command record blocks admission, as defined by
    /// [`crate::bash::CommandRecord::blocks_admission`].
    pub unresolved_commands: bool,
}

/// One turn reservation evaluated by
/// [`crate::store::SessionStore::reserve_turn`].
pub struct TurnAdmission {
    generation: i64,
    input: TurnInput,
    options: TurnOptions,
    request: Option<HostRequest>,
}

impl TurnAdmission {
    pub(crate) fn new(
        input: TurnInput,
        options: TurnOptions,
        request: Option<HostRequest>,
        generation: i64,
    ) -> Self {
        Self {
            generation,
            input,
            options,
            request,
        }
    }

    /// Host ID whose recorded request belongs in [`AdmissionState::request`].
    pub fn host_id(&self) -> Option<&str> {
        self.request.as_ref().map(HostRequest::id)
    }

    /// Classifies a recorded host request before checking busy state, then
    /// requires the expected model generation, no active turn, and no
    /// unresolved command. The latest completed turn's options snapshot must
    /// still decode.
    ///
    /// # Errors
    /// Returns [`SessionError::HostTurnConflict`] for a reused host ID with a
    /// different request, [`SessionError::StaleModel`] for a switched model,
    /// [`SessionError::Busy`] for an active turn or unresolved command, and
    /// [`SessionError::InvalidData`] for an undecodable options snapshot.
    pub fn admit(
        &self,
        session_id: &str,
        state: AdmissionState,
    ) -> Result<Admission, SessionError> {
        if let Some(request) = &self.request
            && let Some(record) = state.request
        {
            record.check_request(request)?;

            return Ok(Admission::Recorded(record));
        }
        check_idle(session_id, &state, self.generation)?;
        if let Some(snapshot) = &state.latest_options {
            StoredTurnOptions::decode(snapshot)?;
        }

        Ok(Admission::Reserve(NewTurn {
            message: self.input.clone().into_user_message(),
            options: StoredTurnOptions::encode(&self.options),
            request: self.request.clone(),
        }))
    }
}

/// Decision returned by [`TurnAdmission::admit`].
pub enum Admission {
    /// Record this turn as running.
    Reserve(NewTurn),
    /// Return this request unchanged; it must never execute again.
    Recorded(HostTurnRecord),
}

/// Running-turn record admitted by [`TurnAdmission::admit`].
pub struct NewTurn {
    message: ModelMessage,
    options: String,
    request: Option<HostRequest>,
}

impl NewTurn {
    /// User message persisted as the turn's first message.
    pub fn message(&self) -> &ModelMessage {
        &self.message
    }

    /// Encoded options snapshot recorded with the turn.
    pub fn options(&self) -> &str {
        &self.options
    }

    /// Host request bound to the turn, retained unchanged.
    pub fn request(&self) -> Option<&HostRequest> {
        self.request.as_ref()
    }
}

/// Outcome of [`crate::store::SessionStore::reserve_turn`].
pub enum Reservation {
    /// A new running turn.
    Reserved(ReservedTurn),
    /// The recorded request returned by [`Admission::Recorded`].
    Recorded(HostTurnRecord),
}

/// Acknowledged reservation of a [`NewTurn`]; the harness owns its lease.
pub struct ReservedTurn {
    pub(crate) checkpoint: Option<SessionCheckpoint>,
    pub(crate) deadline: Instant,
    pub(crate) owner: TurnOwner,
    pub(crate) turns: Vec<Vec<ModelMessage>>,
}

impl ReservedTurn {
    /// Describes a committed reservation. `deadline` must not exceed the
    /// stored lease expiry; `turns` holds completed history bounded by the
    /// stored budget, oldest first.
    pub fn new(owner: TurnOwner, deadline: Instant, turns: Vec<Vec<ModelMessage>>) -> Self {
        Self {
            checkpoint: None,
            deadline,
            owner,
            turns,
        }
    }

    /// Attaches the session's current compaction checkpoint; `turns` must
    /// then contain only turns after its boundary.
    #[must_use]
    pub fn with_checkpoint(mut self, checkpoint: Option<SessionCheckpoint>) -> Self {
        self.checkpoint = checkpoint;

        self
    }
}

/// Model selection evaluated by [`crate::store::SessionStore::switch_model`].
pub struct ModelSwitch {
    capabilities: ModelCapabilities,
    generation: i64,
    identity: ExecutionIdentity,
    metadata: Option<ModelMetadata>,
}

impl ModelSwitch {
    /// Selects `identity` for a session currently at `generation`, whose
    /// canonical history must suit `capabilities`.
    pub fn new(
        identity: ExecutionIdentity,
        metadata: Option<ModelMetadata>,
        capabilities: ModelCapabilities,
        generation: i64,
    ) -> Self {
        Self {
            capabilities,
            generation,
            identity,
            metadata,
        }
    }

    /// Registration recorded as the session's selection.
    pub fn identity(&self) -> &ExecutionIdentity {
        &self.identity
    }

    /// Provider and model names recorded with the selection.
    pub fn metadata(&self) -> Option<&ModelMetadata> {
        self.metadata.as_ref()
    }

    /// Requires the expected generation, no active turn, and no unresolved
    /// command, and returns the generation to record.
    ///
    /// # Errors
    /// Returns [`SessionError::StaleModel`] for a switched model and
    /// [`SessionError::Busy`] for an active turn or unresolved command.
    pub fn admit(&self, session_id: &str, state: &AdmissionState) -> Result<i64, SessionError> {
        check_idle(session_id, state, self.generation)?;

        crate::session_model::next_generation(self.generation)
    }

    /// Validates canonical messages for the target model. Provider-specific
    /// reasoning is always rejected: it has no cross-model portability
    /// contract.
    ///
    /// # Errors
    /// Returns [`SessionError::UnsupportedModelHistory`] for content the
    /// target cannot replay.
    pub fn check_history<'a>(
        &self,
        messages: impl IntoIterator<Item = &'a ModelMessage>,
    ) -> Result<(), SessionError> {
        crate::session_model::check_history(messages.into_iter(), self.capabilities)
    }
}

fn check_idle(
    session_id: &str,
    state: &AdmissionState,
    generation: i64,
) -> Result<(), SessionError> {
    crate::session_model::check_generation(session_id, state.model_generation, generation)?;
    if state.active_turn || state.unresolved_commands {
        return Err(SessionError::Busy {
            id: session_id.to_string(),
        });
    }

    Ok(())
}

#[cfg(test)]
#[path = "admission_test.rs"]
mod tests;
