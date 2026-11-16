//! Read-only transcripts of a session's settled turns, returned by
//! [`crate::Session::transcript`] for the turns a [`TurnRange`] selects.
//!
//! Like [`crate::event`] items, a transcript carries prompts, model output,
//! and tool results: treat it as session content. [`TurnRecord`] and its
//! parts derive `Serialize`, `Deserialize`, and `JsonSchema`, so the same
//! shape can be sent over a wire protocol.

use std::ops::Range;
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::event::{RunEvent, TurnIndex};
use crate::input::InputBlock;
use crate::model::ModelMessage;
use crate::run::{RunOutput, Usage};
use crate::tool::ToolCall;
use crate::turn::TurnOutcome;

/// Which settled turns a transcript returns, oldest first.
///
/// Only settled turns count: a turn still running is never returned.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum TurnRange {
    /// Every settled turn.
    All,
    /// The newest settled turns, at most this many.
    Last(u64),
    /// The settled turns whose index lies in the half-open range.
    Span(Range<TurnIndex>),
}

impl TurnRange {
    /// Returns the first index the range may select.
    pub fn start(&self) -> TurnIndex {
        match self {
            Self::All | Self::Last(_) => TurnIndex::new(0),
            Self::Span(span) => span.start,
        }
    }

    /// Returns the exclusive index bound, or `None` when unbounded.
    pub fn end(&self) -> Option<TurnIndex> {
        match self {
            Self::All | Self::Last(_) => None,
            Self::Span(span) => Some(span.end),
        }
    }

    /// Returns how many of the newest settled turns between [`Self::start`]
    /// and [`Self::end`] the range keeps, or `None` to keep all of them.
    pub fn newest(&self) -> Option<u64> {
        match self {
            Self::All | Self::Span(_) => None,
            Self::Last(count) => Some(*count),
        }
    }

    /// Returns the range as stored positions: the first, the exclusive end,
    /// and how many newest turns to keep, `-1` for all, as SQLite's `LIMIT`
    /// reads it.
    pub(crate) fn stored_bounds(&self) -> (i64, i64, i64) {
        let position = |index: TurnIndex| i64::try_from(index.get()).unwrap_or(i64::MAX);

        (
            position(self.start()),
            self.end().map_or(i64::MAX, position),
            self.newest()
                .map_or(-1, |count| i64::try_from(count).unwrap_or(i64::MAX)),
        )
    }
}

/// How a settled turn ended.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum TurnStatus {
    /// The turn committed its final output.
    Completed,
    /// The turn failed, for example on a model error or a run limit.
    Failed,
    /// The turn was cancelled or its owner stopped renewing it.
    Interrupted,
}

/// One settled turn as its store recorded it, returned by
/// [`crate::store::SessionStore::load_transcript`].
#[derive(Clone, Debug, PartialEq)]
pub struct StoredTurn {
    /// Content-free classification of a failed or interrupted turn.
    pub error_type: Option<String>,
    /// When the turn settled.
    pub finished_at: SystemTime,
    /// Persisted messages in order, starting with the turn's input.
    pub messages: Vec<ModelMessage>,
    /// Encoded turn-options snapshot, as
    /// [`crate::store::StoredTurnOptions::encode`] writes it; `None` for a
    /// turn recorded without one.
    pub options: Option<String>,
    /// Committed result of a host-request turn, when recorded.
    pub outcome: Option<TurnOutcome>,
    /// Durable position of the turn.
    pub position: i64,
    /// When the turn was admitted.
    pub started_at: SystemTime,
    /// How the turn ended.
    pub status: TurnStatus,
}

/// One settled turn of a session transcript.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TurnRecord {
    /// Content-free classification of a failed or interrupted turn, such as
    /// `cancelled` or `RunLimit(ToolCalls)`; `None` for a completed turn.
    pub error_type: Option<String>,
    /// When the turn settled, to the second for SQLite sessions; on the
    /// wire, milliseconds since the Unix epoch.
    #[schemars(with = "u64")]
    #[serde(
        deserialize_with = "RunEvent::deserialize_at",
        serialize_with = "RunEvent::serialize_at"
    )]
    pub finished_at: SystemTime,
    /// The turn's input blocks in order.
    pub input: Vec<TranscriptInput>,
    /// Host instructions given for this turn only.
    pub instructions: Option<String>,
    /// What the model produced, in order: reasoning, assistant text, tool
    /// calls with their results, and, for a completed turn, the final
    /// output last.
    pub items: Vec<TranscriptItem>,
    /// When the turn was admitted, to the second for SQLite sessions; on the
    /// wire, milliseconds since the Unix epoch.
    #[schemars(with = "u64")]
    #[serde(
        deserialize_with = "RunEvent::deserialize_at",
        serialize_with = "RunEvent::serialize_at"
    )]
    pub started_at: SystemTime,
    /// How the turn ended.
    pub status: TurnStatus,
    /// Durable position of the turn within its session.
    pub turn: TurnIndex,
    /// Tokens of the turn's model requests, when recorded: only turns run
    /// with a host request ID keep their usage.
    pub usage: Option<Usage>,
}

