//! Draft-session serialization helpers for staged attachment metadata.
//!
//! Staged attachments persist in the session row's `draft_attachments`
//! column next to the staged prompt, so draft sessions keep their images
//! across restarts without writing files into the session worktree.

use crate::app::session::SessionError;
use crate::domain::turn_prompt::TurnPromptAttachment;

/// Parses the persisted staged draft attachments for one session.
///
/// Missing or invalid metadata is treated as empty so a stale value does not
/// block session loading.
pub(super) fn parse_staged_draft_attachments(
    serialized_attachments: Option<&str>,
) -> Vec<TurnPromptAttachment> {
    serialized_attachments
        .and_then(|serialized_attachments| serde_json::from_str(serialized_attachments).ok())
        .unwrap_or_default()
}

/// Serializes staged draft attachments for the session row.
///
/// An empty slice maps to `None` so the column is cleared instead of storing
/// an empty list.
///
/// # Errors
/// Returns an error if an attachment cannot be represented as JSON, such as
/// a non-UTF-8 image path.
pub(super) fn serialize_staged_draft_attachments(
    attachments: &[TurnPromptAttachment],
) -> Result<Option<String>, SessionError> {
    if attachments.is_empty() {
        return Ok(None);
    }

    serde_json::to_string(attachments)
        .map(Some)
        .map_err(|error| {
            SessionError::Workflow(format!("Failed to serialize draft attachments: {error}"))
        })
}

#[cfg(test)]
#[path = "draft_test.rs"]
mod tests;
