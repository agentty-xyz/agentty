//! Model admission fencing and immutable execution provenance.

use std::borrow::Cow;

#[cfg(feature = "sqlite")]
use serde::Deserialize;
use serde_json::Value;

use crate::SessionError;
use crate::context::{CLEARED_REASONING, HistoryTurn};
use crate::model::{ModelCapabilities, ModelMessage};
use crate::recovery::ExecutionIdentity;
use crate::tool::ToolCall;

/// Leads reasoning that another provider's model wrote, replayed as an
/// assistant note.
const CARRIED_REASONING_HEADING: &str =
    "[harness] Reasoning an earlier model wrote at this step, kept as a note:\n";

/// Replaces reasoning that another provider's model wrote but that has no
/// readable text, such as encrypted reasoning.
const OMITTED_REASONING_NOTE: &str =
    "[harness] An earlier model reasoned at this step; that reasoning is not readable here.";

/// Most reasoning bytes one note carries, so carried reasoning stays a short
/// note rather than replaying a whole reasoning trace as visible text.
const MAX_NOTE_REASONING_BYTES: usize = 4_096;

/// Ends a note whose reasoning was cut to [`MAX_NOTE_REASONING_BYTES`].
const CUT_REASONING_SUFFIX: &str = "\n[harness] The rest of this reasoning was cut.";

/// Model selected when a turn was reserved. Legacy turns have no snapshot.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedModel {
    /// Monotonic session selection generation, independent of request identity.
    pub generation: i64,
    /// Model name, absent for models without metadata.
    pub model: Option<String>,
    /// Provider name, paired with the model name.
    pub provider: Option<String>,
    /// Stable host registration, absent for direct models.
    pub registration_identity: Option<ExecutionIdentity>,
}

impl RecordedModel {
    #[cfg(feature = "sqlite")]
    pub(crate) fn decode(value: &str) -> Result<Self, SessionError> {
        let snapshot: Snapshot =
            serde_json::from_str(value).map_err(|error| SessionError::InvalidData {
                reason: error.to_string(),
            })?;
        let registration_identity = match (snapshot.key, snapshot.revision) {
            (None, None) => None,
            (Some(key), Some(revision)) => Some(ExecutionIdentity::new(key, revision)?),
            _ => return Err(invalid_snapshot()),
        };
        match (&snapshot.provider, &snapshot.model) {
            (None, None) => {}
            (Some(provider), Some(model))
                if !provider.trim().is_empty() && !model.trim().is_empty() => {}
            _ => return Err(invalid_snapshot()),
        }
        if snapshot.generation < 0 {
            return Err(invalid_snapshot());
        }

        Ok(Self {
            generation: snapshot.generation,
            registration_identity,
            model: snapshot.model,
            provider: snapshot.provider,
        })
    }
}

pub(crate) fn check_generation(id: &str, actual: i64, expected: i64) -> Result<(), SessionError> {
    if actual != expected {
        return Err(SessionError::StaleModel { id: id.to_string() });
    }

    Ok(())
}

pub(crate) fn next_generation(generation: i64) -> Result<i64, SessionError> {
    generation.checked_add(1).ok_or_else(invalid_snapshot)
}

pub(crate) fn check_history<'a>(
    messages: impl Iterator<Item = &'a ModelMessage>,
    capabilities: ModelCapabilities,
) -> Result<(), SessionError> {
    for message in messages {
        match message {
            ModelMessage::AssistantToolCall(_)
            | ModelMessage::AssistantToolCalls(_)
            | ModelMessage::ToolResult { .. }
                if !capabilities.tool_calls =>
            {
                return Err(SessionError::UnsupportedModelHistory {
                    reason: "target does not support tool history",
                });
            }
            ModelMessage::UserInput(input) if input.has_images() && !capabilities.image_input => {
                return Err(SessionError::UnsupportedModelHistory {
                    reason: "target does not support image history",
                });
            }
            _ => {}
        }
    }

    Ok(())
}