impl TurnRecord {
    /// Builds the transcript record of the settled `turn`.
    pub(crate) fn from_stored(turn: StoredTurn) -> Self {
        let text_output = turn.options.as_deref().is_some_and(|options| {
            serde_json::from_str::<Value>(options)
                .is_ok_and(|options| options["output_schema"].is_null())
        });
        let mut record = Self {
            error_type: turn.error_type,
            finished_at: turn.finished_at,
            input: Vec::new(),
            instructions: None,
            items: Vec::new(),
            started_at: turn.started_at,
            status: turn.status,
            turn: TurnIndex::from_position(turn.position),
            usage: turn
                .outcome
                .as_ref()
                .map(|outcome| Usage::from_report(outcome.report())),
        };
        for message in turn.messages {
            record.push_message(message, text_output);
        }

        record
    }

    /// Adds one persisted message of the turn.
    fn push_message(&mut self, message: ModelMessage, text_output: bool) {
        match message {
            ModelMessage::User(text) => self.input.push(TranscriptInput::Text { text }),
            ModelMessage::UserInput(input) => self
                .input
                .extend(input.blocks().iter().map(TranscriptInput::from)),
            ModelMessage::Instructions(text) => self.instructions = Some(text),
            ModelMessage::AssistantToolCall(call) => self.push_calls(&[call]),
            ModelMessage::AssistantToolCalls(calls) => self.push_calls(&calls),
            ModelMessage::ToolResult {
                call_id, content, ..
            } => self.answer_call(&call_id, content),
            ModelMessage::Assistant(content) => self.push_output(content, text_output),
            ModelMessage::AssistantReasoning {
                content,
                reasoning_content,
            } => {
                self.items.push(TranscriptItem::Reasoning {
                    text: reasoning_content,
                });
                self.push_output(content, text_output);
            }
            ModelMessage::System(_) => {}
        }
    }

    /// Adds a tool-call response: the reasoning and assistant text that
    /// travel with its first call, then every call, still unanswered.
    fn push_calls(&mut self, calls: &[ToolCall]) {
        let first = calls.first();
        if let Some(text) = first.and_then(ToolCall::reasoning_content) {
            self.items.push(TranscriptItem::Reasoning {
                text: text.to_string(),
            });
        }
        if let Some(text) = first.and_then(ToolCall::assistant_text) {
            self.items.push(TranscriptItem::Message {
                text: text.to_string(),
            });
        }
        self.items.extend(calls.iter().map(|call| {
            TranscriptItem::ToolCall(TranscriptToolCall {
                arguments: call.arguments_json().unwrap_or_default(),
                call_id: call.id().to_string(),
                name: call.name().to_string(),
                result: None,
            })
        }));
    }

    /// Records `content` as the result of the unanswered call `call_id`.
    fn answer_call(&mut self, call_id: &str, content: String) {
        let call = self.items.iter_mut().rev().find_map(|item| match item {
            TranscriptItem::ToolCall(call) if call.call_id == call_id && call.result.is_none() => {
                Some(call)
            }
            _ => None,
        });
        if let Some(call) = call {
            call.result = Some(content);
        }
    }

    /// Adds the final output: text in text output mode, otherwise its JSON,
    /// kept as text if it does not parse.
    fn push_output(&mut self, content: String, text_output: bool) {
        let output = if text_output {
            RunOutput::Text(content)
        } else {
            serde_json::from_str(&content).map_or(RunOutput::Text(content.clone()), RunOutput::Json)
        };
        self.items.push(TranscriptItem::Output { output });
    }
}

/// One input block of a transcript turn.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TranscriptInput {
    /// An image.
    Image {
        /// Standard base64 encoding of the image bytes.
        data: String,
        /// IANA media type, such as `image/png`.
        media_type: String,
    },
    /// A text segment.
    Text {
        /// The text.
        text: String,
    },
}

impl From<&InputBlock> for TranscriptInput {
    fn from(block: &InputBlock) -> Self {
        match block {
            InputBlock::Image(image) => Self::Image {
                data: BASE64_STANDARD.encode(image.bytes()),
                media_type: image.media_type().as_str().to_string(),
            },
            InputBlock::Text(text) => Self::Text { text: text.clone() },
        }
    }
}

/// One item a transcript turn produced.
///
/// It mirrors [`crate::event::Item`], except that a tool call carries only
/// what the session stores: its result text, without the status and summary
/// a live run reports.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum TranscriptItem {
    /// Assistant text the model wrote beside its tool calls.
    Message {
        /// The assistant text.
        text: String,
    },
    /// The validated final output of a completed turn.
    Output {
        /// The output, as [`crate::RunOutcome::result`] returned it.
        output: RunOutput,
    },
    /// Provider reasoning returned with a model response.
    Reasoning {
        /// The reasoning text.
        text: String,
    },
    /// A tool call and, when one was recorded, its result.
    ToolCall(TranscriptToolCall),
}

/// A tool call of a transcript turn.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TranscriptToolCall {
    /// Arguments as the model sent them, as JSON text.
    pub arguments: String,
    /// Provider identifier of the call.
    pub call_id: String,
    /// Function name the model called.
    pub name: String,
    /// Result text the model received, or `None` when the turn stopped
    /// before the call was answered.
    pub result: Option<String>,
}

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;
