//! Model admission fencing and immutable execution provenance.

use std::borrow::Cow;

use ag_router::ReasoningView;
#[cfg(feature = "sqlite")]
use serde::Deserialize;
use serde_json::Value;

use crate::SessionError;
use crate::context::{CLEARED_REASONING, HistoryTurn};
use crate::model::{ModelCapabilities, ModelMessage, ModelMetadata};
use crate::recovery::ExecutionIdentity;
use crate::tool::ToolCall;

/// Leads plain-text reasoning that a model of another reasoning family
/// wrote, carried as one JSON string so the text cannot pass for harness
/// notes or end the note early.
const CARRIED_TEXT_HEADING: &str =
    "[harness] Reasoning an earlier model wrote at this step (JSON string):\n";

/// Leads the summary of encrypted `Responses` reasoning, carried like
/// [`CARRIED_TEXT_HEADING`].
const CARRIED_SUMMARY_HEADING: &str =
    "[harness] Summary of reasoning an earlier model wrote at this step (JSON string):\n";

/// Replaces encrypted reasoning without a summary.
const OMITTED_REASONING_NOTE: &str =
    "[harness] An earlier model reasoned at this step; that reasoning is encrypted and not shown.";

/// Replaces a reasoning note the context window had no room for.
const CLEARED_REASONING_NOTE: &str = "[harness] An earlier model reasoned at this step; that \
                                      reasoning was cleared to fit the context window.";

/// Most reasoning bytes one note carries, so a note stays short rather than
/// replaying a whole reasoning trace.
const MAX_NOTE_REASONING_BYTES: usize = 1_024;

/// Follows the JSON string of a note whose reasoning was cut to
/// [`MAX_NOTE_REASONING_BYTES`].
const CUT_REASONING_SUFFIX: &str = "\n[harness] The rest of this reasoning was cut.";

/// Separates a projected note from the assistant text of its tool call.
const NOTE_SEPARATOR: &str = "\n\n";

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

/// The model a request replays history to, as [`project_turn`] needs it.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct ReplayTarget {
    /// Identity and reasoning family of the request's model (see
    /// [`ModelMetadata::reasoning_family`]); `None` without metadata.
    metadata: Option<ModelMetadata>,
    /// Whether the model replays plain-text reasoning natively, so projected
    /// reasoning stays reasoning instead of becoming assistant text.
    reasoning_text: bool,
    /// Whether the session switched models since it was created; until then
    /// a turn without a model snapshot is the current model's own.
    switched: bool,
}

impl ReplayTarget {
    /// Describes a request to the model of `metadata`, in a session at model
    /// selection `generation`, that drops plain-text reasoning (see
    /// [`Self::with_reasoning_text`]).
    pub(crate) fn new(metadata: Option<&ModelMetadata>, generation: i64) -> Self {
        Self {
            metadata: metadata.cloned(),
            reasoning_text: false,
            switched: generation != 0,
        }
    }

    /// Returns this target replaying plain-text reasoning natively when
    /// `reasoning_text` holds.
    #[must_use]
    pub(crate) fn with_reasoning_text(mut self, reasoning_text: bool) -> Self {
        self.reasoning_text = reasoning_text;
        self
    }

