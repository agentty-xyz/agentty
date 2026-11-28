//! A session's shared notes document: one text that the graph nodes of the
//! session read and add to, with the attributed entries that wrote it.
//!
//! The document holds shared notes only, such as findings, decisions, and
//! hand-offs. Graph structure, executions, and evidence stay in their own
//! records.

use serde::Serialize;

/// Largest note one entry may add, in bytes of UTF-8 text.
pub const SESSION_DOCUMENT_NOTE_MAX_BYTES: usize = 2 * 1024;

/// Largest body the document may hold, in bytes of UTF-8 text, including
/// the heading the host writes above each note.
pub const SESSION_DOCUMENT_BODY_MAX_BYTES: usize = 32 * 1024;

/// Most notes one response may add; the host refuses the rest.
pub const SESSION_DOCUMENT_NOTES_PER_RESPONSE: usize = 5;

/// A session's shared notes document at one version.
///
/// A session that never wrote one reads as the empty document at version
/// zero. Every accepted write advances the version by one.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionDocument {
    /// Current text: every note's section, or the latest replacement and
    /// the sections appended after it.
    pub body: String,
    /// Every accepted write in version order.
    pub entries: Vec<SessionDocumentEntry>,
    /// Version of `body`; zero before the first write.
    pub version: u64,
}

impl SessionDocument {
    /// Returns `text` as the document stores it: without characters that
    /// render as nothing, such as tag, zero-width, and bidirectional control
    /// characters, with every line break as `\n`, tabs as four spaces, and
    /// no outer whitespace. Storage applies it to every note and body before
    /// measuring them, so no writer can hide text from a person reading the
    /// document.
    pub fn visible_text(text: &str) -> String {
        let text = text.replace("\r\n", "\n");
        let mut visible = String::with_capacity(text.len());
        for character in text.chars() {
            match character {
                '\n' | '\r' | '\u{0085}' | '\u{2028}' | '\u{2029}' => visible.push('\n'),
                '\t' => visible.push_str("    "),
                character if ag_protocol::is_hidden_character(character) => {}
                character => visible.push(character),
            }
        }

        visible.trim().to_string()
    }

    /// Returns the section an appended note adds to the body: a heading that
    /// names the note's version and author, then the note's text.
    pub fn note_section(version: u64, author: &SessionDocumentAuthor, text: &str) -> String {
        format!("## Note {version} from {}\n\n{text}", author.label())
    }

    /// Returns the body after appending `section`, separated from earlier
    /// text by a blank line.
    pub fn appended_body(body: &str, section: &str) -> String {
        if body.is_empty() {
            return section.to_string();
        }

        format!("{body}\n\n{section}")
    }

    /// Returns the length of a body of `body_bytes` bytes after appending a
    /// note of `note_bytes` bytes as `version` by `author`.
    pub fn appended_len(
        body_bytes: usize,
        version: u64,
        author: &SessionDocumentAuthor,
        note_bytes: usize,
    ) -> usize {
        let separator = if body_bytes == 0 { 0 } else { 2 };

        body_bytes + separator + Self::note_section(version, author, "").len() + note_bytes
    }
}

/// The part of a session's document that nodes read: the newest
/// replacement and every entry after it, with the body's size.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionDocumentTail {
    /// Length of the current body, in bytes.
    pub body_bytes: usize,
    /// The newest replacement entry, when there is one, then every later
    /// entry, in version order.
    pub entries: Vec<SessionDocumentEntry>,
    /// Current version; zero before the first write.
    pub version: u64,
}

impl SessionDocumentTail {
    /// Whether a one-byte note by `author` would still fit the body.
    pub fn has_room_for(&self, author: &SessionDocumentAuthor) -> bool {
        SessionDocument::appended_len(self.body_bytes, self.version + 1, author, 1)
            <= SESSION_DOCUMENT_BODY_MAX_BYTES
    }
}

/// One accepted write to a session's document.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionDocumentEntry {
    /// Who wrote it.
    pub author: SessionDocumentAuthor,
    /// When it was written, in Unix seconds.
    pub created_at: i64,
    /// Whether it added a note or replaced the whole body.
    pub kind: SessionDocumentEntryKind,
    /// The note's text, or the complete replacement body.
    pub text: String,
    /// Document version this write produced.
    pub version: u64,
}

