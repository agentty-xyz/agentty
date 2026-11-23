//! Turn reservation entry points for suites that drive a store directly,
//! without a harness session.

use std::sync::Arc;

use super::super::{AcquiredTurn, reserve};
use crate::admission::TurnAdmission;
use crate::input::TurnInput;
use crate::recovery::{HostRequest, HostTurnAcquisition};
use crate::session::{SessionError, TurnOwner};
use crate::store::SessionStore;
use crate::turn::TurnOptions;

/// Reserves turns the way a session does and reads their owner.
pub(crate) trait AcquiredTurnFixture: Sized {
    /// Reserves a turn through `store` under the harness admission rules and
    /// starts lease renewal.
    async fn begin(
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        generation: i64,
    ) -> Result<Self, SessionError>;

    /// Returns the request recorded under `request`'s host ID, or reserves a
    /// turn bound to it like [`Self::begin`].
    async fn begin_request(
        store: Arc<dyn SessionStore>,
        session_id: &str,
        input: &TurnInput,
        options: &TurnOptions,
        request: &HostRequest,
        generation: i64,
    ) -> Result<HostTurnAcquisition, SessionError>;

    /// Returns the reservation identity used for backend lifecycle operations.
    fn owner(&self) -> &TurnOwner;
}

impl AcquiredTurnFixture for AcquiredTurn {
    async fn begin(
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

    async fn begin_request(
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

    fn owner(&self) -> &TurnOwner {
        self.guard.owner()
    }
}
