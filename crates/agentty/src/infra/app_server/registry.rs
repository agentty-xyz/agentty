//! Shared app-server runtime registry helpers.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

use crate::infra::app_server::AppServerError;

/// Shared runtime registry used by app-server providers.
///
/// Each session id maps to one idle runtime process. Workers temporarily remove
/// a runtime while executing a turn and register a cancellation token so
/// `shutdown_session()` can still interrupt in-flight runtimes.
pub struct AppServerSessionRegistry<Runtime> {
    active_turn_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    provider_name: &'static str,
    sessions: Arc<Mutex<HashMap<String, Runtime>>>,
}

impl<Runtime> AppServerSessionRegistry<Runtime> {
    /// Creates an empty session runtime registry for one provider.
    pub fn new(provider_name: &'static str) -> Self {
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
    pub fn take_session(&self, session_id: &str) -> Result<Option<Runtime>, AppServerError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppServerError::LockPoisoned {
                provider: self.provider_name,
            })?;

        Ok(sessions.remove(session_id))
    }

    /// Stores or replaces the runtime for `session_id`.
    ///
    /// # Errors
    /// Returns an error when the session map lock is poisoned.
    pub fn store_session(
        &self,
        session_id: String,
        session: Runtime,
    ) -> Result<(), AppServerError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| AppServerError::LockPoisoned {
                provider: self.provider_name,
            })?;
        sessions.insert(session_id, session);

        Ok(())
    }

    /// Stores or replaces the runtime for `session_id`, returning ownership
    /// back to the caller when lock acquisition fails.
    ///
    /// This allows callers to shut down process-backed runtimes before
    /// returning an error, preventing orphaned child processes on early exits.
    ///
    /// # Errors
    /// Returns `(error, session)` when the session map lock is poisoned.
    pub fn store_session_or_recover(
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
    pub fn provider_name(&self) -> &'static str {
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
    pub fn register_active_turn(
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
    pub fn cancel_active_turn(&self, session_id: &str) -> Result<bool, AppServerError> {
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
pub struct ActiveAppServerTurn {
    active_turn_cancellations: Arc<Mutex<HashMap<String, CancellationToken>>>,
    session_id: String,
    token: CancellationToken,
}

impl ActiveAppServerTurn {
    /// Returns the cancellation token observed by the running turn.
    pub fn token(&self) -> CancellationToken {
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
mod tests {
    use super::*;

    #[test]
    fn new_returns_registry_with_provider_name() {
        // Arrange / Act
        let registry = AppServerSessionRegistry::<String>::new("test-provider");

        // Assert
        assert_eq!(registry.provider_name(), "test-provider");
    }

    #[test]
    fn store_and_take_session_round_trips_runtime() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");
        registry
            .store_session("session-1".to_string(), "runtime-a".to_string())
            .expect("store should succeed");

        // Act
        let taken = registry
            .take_session("session-1")
            .expect("take should succeed");

        // Assert
        assert_eq!(taken, Some("runtime-a".to_string()));
    }

    #[test]
    fn take_session_returns_none_for_missing_id() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");

        // Act
        let taken = registry
            .take_session("missing")
            .expect("take should succeed");

        // Assert
        assert_eq!(taken, None);
    }

    #[test]
    fn take_session_removes_entry_from_registry() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");
        registry
            .store_session("session-1".to_string(), "runtime-a".to_string())
            .expect("store should succeed");
        let _ = registry.take_session("session-1");

        // Act
        let second_take = registry
            .take_session("session-1")
            .expect("take should succeed");

        // Assert
        assert_eq!(second_take, None);
    }

    #[test]
    fn store_session_or_recover_stores_runtime_on_healthy_lock() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");

        // Act
        let result =
            registry.store_session_or_recover("session-1".to_string(), "runtime-a".to_string());

        // Assert
        assert!(result.is_ok());
        let taken = registry
            .take_session("session-1")
            .expect("take should succeed");
        assert_eq!(taken, Some("runtime-a".to_string()));
    }

    #[test]
    fn clone_shares_underlying_session_map() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");
        let cloned = registry.clone();
        registry
            .store_session("session-1".to_string(), "runtime-a".to_string())
            .expect("store should succeed");

        // Act
        let taken = cloned
            .take_session("session-1")
            .expect("take on clone should succeed");

        // Assert
        assert_eq!(taken, Some("runtime-a".to_string()));
    }

    #[test]
    fn cancel_active_turn_signals_registered_token() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");
        let active_turn = registry
            .register_active_turn("session-1")
            .expect("register should succeed");
        let token = active_turn.token();

        // Act
        let did_cancel = registry
            .cancel_active_turn("session-1")
            .expect("cancel should succeed");

        // Assert
        assert!(did_cancel);
        assert!(token.is_cancelled());
    }

    #[test]
    fn active_turn_guard_unregisters_on_drop() {
        // Arrange
        let registry = AppServerSessionRegistry::<String>::new("test");
        let active_turn = registry
            .register_active_turn("session-1")
            .expect("register should succeed");

        // Act
        drop(active_turn);
        let did_cancel = registry
            .cancel_active_turn("session-1")
            .expect("cancel should succeed");

        // Assert
        assert!(!did_cancel);
    }
}
