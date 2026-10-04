//! Replay notes for durable turns that stopped before producing an answer.

use crate::model::ModelMessage;
use crate::tool::ToolCall;
use crate::turn::sanitize_report_text;

/// Longest stored error classification quoted in a note.
const MAX_REASON_CHARS: usize = 64;

/// Replayed in place of a turn input the provider rejected.
const OMITTED_INPUT: &str = "[Input omitted: the provider rejected the request that contained it.]";

/// Replayed in place of a tool result the provider rejected.
const OMITTED_TOOL_RESULT: &str = "[Result omitted: the tool call ran, but the provider rejected \
                                   the request that contained its result.]";

/// Content of a running turn that its rejected model request introduced.
///
/// When a provider rejects a request as invalid (HTTP 400, 413, or 422),
/// every earlier request of the turn was accepted, so only the content added
/// since then can be the cause. Stores replace that content with omission
/// placeholders through [`crate::store::SessionStore::omit_rejected`], so
/// replaying the stopped turn does not resend it while the tool calls that
/// ran stay visible by identifier, tool, and path.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RejectedContent {
    /// The turn's input, rejected by the turn's first model request.
    Input,
    /// The turn's last `n` recorded tool exchanges, rejected by the request
    /// that followed them: their results and their calls' Bash source,
    /// patches, and reasoning.
    ToolResults(usize),
}

impl RejectedContent {
    /// Replaces the rejected content of `turn`, a turn's input followed by its
    /// recorded tool exchanges, and returns the positions it changed.
    pub fn omit(self, turn: &mut [ModelMessage]) -> Vec<usize> {
        match self {
            Self::Input => match turn.first_mut() {
                Some(input) => {
                    *input = ModelMessage::User(OMITTED_INPUT.to_string());

                    vec![0]
                }
                None => Vec::new(),
            },
            Self::ToolResults(count) => {
                let mut omitted = Vec::new();
                let mut call_ids = Vec::new();
                for (position, message) in turn.iter_mut().enumerate().rev() {
                    if call_ids.len() == count {
                        break;
                    }
                    if let ModelMessage::ToolResult {
                        call_id, content, ..
                    } = message
                    {
                        OMITTED_TOOL_RESULT.clone_into(content);
                        call_ids.push(call_id.clone());
                        omitted.push(position);
                    }
                }
                let rejected = |call: &ToolCall| call_ids.iter().any(|id| id == call.id());
                for (position, message) in turn.iter_mut().enumerate().rev() {
                    match message {
                        ModelMessage::AssistantToolCall(call) if rejected(call) => {
                            *call = call.omitted();
                        }
                        ModelMessage::AssistantToolCalls(calls) if calls.iter().any(rejected) => {
                            for call in calls.iter_mut().filter(|call| rejected(call)) {
                                *call = call.omitted();
                            }
                        }
                        _ => continue,
                    }
                    omitted.push(position);
                }

                omitted
            }
        }
    }
}

/// How a durable turn that never completed ended.
///
/// Stores replay such a turn as its input, the tool exchanges recorded before
/// it stopped, and [`Self::note`], so later turns see work that may have
/// changed the repository even though the turn produced no answer. When the
/// whole turn does not fit the remaining history budget, stores replay only
/// its input and note, and older turns stay eligible. Stopped turns are never
/// replayed as completed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StoppedTurn {
    /// The turn failed, for example on a provider error or invalid output.
    Failed,
    /// The turn was cancelled, dropped, or lost its lease.
    Interrupted,
}

impl StoppedTurn {
    /// Returns the user-role message that ends this turn's replayed history.
    ///
    /// `error_type` is the stored failure classification; it is sanitized and
    /// bounded before it reaches the model.
    pub fn note(self, error_type: &str) -> ModelMessage {
        let tag = self.tag();
        let outcome = match self {
            Self::Failed => "failed",
            Self::Interrupted => "was interrupted",
        };
        let reason: String = sanitize_report_text(error_type.trim())
            .chars()
            .take(MAX_REASON_CHARS)
            .collect();
        let reason = if reason.is_empty() {
            "unknown".to_string()
        } else {
            reason
        };

        ModelMessage::User(format!(
            "<{tag}>\nThe previous turn {outcome} before it finished (reason: {reason}) and \
             produced no final answer. Tool calls shown above completed with the results shown; \
             tool calls omitted from this history may also have run. Commands or writes still \
             running when it stopped may have partially executed, so check the current repository \
             state before relying on them.\n</{tag}>"
        ))
    }

    /// Returns a replayed stopped turn reduced to its input and note, or
    /// `None` when `turn` does not end with a stop note.
    pub(crate) fn input_and_note(turn: &[ModelMessage]) -> Option<Vec<ModelMessage>> {
        match turn {
            [input, .., note] if Self::is_note(note) => Some(vec![input.clone(), note.clone()]),
            _ => None,
        }
    }

    fn tag(self) -> &'static str {
        match self {
            Self::Failed => "turn_failed",
            Self::Interrupted => "turn_aborted",
        }
    }

    fn is_note(message: &ModelMessage) -> bool {
        let ModelMessage::User(text) = message else {
            return false;
        };

        [Self::Failed, Self::Interrupted]
            .into_iter()
            .any(|stopped| text.starts_with(&format!("<{}>\n", stopped.tag())))
    }
}

#[cfg(test)]
#[path = "stopped_turn_test.rs"]
mod tests;
