//! A graph task node's view of its session document: the prompt section it
//! reads at admission, and the notes its completed turn publishes.

use std::collections::VecDeque;
use std::fmt::Write as _;

use ag_session::{
    SESSION_DOCUMENT_BODY_MAX_BYTES, SESSION_DOCUMENT_NOTE_MAX_BYTES,
    SESSION_DOCUMENT_NOTES_PER_RESPONSE, SessionDocument, SessionDocumentAppend,
    SessionDocumentAuthor, SessionDocumentEntry, SessionDocumentEntryKind, SessionDocumentTail,
    TranscriptNotice,
};
use askama::Template;
use serde_json::Value;

use crate::app::session::SessionError;
use crate::infra::db::AppRepositories;

/// Most bytes of entry text one node prompt carries, each entry counted as
/// its raw text plus [`PROMPT_ENTRY_OVERHEAD_BYTES`]; older entries beyond
/// it are left out whole, and a replacement that does not fit is cut to its
/// newest part.
pub(super) const PROMPT_DOCUMENT_MAX_BYTES: usize = 16 * 1024;

/// Fixed share of [`PROMPT_DOCUMENT_MAX_BYTES`] each entry counts besides
/// its text, for its version, author, and kind. Escaping is not counted,
/// so text that escapes long cannot push older entries out.
pub(super) const PROMPT_ENTRY_OVERHEAD_BYTES: usize = 256;

/// Askama view model for the document section of a graph node prompt.
#[derive(Template)]
#[template(path = "graph_session_document_prompt.md", escape = "none")]
struct SessionDocumentPromptTemplate<'a> {
    /// The fenced entries, or a sentence saying there are none.
    document: &'a str,
    /// Most notes the host takes from one response.
    max_notes: usize,
    /// Longest note the host appends, in bytes.
    note_max_bytes: usize,
    /// What the host says about the document's size and left-out entries.
    status: &'a str,
    /// Version of the document the node reads.
    version: u64,
}

/// Returns the prompt section that shows the document's newest entries to
/// the graph task node `reader` and invites its notes.
///
/// Nodes read entries, not the rendered body: each entry is one JSON object
/// whose version, author, and kind the host wrote, and whose `text` is the
/// author's, so no text can forge a heading, an author, or a host remark.
/// Strings are escaped so they close no tag and hold no mention or image
/// placeholder, and the array is fenced with a delimiter longer than any
/// backtick run inside it. The newest replacement opens the entries unless
/// it cleared the document. When the entries exceed
/// [`PROMPT_DOCUMENT_MAX_BYTES`], the oldest are left out whole, except
/// that the opening replacement keeps its newest part when some of it
/// fits; the host says what it left out.
pub(super) fn prompt_section(tail: &SessionDocumentTail, reader: &SessionDocumentAuthor) -> String {
    let mut entries = tail
        .entries
        .iter()
        .filter(|entry| entry.kind == SessionDocumentEntryKind::Note || !entry.text.is_empty())
        .map(|entry| (entry, entry.text.as_str()))
        .collect::<VecDeque<_>>();
    let mut cost = entries
        .iter()
        .map(|(_, text)| text.len() + PROMPT_ENTRY_OVERHEAD_BYTES)
        .sum::<usize>();
    let mut omitted = 0;
    let mut cut_replacement = None;
    while cost > PROMPT_DOCUMENT_MAX_BYTES
        && let Some((entry, text)) = entries.pop_front()
    {
        cost -= text.len() + PROMPT_ENTRY_OVERHEAD_BYTES;
        let room = PROMPT_DOCUMENT_MAX_BYTES.saturating_sub(cost + PROMPT_ENTRY_OVERHEAD_BYTES);
        if entry.kind == SessionDocumentEntryKind::Replace && room > 0 {
            let newest = newest_part(text, room);
            cut_replacement = Some((newest.len(), text.len()));
            entries.push_front((entry, newest));

            break;
        }
        omitted += 1;
    }
    let document = if entries.is_empty() {
        "The document shows no entries here.".to_string()
    } else {
        let lines = entries
            .iter()
            .map(|(entry, text)| entry_json(entry, text))
            .collect::<Vec<_>>();
        let json = format!("[\n{}\n]", lines.join(",\n"));
        let fence = ag_protocol::diff_fence(&json);

        format!("{fence}json\n{json}\n{fence}")
    };
    let mut status = if tail.has_room_for(reader) {
        format!(
            "It uses {} of its {SESSION_DOCUMENT_BODY_MAX_BYTES} bytes.",
            tail.body_bytes
        )
    } else {
        "It is full: notes added now are refused until the user clears it.".to_string()
    };
    if let Some((shown, full)) = cut_replacement {
        let _ = write!(
            status,
            " The replacement that opens the entries shows only its last {shown} of {full} bytes, \
             to fit this prompt."
        );
    }
    if omitted > 0 {
        let _ = write!(
            status,
            " Entries left out, oldest first, to fit this prompt: {omitted}."
        );
    }
    let template = SessionDocumentPromptTemplate {
        document: &document,
        max_notes: SESSION_DOCUMENT_NOTES_PER_RESPONSE,
        note_max_bytes: SESSION_DOCUMENT_NOTE_MAX_BYTES,
        status: &status,
        version: tail.version,
    };

    template.render().unwrap_or_default()
}