impl SessionDocumentEntry {
    /// Returns how the viewer describes what this entry did.
    pub fn action_label(&self) -> &'static str {
        match self.kind {
            SessionDocumentEntryKind::Note => "note",
            SessionDocumentEntryKind::Replace if self.text.is_empty() => "cleared the document",
            SessionDocumentEntryKind::Replace => "replaced the document",
        }
    }
}

/// Author of one session document entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionDocumentAuthor {
    /// A graph node's execution, which the host attributes.
    GraphNode {
        /// Execution that returned the note, such as its task attempt.
        execution_id: String,
        /// Graph that owns the node.
        graph_id: String,
        /// Node within that graph.
        node_id: String,
    },
    /// The session's main chat, such as a turn that coordinates its graphs.
    MainChat,
    /// The session's user.
    User,
}

impl SessionDocumentAuthor {
    /// Returns how the document's headings and its viewer name the author.
    pub fn label(&self) -> String {
        match self {
            Self::GraphNode {
                graph_id, node_id, ..
            } => format!("node `{node_id}` of graph `{graph_id}`"),
            Self::MainChat => "the main chat".to_string(),
            Self::User => "the user".to_string(),
        }
    }

    /// Returns the stored name of the author's kind.
    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::GraphNode { .. } => "graph_node",
            Self::MainChat => "main_chat",
            Self::User => "user",
        }
    }

    /// Returns the execution, graph, and node of a graph node's execution,
    /// or `None` for an author without an execution.
    pub fn execution(&self) -> Option<(&str, &str, &str)> {
        match self {
            Self::GraphNode {
                execution_id,
                graph_id,
                node_id,
            } => Some((execution_id, graph_id, node_id)),
            Self::MainChat | Self::User => None,
        }
    }

    /// Rebuilds a stored author from its kind name and, for a graph node,
    /// its execution, graph, and node, as `(execution, graph, node)`.
    /// Returns `None` for an unknown kind, or when the execution's presence
    /// does not match the kind.
    pub fn from_stored(kind: &str, execution: Option<(String, String, String)>) -> Option<Self> {
        match (kind, execution) {
            ("graph_node", Some((execution_id, graph_id, node_id))) => Some(Self::GraphNode {
                execution_id,
                graph_id,
                node_id,
            }),
            ("main_chat", None) => Some(Self::MainChat),
            ("user", None) => Some(Self::User),
            _ => None,
        }
    }
}

/// What one session document entry did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum SessionDocumentEntryKind {
    /// Added a note to the end of the body.
    Note,
    /// Replaced the whole body at an expected version.
    Replace,
}

impl SessionDocumentEntryKind {
    /// Returns the stored name of the kind.
    pub fn name(self) -> &'static str {
        match self {
            Self::Note => "note",
            Self::Replace => "replace",
        }
    }

    /// Returns the kind a stored name names, or `None` for an unknown name.
    pub fn from_stored(name: &str) -> Option<Self> {
        match name {
            "note" => Some(Self::Note),
            "replace" => Some(Self::Replace),
            _ => None,
        }
    }
}

/// Outcome of appending one note to a session's document.
///
/// Refused notes are not stored at all: storage never drops or shortens a
/// note silently.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDocumentAppend {
    /// The note was added and produced this version.
    Appended {
        /// Version the note produced.
        version: u64,
    },
    /// The note's execution already added notes, by an earlier delivery,
    /// so this delivery adds none, even when its notes differ.
    AlreadyAppended {
        /// Version of the execution's note at this position, or of its
        /// newest note when it added fewer.
        version: u64,
    },
    /// The note is longer than [`SESSION_DOCUMENT_NOTE_MAX_BYTES`].
    NoteTooLong {
        /// Length of the refused note, in bytes.
        bytes: usize,
    },
    /// Adding the note would grow the body past
    /// [`SESSION_DOCUMENT_BODY_MAX_BYTES`].
    DocumentFull {
        /// Length the body would have reached, in bytes.
        bytes: usize,
    },
}

/// Outcome of replacing a session document's whole body.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionDocumentReplace {
    /// The body was replaced and produced this version.
    Replaced {
        /// Version the replacement produced.
        version: u64,
    },
    /// The document is no longer at the expected version, so nothing
    /// changed; read it again before replacing.
    VersionConflict {
        /// The document's current version.
        current: u64,
    },
    /// The replacement is longer than [`SESSION_DOCUMENT_BODY_MAX_BYTES`].
    TooLong {
        /// Length of the refused body, in bytes.
        bytes: usize,
    },
}

#[cfg(test)]
#[path = "session_document_test.rs"]
mod tests;
