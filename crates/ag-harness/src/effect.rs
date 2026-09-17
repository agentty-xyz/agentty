//! Turn-local completion observation for retained filesystem replacements.

use std::sync::{Arc, Mutex, OnceLock};

use thiserror::Error;
use tokio::sync::{OwnedMutexGuard, watch};

/// A managed write could not fully acknowledge completion or its journal
/// outcome.
#[derive(Clone, Debug, Error)]
#[error("managed filesystem effect settlement failed: {message}")]
pub struct EffectSettlementError {
    message: String,
    unresolved: bool,
}

impl EffectSettlementError {
    /// Whether filesystem completion itself is unknown. Such a turn keeps local
    /// admission blocked for the lifetime of this process. A recording failure
    /// alone does not imply that the filesystem operation is still running.
    pub fn is_unresolved(&self) -> bool {
        self.unresolved
    }
}

type Admission = Arc<OwnedMutexGuard<()>>;

#[derive(Clone)]
pub(crate) struct Effects(watch::Sender<State>);

impl Effects {
    pub(crate) fn retain(&self) -> EffectLease {
        self.0.send_modify(|state| state.pending += 1);

        EffectLease {
            effects: self.clone(),
            recording: false,
            unresolved: false,
        }
    }

    pub(crate) fn admit(&self, admission: Admission) {
        self.0
            .send_modify(|state| state.admission = Some(admission));
    }

    pub(crate) async fn settled(&self) -> Result<(), EffectSettlementError> {
        let mut receiver = self.0.subscribe();
        loop {
            {
                let state = receiver.borrow_and_update();
                if state.pending == 0 {
                    return state.failure.clone().map_or(Ok(()), Err);
                }
            }
            let _ = receiver.changed().await;
        }
    }

    pub(crate) fn recording_failed(&self, error: &impl std::fmt::Display) {
        self.0.send_modify(|state| {
            state.failure = Some(EffectSettlementError {
                message: crate::schema_contract::bounded_diagnostic(error),
                unresolved: false,
            });
        });
    }
}

impl Default for Effects {
    fn default() -> Self {
        Self(watch::Sender::new(State::default()))
    }
}

#[derive(Default)]
struct State {
    admission: Option<Admission>,
    failure: Option<EffectSettlementError>,
    pending: usize,
}

pub(crate) struct EffectLease {
    effects: Effects,
    recording: bool,
    unresolved: bool,
}

impl EffectLease {
    pub(crate) fn starting(&mut self) {
        self.unresolved = true;
    }

    pub(crate) fn acknowledged(&mut self) {
        self.unresolved = false;
        self.recording = true;
    }

    pub(crate) fn recorded(&mut self) {
        self.recording = false;
    }
}

impl Drop for EffectLease {
    fn drop(&mut self) {
        if self.recording {
            self.effects
                .recording_failed(&"outcome recording worker ended without acknowledgment");
        }
        self.effects.0.send_modify(|state| {
            if self.unresolved {
                state.failure = Some(EffectSettlementError {
                    message: "replacement worker ended without acknowledging filesystem completion"
                        .into(),
                    unresolved: true,
                });
                // A failed worker may have detached blocking work. Retain the
                // admission even if every host control is dropped; no retry can
                // establish that this unknown operation stopped.
                if let Some(admission) = state.admission.take() {
                    static UNRESOLVED: OnceLock<Mutex<Vec<Admission>>> = OnceLock::new();
                    UNRESOLVED
                        .get_or_init(Mutex::default)
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .push(admission);
                }
            }
            state.pending -= 1;
            if state.pending == 0 {
                state.admission.take();
            }
        });
    }
}

#[cfg(test)]
#[path = "effect_test.rs"]
mod tests;