/// Returns finished `turn` as a request to a model of `target_provider`
/// replays it.
///
/// A turn recorded under the same provider name, or without a model
/// snapshot, keeps its provider reasoning for native replay. A turn another
/// provider produced, or a model without metadata when the target has it,
/// is projected deterministically: each final answer's reasoning becomes an
/// assistant note before the answer, and each tool call loses its reasoning,
/// which leads its assistant text as a note instead, while its identifier,
/// name, arguments, and result stay unchanged. Readable reasoning, plain text
/// or a `Responses` reasoning summary, is carried as bounded text; encrypted
/// or cleared reasoning becomes a placeholder note. Projection happens only
/// while building requests; stored turns stay native, so switching back
/// replays their reasoning natively again.
pub(crate) fn project_turn<'turn>(
    turn: &'turn HistoryTurn,
    target_provider: Option<&str>,
) -> Cow<'turn, HistoryTurn> {
    let native = turn
        .model
        .as_ref()
        .is_none_or(|model| model.provider.as_deref() == target_provider);
    if native {
        return Cow::Borrowed(turn);
    }
    let mut messages = Vec::with_capacity(turn.messages.len());
    for message in &turn.messages {
        match message {
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => {
                messages.push(ModelMessage::Assistant(reasoning_note(reasoning_content)));
                messages.push(ModelMessage::Assistant(content.clone()));
            }
            ModelMessage::AssistantToolCall(call) => {
                messages.push(ModelMessage::AssistantToolCall(projected_call(call)));
            }
            ModelMessage::AssistantToolCalls(calls) => {
                messages.push(ModelMessage::AssistantToolCalls(
                    calls.iter().map(projected_call).collect(),
                ));
            }
            other => messages.push(other.clone()),
        }
    }

    Cow::Owned(HistoryTurn {
        messages,
        model: turn.model.clone(),
        stop: turn.stop.clone(),
    })
}

#[cfg(feature = "sqlite")]
#[derive(Deserialize)]
struct Snapshot {
    generation: i64,
    key: Option<String>,
    model: Option<String>,
    provider: Option<String>,
    revision: Option<String>,
}

fn invalid_snapshot() -> SessionError {
    SessionError::InvalidData {
        reason: "invalid model selection snapshot".into(),
    }
}

/// Returns the note that replaces `reasoning` for another provider: its
/// readable text under a heading, cut to [`MAX_NOTE_REASONING_BYTES`], or a
/// placeholder when nothing readable remains.
fn reasoning_note(reasoning: &str) -> String {
    let readable = readable_reasoning(reasoning);
    let text = readable.trim();
    if text.is_empty() {
        return OMITTED_REASONING_NOTE.to_string();
    }
    if text.len() <= MAX_NOTE_REASONING_BYTES {
        return format!("{CARRIED_REASONING_HEADING}{text}");
    }
    let end = text.floor_char_boundary(MAX_NOTE_REASONING_BYTES);

    format!(
        "{CARRIED_REASONING_HEADING}{}{CUT_REASONING_SUFFIX}",
        &text[..end]
    )
}

/// Returns the readable text of stored reasoning: the summaries of
/// `Responses` reasoning items, which otherwise carry only encrypted
/// content, or plain text as written. Reasoning cleared to fit the context
/// window has no readable text.
fn readable_reasoning(reasoning: &str) -> String {
    if reasoning == CLEARED_REASONING {
        return String::new();
    }
    let Some(items) = serde_json::from_str::<Vec<Value>>(reasoning)
        .ok()
        .filter(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
    else {
        return reasoning.to_string();
    };
    let summaries: Vec<&str> = items
        .iter()
        .filter_map(|item| item.get("summary").and_then(Value::as_array))
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();

    summaries.join("\n\n")
}

fn projected_call(call: &ToolCall) -> ToolCall {
    call.without_reasoning(call.reasoning_content().map(reasoning_note))
}

#[cfg(test)]
#[path = "session_model_test.rs"]
mod tests;
