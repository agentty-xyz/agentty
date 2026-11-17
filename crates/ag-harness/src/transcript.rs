//! Read-only transcripts of a session's settled turns, returned by
//! [`crate::Session::transcript`] for the turns a [`TurnRange`] selects.
//!
//! Like [`crate::event`] items, a transcript carries prompts, model output,
//! and tool results: treat it as session content. [`TurnRecord`] and
//! [`TurnRange`] derive `Serialize`, `Deserialize`, and `JsonSchema`, so the
//! same shapes can be sent over a wire protocol. A transcript reuses the
//! [`Item`] type of live run events for what each turn produced.

use std::ops::Range;
use std::time::SystemTime;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::{ErrorKind, ErrorKindName, ErrorKindWireError};
use crate::event::{Item, RunEvent, ToolCallItem, ToolCallStatus, TurnIndex};
use crate::input::InputBlock;
use crate::lifecycle::TurnErrorType;
use crate::limit::LimitKind;
use crate::model::{ModelErrorType, ModelMessage};
use crate::run::{RunOutput, Usage};
use crate::store::StoredTurn;
use crate::tool::ToolCall;

/// Which settled turns a transcript returns, oldest first.
///
/// Only settled turns count: a turn still running is never returned and
/// never counts toward [`Self::Last`]. Selection first keeps the turns from
/// [`Self::start`] up to, not including, [`Self::end`], then keeps at most
/// the newest [`Self::newest`] of them. On the wire it is an object tagged
/// by `type`: `{"type": "all"}`, `{"type": "last", "count": 2}`, or
/// `{"type": "span", "start": 1, "end": 3}`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(from = "TurnRangeFields", into = "TurnRangeFields")]
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

impl From<TurnRangeFields> for TurnRange {
    fn from(fields: TurnRangeFields) -> Self {
        match fields {
            TurnRangeFields::All => Self::All,
            TurnRangeFields::Last { count } => Self::Last(count),
            TurnRangeFields::Span { end, start } => Self::Span(start..end),
        }
    }
}

/// Wire form of a [`TurnRange`].
#[derive(Deserialize, JsonSchema, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[schemars(rename = "TurnRange")]
enum TurnRangeFields {
    /// Every settled turn.
    All,
    /// The newest settled turns, at most `count`.
    Last {
        /// How many of the newest settled turns to keep.
        count: u64,
    },
    /// The settled turns from `start` up to, not including, `end`.
    Span {
        /// Exclusive index bound.
        end: TurnIndex,
        /// First index selected.
        start: TurnIndex,
    },
}

impl From<TurnRange> for TurnRangeFields {
    fn from(range: TurnRange) -> Self {
        match range {
            TurnRange::All => Self::All,
            TurnRange::Last(count) => Self::Last { count },
            TurnRange::Span(span) => Self::Span {
                end: span.end,
                start: span.start,
            },
        }
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

/// One settled turn of a session transcript.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct TurnRecord {
    /// Why a failed or cancelled turn stopped; `None` for a completed turn
    /// and for a turn interrupted because its owner stopped renewing it.
    pub error: Option<TurnFailure>,
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
    /// output last. A tool call reports
    /// [`ToolCallStatus::Answered`] with its result, or
    /// [`ToolCallStatus::Unanswered`] with empty output when
    /// the turn stopped before answering it, and an empty summary.
    pub items: Vec<Item>,
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
        let mut record = Self {
            error: turn.error_type.as_deref().and_then(Self::stored_error),
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
            record.push_message(message, turn.text_output);
        }

        record
    }

