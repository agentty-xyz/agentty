//! Panel state for reading a session's shared notes document, whose one
//! action clears it after a typed confirmation.

use ag_session::{SessionDocument, SessionId};

use super::app_mode::{AppMode, ConfirmationViewMode};
use super::execution_graph::GraphView;
use crate::domain::input::InputState;

/// Word the user types to confirm clearing the session notes.
pub const CLEAR_CONFIRMATION_WORD: &str = "clear";

/// State of the session notes panel.
pub struct SessionDocumentPanel {
    /// Typed confirmation while the user is clearing the document.
    pub clear_confirmation: Option<ClearConfirmation>,
    /// Document loaded when the panel opened or last reloaded, or why it
    /// could not be read.
    pub document: Result<SessionDocument, String>,
    /// Outcome of the last action, shown above the entries.
    pub notice: Option<String>,
    /// Page restored when the panel closes.
    pub restore: SessionDocumentRestore,
    /// Vertical panel-content scroll offset.
    pub scroll_offset: u16,
}

impl SessionDocumentPanel {
    /// Returns a panel showing `document` above the page `restore` reopens.
    pub fn new(document: Result<SessionDocument, String>, restore: SessionDocumentRestore) -> Self {
        Self {
            clear_confirmation: None,
            document,
            notice: None,
            restore,
            scroll_offset: 0,
        }
    }

    /// Returns the version of the loaded document when it holds anything
    /// that clearing would remove.
    pub fn clearable_version(&self) -> Option<u64> {
        self.document
            .as_ref()
            .ok()
            .filter(|document| !document.body.is_empty())
            .map(|document| document.version)
    }

    /// Shows `document`, reloaded while the panel is open. A confirmation
    /// opened on another version closes with a notice, so a clear never
    /// removes notes the user has not seen.
    pub fn reload(&mut self, document: Result<SessionDocument, String>) {
        let version = document.as_ref().ok().map(|document| document.version);
        if self
            .clear_confirmation
            .as_ref()
            .is_some_and(|confirmation| Some(confirmation.version) != version)
        {
            self.clear_confirmation = None;
            self.notice = Some(SESSION_NOTES_CHANGED_NOTICE.to_string());
        }
        self.document = document;
    }
}

/// Notice shown when the notes change under an open clear confirmation, or
/// before a clear lands.
pub const SESSION_NOTES_CHANGED_NOTICE: &str =
    "The session notes changed meanwhile; review them and clear again.";

/// The typed confirmation that clears the session notes.
pub struct ClearConfirmation {
    /// Word typed so far.
    pub input: InputState,
    /// Document version shown when the confirmation opened, which the
    /// clear requires to still be current.
    pub version: u64,
}

/// Page restored when the session notes panel closes.
pub enum SessionDocumentRestore {
    /// Restore the graph inspector that pressed `N`, with its selection and
    /// any retained editor.
    Graph(Box<GraphView>),
    /// Restore the session view that pressed `N`.
    View(ConfirmationViewMode),
}

impl SessionDocumentRestore {
    /// Returns the session whose notes the panel shows.
    pub fn session_id(&self) -> &SessionId {
        match self {
            Self::Graph(view) => &view.restore.session_id,
            Self::View(view) => &view.session_id,
        }
    }

    /// Returns the transcript scroll position of the session view behind
    /// the panel, or that the graph inspector restores.
    pub fn scroll_offset(&self) -> Option<usize> {
        match self {
            Self::Graph(view) => view.restore.scroll_offset,
            Self::View(view) => view.scroll_offset,
        }
    }

    /// Restores the originating page.
    #[must_use]
    pub fn into_mode(self) -> AppMode {
        match self {
            Self::Graph(view) => AppMode::ExecutionGraph(view),
            Self::View(view) => view.into_view_mode(),
        }
    }
}