    /// Returns whether `turn` replays its reasoning natively: a turn of the
    /// same reasoning family, or a turn without a snapshot before any switch.
    /// After a switch, a turn without a snapshot may hold any provider's
    /// reasoning, so it is projected. A turn recorded without metadata
    /// replays natively only to a model without metadata.
    pub(crate) fn replays_natively(&self, turn: &HistoryTurn) -> bool {
        let Some(recorded) = &turn.model else {
            return !self.switched;
        };
        match (&self.metadata, &recorded.provider, &recorded.model) {
            (Some(metadata), Some(provider), Some(model)) => {
                metadata.shares_reasoning_with(provider, model)
            }
            (None, None, None) => true,
            _ => false,
        }
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

/// Returns finished `turn` as a request to `target` replays it.
///
/// A turn of the target's reasoning family keeps its provider reasoning for
/// native replay (see [`ReplayTarget`]). Another turn is projected
/// deterministically, and its tool calls keep their identifiers, arguments,
/// and results. Each reasoning becomes a note: readable reasoning, plain text
/// or a `Responses` summary, is carried as a JSON string of at most
/// [`MAX_NOTE_REASONING_BYTES`]; encrypted reasoning becomes a placeholder.
/// A target that replays plain-text reasoning receives the note as the
/// message's reasoning, as its own reasoning would replay. For any other
/// target, a final answer's note becomes an assistant message before the
/// answer, and a tool call's note leads its assistant text.
///
/// Projection commutes with clearing reasoning (see [`clears_reasoning`]
/// and [`cleared_note_text`]): projecting an elided turn equals eliding the
/// projected turn, so an elided prefix replays identically on later turns.
/// Projection happens only while building requests; stored turns stay
/// native, so switching back replays their reasoning natively again.
pub(crate) fn project_turn<'turn>(
    turn: &'turn HistoryTurn,
    target: &ReplayTarget,
) -> Cow<'turn, HistoryTurn> {
    if target.replays_natively(turn) {
        return Cow::Borrowed(turn);
    }
    let mut messages = Vec::with_capacity(turn.messages.len());
    for message in &turn.messages {
        match message {
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } if target.reasoning_text => messages.push(ModelMessage::AssistantReasoning {
                content: content.clone(),
                reasoning_content: reasoning_note(reasoning_content, true),
            }),
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => {
                messages.push(ModelMessage::Assistant(reasoning_note(
                    reasoning_content,
                    false,
                )));
                messages.push(ModelMessage::Assistant(content.clone()));
            }
            ModelMessage::AssistantToolCall(call) => {
                messages.push(ModelMessage::AssistantToolCall(projected_call(
                    call, target,
                )));
            }
            ModelMessage::AssistantToolCalls(calls) => {
                messages.push(ModelMessage::AssistantToolCalls(
                    calls
                        .iter()
                        .map(|call| projected_call(call, target))
                        .collect(),
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

/// Returns whether clearing `reasoning` to fit the context window shortens
/// it: native reasoning longer than [`CLEARED_REASONING`], or a projected
/// note whose source reasoning was, so clearing a projected turn matches
/// projecting a cleared one.
pub(crate) fn clears_reasoning(reasoning: &str) -> bool {
    match parse_note(reasoning) {
        Some((clearable, "")) => clearable,
        _ => reasoning.len() > CLEARED_REASONING.len(),
    }
}

/// Returns `text` with the projected reasoning note it starts with cleared,
/// keeping any assistant text after it, or `None` when `text` does not start
/// with a clearable note. `whole` requires the note to be all of `text`, as
/// for the assistant message a projected final answer's note becomes.
pub(crate) fn cleared_note_text(text: &str, whole: bool) -> Option<String> {
    let (clearable, rest) = parse_note(text)?;
    let fits = if whole {
        rest.is_empty()
    } else {
        rest.is_empty() || rest.starts_with(NOTE_SEPARATOR)
    };

    (clearable && fits).then(|| format!("{CLEARED_REASONING_NOTE}{rest}"))
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

/// Returns the note that replaces `reasoning` for a model of another
/// family, as reasoning when `as_reasoning` holds or as assistant text
/// otherwise. Reasoning already cleared stays cleared. Reasoning no longer
/// than [`CLEARED_REASONING`], which clearing keeps, is read as plain text
/// whatever its form, so it is carried in either order of projecting and
/// clearing.
fn reasoning_note(reasoning: &str, as_reasoning: bool) -> String {
    if reasoning == CLEARED_REASONING {
        let cleared = if as_reasoning {
            CLEARED_REASONING
        } else {
            CLEARED_REASONING_NOTE
        };
        return cleared.to_string();
    }
    let view = if reasoning.len() > CLEARED_REASONING.len() {
        ag_router::reasoning_view(reasoning)
    } else {
        ReasoningView::Text(reasoning)
    };
    match view {
        ReasoningView::Text(text) => carried_note(CARRIED_TEXT_HEADING, text),
        ReasoningView::Summary(summary) => carried_note(CARRIED_SUMMARY_HEADING, &summary),
        ReasoningView::Opaque => OMITTED_REASONING_NOTE.to_string(),
    }
}

/// Returns `text` under `heading` as one JSON string of at most
/// [`MAX_NOTE_REASONING_BYTES`] of it, followed by
/// [`CUT_REASONING_SUFFIX`] when cut.
fn carried_note(heading: &str, text: &str) -> String {
    let end = text.floor_char_boundary(MAX_NOTE_REASONING_BYTES);
    let carried = Value::String(text[..end].to_string());
    let suffix = if end < text.len() {
        CUT_REASONING_SUFFIX
    } else {
        ""
    };

    format!("{heading}{carried}{suffix}")
}

/// Reads the projected reasoning note `text` starts with and returns
/// whether clearing it matches clearing its source reasoning, with the text
/// after the note; `None` when `text` starts with no such note. Summaries
/// and placeholders stand for reasoning items longer than
/// [`CLEARED_REASONING`]; carried plain text, cut or not, is as long as its
/// source at least.
fn parse_note(text: &str) -> Option<(bool, &str)> {
    if let Some(rest) = text.strip_prefix(OMITTED_REASONING_NOTE) {
        return Some((true, rest));
    }
    if let Some(rest) = text.strip_prefix(CLEARED_REASONING_NOTE) {
        return Some((false, rest));
    }
    let (summary, rest) = match text.strip_prefix(CARRIED_SUMMARY_HEADING) {
        Some(rest) => (true, rest),
        None => (false, text.strip_prefix(CARRIED_TEXT_HEADING)?),
    };
    let mut strings = serde_json::Deserializer::from_str(rest).into_iter::<String>();
    let carried = strings.next()?.ok()?;
    let rest = &rest[strings.byte_offset()..];
    let rest = rest.strip_prefix(CUT_REASONING_SUFFIX).unwrap_or(rest);

    Some((summary || carried.len() > CLEARED_REASONING.len(), rest))
}

/// Returns `call` as a request to `target` replays it when another family
/// wrote it: its reasoning becomes a note, kept as reasoning for a target
/// that replays plain-text reasoning, or leading its assistant text.
fn projected_call(call: &ToolCall, target: &ReplayTarget) -> ToolCall {
    let Some(reasoning) = call.reasoning_content() else {
        return call.clone();
    };
    let text = call.assistant_text().map(str::to_string);
    if target.reasoning_text {
        return call.replayed_as(Some(reasoning_note(reasoning, true)), text);
    }
    let note = reasoning_note(reasoning, false);
    let text = match text {
        Some(text) => format!("{note}{NOTE_SEPARATOR}{text}"),
        None => note,
    };

    call.replayed_as(None, Some(text))
}

#[cfg(test)]
#[path = "session_model_test.rs"]
mod tests;
