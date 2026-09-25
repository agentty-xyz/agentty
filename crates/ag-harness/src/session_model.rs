//! Model admission fencing and immutable execution provenance.

use serde::Deserialize;

use crate::SessionError;
use crate::model::{ModelCapabilities, ModelMessage};
use crate::recovery::ExecutionIdentity;

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
            ModelMessage::AssistantReasoning { .. } => {
                return Err(SessionError::UnsupportedModelHistory {
                    reason: "provider reasoning cannot be transferred between model selections",
                });
            }
            ModelMessage::AssistantToolCall(call) if call.reasoning_content().is_some() => {
                return Err(SessionError::UnsupportedModelHistory {
                    reason: "provider reasoning cannot be transferred between model selections",
                });
            }
            ModelMessage::AssistantToolCalls(calls)
                if calls.iter().any(|call| call.reasoning_content().is_some()) =>
            {
                return Err(SessionError::UnsupportedModelHistory {
                    reason: "provider reasoning cannot be transferred between model selections",
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

#[cfg(test)]
#[path = "session_model_test.rs"]
mod tests;