/// Returns the longest end of `text` of at most `max_bytes` bytes that
/// starts on a character boundary.
fn newest_part(text: &str, max_bytes: usize) -> &str {
    let mut start = text.len().saturating_sub(max_bytes);
    while !text.is_char_boundary(start) {
        start += 1;
    }

    &text[start..]
}

/// Returns one entry, showing `text` as its text, as a JSON object on one
/// line.
fn entry_json(entry: &SessionDocumentEntry, text: &str) -> String {
    escaped_json(&serde_json::json!({
        "version": entry.version,
        "author": entry.author,
        "kind": entry.kind,
        "text": text,
    }))
}

/// Renders `value` as compact JSON whose strings use
/// [`ag_protocol::escaped_json_string`].
fn escaped_json(value: &Value) -> String {
    match value {
        Value::Object(fields) => {
            let fields = fields
                .iter()
                .map(|(key, field)| {
                    format!(
                        "{}:{}",
                        ag_protocol::escaped_json_string(key),
                        escaped_json(field)
                    )
                })
                .collect::<Vec<_>>();

            format!("{{{}}}", fields.join(","))
        }
        Value::String(text) => ag_protocol::escaped_json_string(text),
        other => other.to_string(),
    }
}

/// Publishes the `notes` of a graph node's completed turn to its session's
/// document as `author`, the node's execution, in one transaction, and
/// returns one notice summarizing every note the document did not add.
///
/// Notes are read as [`SessionDocument::visible_text`] returns them, as
/// storage keeps them: a blank one carries nothing to add, and only the
/// first [`SESSION_DOCUMENT_NOTES_PER_RESPONSE`] of the rest are
/// considered. An execution adds notes once, so a redelivered response
/// adds none.
///
/// # Errors
/// Returns an error when the document cannot be written.
pub(super) async fn publish_node_notes(
    db: &AppRepositories,
    session_id: &str,
    author: &SessionDocumentAuthor,
    notes: &[String],
) -> Result<Option<String>, SessionError> {
    let mut notes = notes
        .iter()
        .map(|note| SessionDocument::visible_text(note))
        .filter(|note| !note.is_empty())
        .collect::<Vec<_>>();
    let over_limit = notes
        .len()
        .saturating_sub(SESSION_DOCUMENT_NOTES_PER_RESPONSE);
    notes.truncate(SESSION_DOCUMENT_NOTES_PER_RESPONSE);
    if notes.is_empty() {
        return Ok(None);
    }
    let outcomes = db
        .session_documents()
        .append_session_notes(session_id, author, &notes)
        .await?;
    let too_long = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, SessionDocumentAppend::NoteTooLong { .. }))
        .count();
    let full = outcomes
        .iter()
        .filter(|outcome| matches!(outcome, SessionDocumentAppend::DocumentFull { .. }))
        .count();
    let mut reasons = Vec::new();
    if too_long > 0 {
        reasons.push(format!(
            "{too_long} longer than {SESSION_DOCUMENT_NOTE_MAX_BYTES} bytes"
        ));
    }
    if full > 0 {
        reasons.push(format!(
            "{full} because the document is full ({SESSION_DOCUMENT_BODY_MAX_BYTES} bytes); clear \
             it from the notes panel"
        ));
    }
    if over_limit > 0 {
        reasons.push(format!(
            "{over_limit} past the limit of {SESSION_DOCUMENT_NOTES_PER_RESPONSE} notes per \
             response"
        ));
    }
    if reasons.is_empty() {
        return Ok(None);
    }

    Ok(Some(TranscriptNotice::SessionNotesWarning.format(format!(
        "The session notes did not add {} of the {} notes from {}: {}.",
        too_long + full + over_limit,
        notes.len() + over_limit,
        author.label(),
        reasons.join("; ")
    ))))
}

#[cfg(test)]
#[path = "session_document_test.rs"]
mod tests;
