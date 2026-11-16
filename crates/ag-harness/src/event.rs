//! Typed events of one [`crate::Run`], streamed to the embedder that split
//! the run with [`crate::Run::into_parts`].
//!
//! Unlike [`crate::lifecycle`] events, which stay content-free for
//! telemetry, run events carry the model output, reasoning, tool arguments,
//! and tool results an embedder needs to render the run. Treat them as
//! session content: they may contain secrets from prompts, files, and
//! commands, and must never be exported as telemetry.
//!
//! Every type derives `Serialize`, `Deserialize`, and `JsonSchema`, so the
//! same shape can be sent over a wire protocol.

use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::SystemTime;

use futures_core::Stream;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, mpsc};

use crate::catalog::ModelId;
use crate::error::{ErrorKind, RunError};
use crate::run::{RunOutcome, RunOutput, Usage};
use crate::tool::ToolCall;
use crate::turn::{ContextActivity, ToolActivity};

/// Events a run may emit before a consumer reads them; a run whose stream
/// is full waits at its next event until the consumer catches up.
const EVENT_BUFFER: usize = 64;

/// Identifier of one run, unique across processes.
#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct RunId(String);

impl RunId {
    /// Returns the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifier of one item within its run, assigned in the order items
/// start.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
#[non_exhaustive]
pub struct ItemId(u64);

impl ItemId {
    /// Returns the zero-based position of the item among its run's items.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// One event of a run, in the order the run produced it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunEvent {
    /// Wall-clock time the event was produced.
    pub at: SystemTime,
    /// What happened.
    pub kind: RunEventKind,
    /// Run that produced the event.
    pub run: RunId,
    /// Zero-based position of the event in its run's stream; consecutive
    /// events differ by one.
    pub seq: u64,
}

/// What a [`RunEvent`] reports.
///
/// [`Self::Completed`] is always the last event, including for a run that
/// failed or was cancelled, unless the run itself was dropped before it
/// finished; then the stream ends without it. New kinds, such as
/// streamed deltas, retries, and approval requests, may be added.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunEventKind {
    /// The run's turn was admitted to its session and is about to send its
    /// first model request. A run that fails before admission, such as one
    /// whose session is busy, emits only [`Self::Completed`].
    Started {
        /// Model as `provider/model` from its metadata, or `None` when the
        /// model reports no metadata.
        model: Option<ModelId>,
        /// Session the turn belongs to; a one-shot run reports its
        /// in-memory session.
        session: String,
        /// Durable position of the run's turn within its session.
        turn: u64,
    },
    /// An item started. Its content arrives with [`Self::ItemCompleted`]
    /// for the same `id`.
    ItemStarted {
        /// Identifier of the item.
        id: ItemId,
        /// What the item is.
        item_kind: ItemKind,
    },
    /// An item finished with its content.
    ItemCompleted {
        /// Identifier of the item, as [`Self::ItemStarted`] reported it.
        id: ItemId,
        /// The finished item.
        item: Item,
    },
    /// A model request completed, with the tokens its provider reported.
    /// The usage of these events sums to the run outcome's
    /// [`crate::RunOutcome::usage`].
    ModelRequestCompleted {
        /// Tokens of this request.
        usage: Usage,
    },
    /// The next model request is sent reduced to fit the context budget.
    /// Counts describe that whole request, so a reduction carried over from
    /// an earlier request is reported again.
    ContextReduced(ContextActivity),
    /// The run finished; always the last event.
    Completed(RunSummary),
}

/// What an item is, known when it starts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemKind {
    /// An assistant message.
    Message,
    /// Provider reasoning the model returned.
    Reasoning,
    /// A tool call and its result.
    ToolCall,
}

/// Content of a finished item.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Item {
    /// An assistant message: the run's validated final output, or text the
    /// model wrote beside its tool calls as [`RunOutput::Text`].
    Message {
        /// The message content.
        content: RunOutput,
    },
    /// Provider reasoning returned with a model response.
    Reasoning {
        /// The reasoning text.
        text: String,
    },
    /// A tool call and its result.
    ToolCall(ToolCallItem),
}

/// A tool call the model made and the result it was answered with.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct ToolCallItem {
    /// Arguments as the model sent them, as JSON text; arguments the tool
    /// could not decode are kept verbatim and may not be valid JSON.
    pub arguments: String,
    /// Provider identifier of the call.
    pub call_id: String,
    /// Function name the model called.
    pub name: String,
    /// Result text the model received.
    pub output: String,
    /// How the call was answered.
    pub status: ToolCallStatus,
    /// Bounded one-line description of what the call did.
    pub summary: String,
}

impl ToolCallItem {
    /// Describes `call` answered with `output`, as `activity` reports it.
    pub(crate) fn new(call: &ToolCall, output: &str, activity: &ToolActivity) -> Self {
        Self {
            arguments: call.arguments_json().unwrap_or_default(),
            call_id: call.id().to_string(),
            name: call.name().to_string(),
            output: output.to_string(),
            status: ToolCallStatus::from(activity),
            summary: activity.to_string(),
        }
    }
}

