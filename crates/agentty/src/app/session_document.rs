//! The panel that shows a session's shared notes document: opening it,
//! reloading it when the session changes, and its one action, clearing the
//! document.
//!
//! Graph nodes write the document; the user can only clear it, which
//! records an empty replacement authored by the user.

use std::collections::HashSet;

use ag_session::{SessionDocument, SessionDocumentAuthor, SessionDocumentReplace, SessionId};

use super::App;
use crate::infra::db::AppRepositories;
use crate::presentation::app_mode::AppMode;
use crate::presentation::session_document::{
    SESSION_NOTES_CHANGED_NOTICE, SessionDocumentPanel, SessionDocumentRestore,
};

impl App {
    /// Opens the notes panel for the session `restore` returns to, loading
    /// its document; a failed read opens the panel with the reason instead.
    pub(crate) async fn open_session_document(&mut self, restore: SessionDocumentRestore) {
        let document = load_panel_document(self.services.db(), restore.session_id()).await;
        self.mode =
            AppMode::SessionDocument(Box::new(SessionDocumentPanel::new(document, restore)));
    }

    /// Reloads the open notes panel when its session is among
    /// `session_ids`, so notes a node publishes while it is open appear.
    pub(crate) async fn refresh_notes_panel(&mut self, session_ids: &HashSet<SessionId>) {
        let AppMode::SessionDocument(panel) = &mut self.mode else {
            return;
        };
        if !session_ids.contains(panel.restore.session_id()) {
            return;
        }
        let document = load_panel_document(self.services.db(), panel.restore.session_id()).await;
        panel.reload(document);
    }

    /// Closes the notes panel back to the view it opened from; other modes
    /// stay as they are.
    pub(crate) fn close_session_document(&mut self) {
        self.mode = match std::mem::replace(&mut self.mode, AppMode::List) {
            AppMode::SessionDocument(panel) => panel.restore.into_mode(),
            mode => mode,
        };
    }

    /// Clears the open panel's document as the user, unless it moved past
    /// `expected_version`, the version the user confirmed, and reloads the
    /// panel with the outcome.
    pub(crate) async fn clear_session_document(&mut self, expected_version: u64) {
        let AppMode::SessionDocument(panel) = &mut self.mode else {
            return;
        };
        let db = self.services.db();
        let session_id = panel.restore.session_id();
        let outcome = db
            .session_documents()
            .replace_session_document(
                session_id,
                &SessionDocumentAuthor::User,
                expected_version,
                "",
            )
            .await;
        let notice = match outcome {
            Ok(SessionDocumentReplace::Replaced { .. }) => "Session notes cleared.".to_string(),
            Ok(_) => SESSION_NOTES_CHANGED_NOTICE.to_string(),
            Err(error) => format!("Could not clear the session notes: {error}"),
        };
        panel.document = load_panel_document(db, session_id).await;
        panel.clear_confirmation = None;
        panel.notice = Some(notice);
    }
}

/// Loads the document the panel shows, or why it could not be read.
async fn load_panel_document(
    db: &AppRepositories,
    session_id: &str,
) -> Result<SessionDocument, String> {
    db.session_documents()
        .load_session_document(session_id)
        .await
        .map_err(|error| error.to_string())
}

#[cfg(test)]
#[path = "session_document_test.rs"]
mod tests;