    /// Returns the kind of the classification a store recorded, or `None`
    /// for one naming no failure, such as an expired lease's `interrupted`.
    ///
    /// Failed turns record the `Debug` form of their [`TurnErrorType`] and
    /// turns interrupted mid-tool its telemetry name, so both are accepted.
    fn stored_error(stored: &str) -> Option<TurnFailure> {
        TurnErrorKind::ALL
            .into_iter()
            .find(|error| format!("{:?}", error.0) == stored || error.0.as_str() == stored)
            .map(|error| TurnFailure { kind: error.kind() })
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
                self.items.push(Item::Reasoning {
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
            self.items.push(Item::Reasoning {
                text: text.to_string(),
            });
        }
        if let Some(text) = first.and_then(ToolCall::assistant_text) {
            self.items.push(Item::Message {
                text: text.to_string(),
            });
        }
        self.items.extend(
            calls
                .iter()
                .map(|call| Item::ToolCall(ToolCallItem::unanswered(call))),
        );
    }

    /// Records `content` as the result of the unanswered call `call_id`.
    fn answer_call(&mut self, call_id: &str, content: String) {
        let call = self.items.iter_mut().rev().find_map(|item| match item {
            Item::ToolCall(call)
                if call.call_id == call_id && call.status == ToolCallStatus::Unanswered =>
            {
                Some(call)
            }
            _ => None,
        });
        if let Some(call) = call {
            call.output = content;
            call.status = ToolCallStatus::Answered;
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
        self.items.push(Item::Output { output });
    }
}

/// Why a settled turn stopped, as [`crate::event::RunFailure`] reports a
/// live run's failure without its message.
///
/// On the wire it is an object with `kind` and, for `budget_exhausted`, the
/// `limit` it reached.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(into = "TurnFailureFields", try_from = "TurnFailureFields")]
#[non_exhaustive]
pub struct TurnFailure {
    /// Failure classification; [`ErrorKind::Cancelled`] for a cancelled
    /// turn. Sessions store a coarser classification than a live run
    /// reports, so a provider rejection reads as [`ErrorKind::Other`]
    /// rather than as its finer kind, and every failing tool as
    /// [`ErrorKind::ToolFailed`].
    pub kind: ErrorKind,
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

/// A stored failure classification and the [`ErrorKind`] it reads as.
#[derive(Clone, Copy)]
struct TurnErrorKind(TurnErrorType);

impl TurnErrorKind {
    /// Every classification a store may record for a stopped turn.
    const ALL: [Self; 22] = [
        Self(TurnErrorType::Cancelled),
        Self(TurnErrorType::Tool),
        Self(TurnErrorType::ContextBudget),
        Self(TurnErrorType::RepeatedToolCalls),
        Self(TurnErrorType::RepositoryRequired),
        Self(TurnErrorType::ComparisonRepositoryMismatch),
        Self(TurnErrorType::Session),
        Self(TurnErrorType::RunLimit(LimitKind::ModelRequests)),
        Self(TurnErrorType::RunLimit(LimitKind::ToolCalls)),
        Self(TurnErrorType::RunLimit(LimitKind::Tokens)),
        Self(TurnErrorType::RunLimit(LimitKind::WallTime)),
        Self(TurnErrorType::Model(ModelErrorType::Request)),
        Self(TurnErrorType::Model(ModelErrorType::Transport)),
        Self(TurnErrorType::Model(ModelErrorType::Provider)),
        Self(TurnErrorType::Model(ModelErrorType::ContextOverflow)),
        Self(TurnErrorType::Model(
            ModelErrorType::InvalidProviderResponse,
        )),
        Self(TurnErrorType::Model(ModelErrorType::InvalidResponse)),
        Self(TurnErrorType::Model(ModelErrorType::UnsupportedOutput)),
        Self(TurnErrorType::Model(ModelErrorType::UnsupportedInput)),
        Self(TurnErrorType::Model(ModelErrorType::ResponseTooLarge)),
        Self(TurnErrorType::Model(ModelErrorType::InvalidOutput)),
        Self(TurnErrorType::Model(ModelErrorType::InvalidToolCall)),
    ];

    /// Returns the kind a live run reports for this classification, as far
    /// as the classification tells it.
    fn kind(self) -> ErrorKind {
        match self.0 {
            TurnErrorType::Cancelled => ErrorKind::Cancelled,
            TurnErrorType::Tool => ErrorKind::ToolFailed,
            TurnErrorType::ContextBudget
            | TurnErrorType::Model(ModelErrorType::ContextOverflow) => ErrorKind::ContextOverflow,
            TurnErrorType::RepeatedToolCalls => ErrorKind::Repetition,
            TurnErrorType::RepositoryRequired
            | TurnErrorType::ComparisonRepositoryMismatch
            | TurnErrorType::Model(
                ModelErrorType::UnsupportedOutput | ModelErrorType::UnsupportedInput,
            ) => ErrorKind::Config,
            TurnErrorType::Session => ErrorKind::Store,
            TurnErrorType::RunLimit(limit) => ErrorKind::BudgetExhausted(limit),
            TurnErrorType::Model(ModelErrorType::Transport) => ErrorKind::Transport,
            TurnErrorType::Model(ModelErrorType::Request | ModelErrorType::Provider) => {
                ErrorKind::Other
            }
            TurnErrorType::Model(
                ModelErrorType::InvalidProviderResponse
                | ModelErrorType::InvalidResponse
                | ModelErrorType::ResponseTooLarge
                | ModelErrorType::InvalidOutput
                | ModelErrorType::InvalidToolCall,
            ) => ErrorKind::InvalidOutput,
        }
    }
}

impl TryFrom<TurnFailureFields> for TurnFailure {
    type Error = ErrorKindWireError;

    fn try_from(fields: TurnFailureFields) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: fields.kind.with_limit(fields.limit)?,
        })
    }
}

/// Wire form of a [`TurnFailure`].
#[derive(Deserialize, JsonSchema, Serialize)]
#[schemars(rename = "TurnFailure")]
struct TurnFailureFields {
    /// Failure classification; `cancelled` for a cancelled turn.
    kind: ErrorKindName,
    /// The run limit a `budget_exhausted` turn reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<LimitKind>,
}

impl From<TurnFailure> for TurnFailureFields {
    fn from(failure: TurnFailure) -> Self {
        Self {
            kind: failure.kind.into(),
            limit: match failure.kind {
                ErrorKind::BudgetExhausted(limit) => Some(limit),
                _ => None,
            },
        }
    }
}

#[cfg(test)]
#[path = "transcript_test.rs"]
mod tests;