/// How a tool call was answered.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolCallStatus {
    /// The tool ran; a `bash` result reports the command's own exit status.
    Completed,
    /// The tool rejected the call, such as malformed arguments or a path
    /// outside the repository, and the model was told why.
    Rejected,
    /// The call was answered without running: a repeat of an earlier read,
    /// or a read deferred to a later response.
    Skipped,
    /// The tool is denied by policy or does not exist.
    Unavailable,
}

impl From<&ToolActivity> for ToolCallStatus {
    fn from(activity: &ToolActivity) -> Self {
        match activity {
            ToolActivity::Bash { .. }
            | ToolActivity::Read { .. }
            | ToolActivity::ReadInspection { .. }
            | ToolActivity::Write { .. } => Self::Completed,
            ToolActivity::MalformedArguments { .. }
            | ToolActivity::ReadInspectionRejected { .. }
            | ToolActivity::ReadRejected { .. }
            | ToolActivity::WriteRejected { .. } => Self::Rejected,
            ToolActivity::ReadDeferred { .. } | ToolActivity::ReadRepeated { .. } => Self::Skipped,
            ToolActivity::Unavailable { .. } => Self::Unavailable,
        }
    }
}

/// How a run finished, carried by [`RunEventKind::Completed`].
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunSummary {
    /// Why the run failed, or `None` when it produced its output.
    pub error: Option<RunFailure>,
    /// Tokens of every model request, as in [`crate::RunOutcome::usage`].
    pub usage: Usage,
}

impl RunSummary {
    /// Summarizes the settled `outcome`.
    pub(crate) fn new(outcome: &RunOutcome) -> Self {
        Self {
            error: outcome.result.as_ref().err().map(RunFailure::from),
            usage: outcome.usage,
        }
    }
}

/// A run's failure as plain data.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunFailure {
    /// Failure classification; [`ErrorKind::Cancelled`] for a cancelled run.
    pub kind: ErrorKind,
    /// Bounded failure description.
    pub message: String,
    /// Whether retrying the run may succeed.
    pub retryable: bool,
}

impl From<&RunError> for RunFailure {
    fn from(error: &RunError) -> Self {
        Self {
            kind: error.kind(),
            message: error.to_string(),
            retryable: error.is_retryable(),
        }
    }
}

/// Ordered events of one run, from [`crate::Run::into_parts`].
///
/// Delivery is lossless: the run waits at its next event while 64 events
/// are unread, so read the stream concurrently with
/// awaiting the run's outcome. Dropping the stream never stalls or cancels
/// the run; later events are discarded. The stream ends after
/// [`RunEventKind::Completed`].
#[must_use = "a run waits for its events to be read; drop the stream to discard them"]
#[non_exhaustive]
pub struct EventStream {
    receiver: mpsc::Receiver<RunEvent>,
}

impl EventStream {
    /// Waits for the next event, or `None` once the stream ended.
    pub async fn next(&mut self) -> Option<RunEvent> {
        self.receiver.recv().await
    }
}

impl Stream for EventStream {
    type Item = RunEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<RunEvent>> {
        self.receiver.poll_recv(context)
    }
}

/// Producing side of one run's [`EventStream`].
pub(crate) struct EventSink {
    run: RunId,
    state: Mutex<SinkState>,
}

impl EventSink {
    /// Creates a sink for a new run and the stream that reads it.
    pub(crate) fn channel() -> (Arc<Self>, EventStream) {
        let (sender, receiver) = mpsc::channel(EVENT_BUFFER);
        let sink = Self {
            run: RunId(uuid::Uuid::new_v4().to_string()),
            state: Mutex::new(SinkState {
                next_item: 0,
                next_seq: 0,
                sender: Some(sender),
            }),
        };

        (Arc::new(sink), EventStream { receiver })
    }

    /// Sends `kind`, waiting while the stream is full; does nothing once the
    /// run completed or the stream was dropped.
    pub(crate) async fn emit(&self, kind: RunEventKind) {
        let mut state = self.state.lock().await;
        self.send(&mut state, kind).await;
    }

    /// Starts an item of `item_kind` and returns its identifier.
    pub(crate) async fn start_item(&self, item_kind: ItemKind) -> ItemId {
        let mut state = self.state.lock().await;
        let id = ItemId(state.next_item);
        state.next_item += 1;
        self.send(&mut state, RunEventKind::ItemStarted { id, item_kind })
            .await;

        id
    }

    /// Sends the run's last event and ends the stream.
    pub(crate) async fn complete(&self, summary: RunSummary) {
        let mut state = self.state.lock().await;
        self.send(&mut state, RunEventKind::Completed(summary))
            .await;
        state.sender = None;
    }

    /// Sends one event under the held `state`, so sequence numbers follow
    /// delivery order.
    async fn send(&self, state: &mut SinkState, kind: RunEventKind) {
        let Some(sender) = state.sender.clone() else {
            return;
        };
        let event = RunEvent {
            at: SystemTime::now(),
            kind,
            run: self.run.clone(),
            seq: state.next_seq,
        };
        state.next_seq += 1;
        // A dropped stream discards the event; the run continues.
        let _ = sender.send(event).await;
    }
}

/// Sequence counters and the sender of one run's events.
struct SinkState {
    next_item: u64,
    next_seq: u64,
    /// `None` once the run completed.
    sender: Option<mpsc::Sender<RunEvent>>,
}

#[cfg(test)]
#[path = "event_test.rs"]
mod tests;
