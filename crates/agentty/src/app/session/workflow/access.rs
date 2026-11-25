//! Shared session lookup helpers using typed [`SessionError`] results.
//!
//! Every helper resolves a session by its stable identifier on any loaded
//! page, never by a list position, so a position on one page is never used on
//! another.

use crate::app::SessionManager;
use crate::app::session::SessionError;
use crate::domain::session::{Session, SessionHandles};

impl SessionManager {
    /// Resolves an immutable session reference by identifier.
    pub(crate) fn session_or_err(&self, session_id: &str) -> Result<&Session, SessionError> {
        self.session_for_id(session_id)
            .ok_or(SessionError::NotFound)
    }

    /// Resolves a mutable session reference by identifier.
    pub(crate) fn session_mut_or_err(
        &mut self,
        session_id: &str,
    ) -> Result<&mut Session, SessionError> {
        self.state
            .session_mut_for_id(session_id)
            .ok_or(SessionError::NotFound)
    }

    /// Resolves runtime handles for a session identifier.
    pub(crate) fn session_handles_or_err(
        &self,
        session_id: &str,
    ) -> Result<&SessionHandles, SessionError> {
        self.session_handles()
            .get(session_id)
            .ok_or(SessionError::HandlesNotFound)
    }

    /// Resolves both immutable session data and runtime handles together.
    pub(crate) fn session_and_handles_or_err(
        &self,
        session_id: &str,
    ) -> Result<(&Session, &SessionHandles), SessionError> {
        let session = self.session_or_err(session_id)?;
        let handles = self.session_handles_or_err(session_id)?;

        Ok((session, handles))
    }
}

#[cfg(test)]
#[path = "access_test.rs"]
mod tests;
