//! Shared app-server runtime registry helpers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::app_server::AppServerError;

/// Shared runtime registry used by managed provider processes.
///
/// Each session id maps to one idle runtime process. Workers temporarily remove
/// a runtime while executing a turn and register a cancellation token so
/// `shutdown_session()` can still interrupt in-flight runtimes.
pub(crate) struct AppServerSessionRegistry<Runtime> {
    active_turn_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    provider_name: &'static str,
    sessions: Arc<Mutex<HashMap<String, Runtime>>>,
}

impl<Runtime> AppServerSessionRegistry<Runtime> {
    /// Creates an empty session runtime registry for one provider.
    pub(crate) fn new(provider_name: &'static str) -> Self {
        Self {
            active_turn_cancellations: Arc::new(Mutex::new(HashMap::new())),
            provider_name,
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Removes and returns the runtime stored for `session_id`.
    ///
    /// # Errors
    /// Returns an error when the session map lock is poisoned.
    pub(crate) fn take_session(&self, session_id: &str) -> Result<Option<Runtime>, AppServerError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppServerError::LockPoisoned {
                provider: self.provider_name,
            })?;

        Ok(sessions.remove(session_id))
    }

    /// Stores or replaces the runtime for `session_id`, returning ownership
    /// back to the caller when lock acquisition fails.
    ///
    /// This allows callers to shut down process-backed runtimes before
    /// returning an error, preventing orphaned child processes on early exits.
    ///
    /// # Errors
    /// Returns `(error, session)` when the session map lock is poisoned.
    pub(crate) fn store_session_or_recover(
        &self,
        session_id: String,
        session: Runtime,
    ) -> Result<(), (AppServerError, Runtime)> {
        let Ok(mut sessions) = self.sessions.lock() else {
            return Err((
                AppServerError::LockPoisoned {
                    provider: self.provider_name,
                },
                session,
            ));
        };
        sessions.insert(session_id, session);

        Ok(())
    }

    /// Returns the provider label used in user-facing retry errors.
    pub(crate) fn provider_name(&self) -> &'static str {
        self.provider_name
    }

    /// Registers one in-flight app-server turn and returns its cancellation
    /// guard.
    ///
    /// The returned guard unregisters itself on drop. This keeps
    /// `shutdown_session()` able to signal a runtime even while the runtime is
    /// temporarily owned by the running turn rather than stored in
    /// [`AppServerSessionRegistry::sessions`].
    ///
    /// # Errors
    /// Returns an error when the active-turn map lock is poisoned.
    pub(crate) fn register_active_turn(
        &self,
        session_id: &str,
    ) -> Result<ActiveAppServerTurn, AppServerError> {
        let token = CancellationToken::new();
        let mut active_turn_cancellations =
            self.active_turn_cancellations
                .lock()
                .map_err(|_| AppServerError::LockPoisoned {
                    provider: self.provider_name,
                })?;
        active_turn_cancellations.insert(session_id.to_string(), token.clone());

        Ok(ActiveAppServerTurn {
            active_turn_cancellations: Arc::clone(&self.active_turn_cancellations),
            session_id: session_id.to_string(),
            token,
        })
    }

    /// Signals an active app-server turn for `session_id`, if one is currently
    /// registered.
    ///
    /// # Errors
    /// Returns an error when the active-turn map lock is poisoned.
    pub(crate) fn cancel_active_turn(&self, session_id: &str) -> Result<bool, AppServerError> {
        let active_turn_cancellations =
            self.active_turn_cancellations
                .lock()
                .map_err(|_| AppServerError::LockPoisoned {
                    provider: self.provider_name,
                })?;

        let Some(token) = active_turn_cancellations.get(session_id) else {
            return Ok(false);
        };

        token.cancel();

        Ok(true)
    }
}

/// Clones the registry handle by sharing the same underlying session map.
impl<Runtime> Clone for AppServerSessionRegistry<Runtime> {
    fn clone(&self) -> Self {
        Self {
            active_turn_cancellations: Arc::clone(&self.active_turn_cancellations),
            provider_name: self.provider_name,
            sessions: Arc::clone(&self.sessions),
        }
    }
}

/// RAII guard for one app-server turn cancellation registration.
pub(crate) struct ActiveAppServerTurn {
    active_turn_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    session_id: String,
    token: CancellationToken,
}

impl ActiveAppServerTurn {
    /// Returns the cancellation token observed by the running turn.
    pub(crate) fn token(&self) -> CancellationToken {
        self.token.clone()
    }
}

impl Drop for ActiveAppServerTurn {
    fn drop(&mut self) {
        if let Ok(mut active_turn_cancellations) = self.active_turn_cancellations.lock() {
            active_turn_cancellations.remove(&self.session_id);
        }
    }
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
