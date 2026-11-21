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

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use futures_core::Stream;
use schemars::JsonSchema;
use serde::de::Error as _;
use serde::ser::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use tokio::sync::{Mutex, mpsc};

use crate::approval::ApprovalRequest;
use crate::catalog::ModelId;
use crate::error::{ErrorKind, ErrorKindName, ErrorKindWireError, RunError};
use crate::limit::LimitKind;
use crate::run::{RunOutcome, RunOutput, Usage};
use crate::sandbox::SandboxWarning;
use crate::tool::ToolCall;
use crate::turn::{ContextActivity, HostToolOutcome, ToolActivity};
use crate::usage::Cost;

/// Events a run may emit before a consumer reads them; a run whose stream
/// is full waits at its next event until the consumer catches up.
const EVENT_BUFFER: usize = 64;

/// Slots held back from [`EVENT_BUFFER`] traffic for the events that end a
/// run: the started and completed [`Item::Output`], and
/// [`RunEventKind::Completed`]. A run therefore always finishes, even when
/// its stream is full and unread.
const FINAL_EVENTS: usize = 3;

/// Identifier of one run, unique across processes.
///
/// Every [`crate::Run`] has one: each event of its stream carries it, and
/// each of its tool calls sees it in [`crate::tool::ToolContext::run_id`].
#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct RunId(String);

impl RunId {
    /// Returns a new random identifier.
    pub(crate) fn generate() -> Self {
        Self(uuid::Uuid::new_v4().to_string())
    }

    /// Returns the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identifier of one item within its run, assigned in the order items
/// start.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct ItemId(u64);

impl ItemId {
    /// Returns the zero-based position of the item among its run's items.
    pub fn get(self) -> u64 {
        self.0
    }
}

/// Identifier of the session a run's turn belongs to.
#[derive(Clone, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct SessionId(String);

impl SessionId {
    /// Returns the identifier text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Wraps the session identifier `id`.
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }
}

/// Durable position of a turn within its session.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(transparent)]
pub struct TurnIndex(u64);

impl TurnIndex {
    /// Wraps the zero-based durable turn position `position`.
    pub fn new(position: u64) -> Self {
        Self(position)
    }

    /// Returns the position.
    pub fn get(self) -> u64 {
        self.0
    }

    /// Wraps a durable turn position, which the store keeps non-negative.
    pub(crate) fn from_position(position: i64) -> Self {
        debug_assert!(position >= 0, "durable turn positions are non-negative");

        Self(position.unsigned_abs())
    }
}

/// One event of a run, in the order the run produced it.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunEvent {
    /// Wall-clock time the event was produced, to the millisecond; on the
    /// wire, milliseconds since the Unix epoch.
    #[schemars(with = "u64")]
    #[serde(
        deserialize_with = "RunEvent::deserialize_at",
        serialize_with = "RunEvent::serialize_at"
    )]
    pub at: SystemTime,
    /// What happened.
    pub kind: RunEventKind,
    /// Run that produced the event.
    pub run: RunId,
    /// Zero-based position of the event in its run's stream; consecutive
    /// events differ by one.
    pub seq: u64,
}

impl RunEvent {
    /// Encodes `at` as milliseconds since the Unix epoch; transcript turn
    /// times share this encoding.
    pub(crate) fn serialize_at<S: Serializer>(
        at: &SystemTime,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        let millis = at
            .duration_since(UNIX_EPOCH)
            .ok()
            .and_then(|elapsed| u64::try_from(elapsed.as_millis()).ok())
            .ok_or(S::Error::custom("event time precedes the Unix epoch"))?;

        serializer.serialize_u64(millis)
    }

    /// Decodes `at` from milliseconds since the Unix epoch.
    pub(crate) fn deserialize_at<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<SystemTime, D::Error> {
        let millis = u64::deserialize(deserializer)?;

        UNIX_EPOCH
            .checked_add(Duration::from_millis(millis))
            .ok_or(D::Error::custom("event time exceeds the platform clock"))
    }
}

/// What a [`RunEvent`] reports.
///
/// [`Self::Completed`] is always the last event, including for a run that
/// failed or was cancelled, unless the run itself was dropped before it
/// finished; then the stream ends without it. New kinds may be added.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum RunEventKind {
    /// The run's turn was admitted to its session and is about to send its
    /// first model request, or the compaction requests that precede it (see
    /// [`Self::Compacted`]). A run that fails before admission, such as
    /// one whose session is busy, emits only [`Self::Completed`].
    Started {
        /// Catalog identifier of the model, when the harness selected it
        /// from a [`crate::ModelCatalog`]; `None` for a model the host
        /// supplied directly.
        model: Option<ModelId>,
        /// Session the turn belongs to; a one-shot run reports its
        /// in-memory session.
        session: SessionId,
        /// Durable position of the run's turn within its session.
        turn: TurnIndex,
    },
    /// An item started. Its content arrives with [`Self::ItemCompleted`]
    /// for the same `id`, unless the run fails or is cancelled first: an
    /// item still open when [`Self::Completed`] arrives was interrupted
    /// and never completes. So was a tool-call item a streamed model
    /// response opened when that response was discarded, such as a response
    /// that failed or an invalid final answer that was requested again; the
    /// discarded response's reasoning and message items complete with the
    /// text it streamed.
    ItemStarted {
        /// Identifier of the item.
        id: ItemId,
        /// What the item is.
        item_kind: ItemKind,
    },
    /// A fragment of an open item's content as the model streams it,
    /// between the item's [`Self::ItemStarted`] and [`Self::ItemCompleted`].
    ///
    /// Only the built-in [`crate::model::ModelClient`] streams, for
    /// providers on the Responses and Chat Completions protocols, and only
    /// for the run's turn requests. A response's reasoning streams into a
    /// [`ItemKind::Reasoning`] item, its text into a [`ItemKind::Message`]
    /// item, and each tool call's arguments into that call's
    /// [`ItemKind::ToolCall`] item, each started at its first fragment.
    /// Fragments of one item concatenate to its streamed content; the
    /// completed item stays authoritative.
    ItemDelta {
        /// The fragment.
        delta: Delta,
        /// Identifier of the item, as [`Self::ItemStarted`] reported it.
        id: ItemId,
    },
    /// An item finished with its content.
    ItemCompleted {
        /// Identifier of the item, as [`Self::ItemStarted`] reported it.
        id: ItemId,
        /// The finished item.
        item: Item,
    },
    /// A tool call that changes state waits for the harness's
    /// [`crate::approval::ApprovalHandler`], which the harness asks right
    /// after this event. It falls between the call's [`Self::ItemStarted`]
    /// and [`Self::ItemCompleted`]; a call the handler did not decide on in
    /// time fails the run with [`ErrorKind::ApprovalTimeout`].
    ApprovalRequested {
        /// Identifier of the call's [`ItemKind::ToolCall`] item.
        id: ItemId,
        /// The call waiting for a decision.
        request: ApprovalRequest,
    },
    /// A model request completed, with the tokens its provider reported.
    /// The usage and cost of these events sum to the run outcome's
    /// [`crate::RunOutcome::usage`] and [`crate::RunOutcome::cost`].
    /// Later per-request fields may be added, so match it with `..`.
    #[non_exhaustive]
    ModelRequestCompleted {
        /// Price of `usage` at the prices of `model`, the model that
        /// answered this request, or `None` when that model has no
        /// [`crate::catalog::Pricing`].
        cost: Option<Cost>,
        /// Catalog identifier of the model that answered this request, such
        /// as the summary model answering a round summary request, when the
        /// harness selected it from a [`crate::ModelCatalog`]; `None` for a
        /// model the host supplied directly.
        model: Option<ModelId>,
        /// Tokens of this request.
        usage: Usage,
    },
    /// The next model request is sent reduced to fit the context budget.
    /// Counts describe that whole request, so a reduction carried over from
    /// an earlier request is reported again.
    ContextReduced(ContextActivity),
    /// Under [`crate::model::ContextPolicy::AutoCompact`], the session tried
    /// to compact earlier turns before the run's first turn request. It
    /// follows [`Self::Started`] and the [`Self::ModelRequestCompleted`]
    /// events of the compaction requests, each emitted as it completed. The
    /// turn's [`crate::turn::HistoryActivity`] counts what its first request
    /// still evicted.
    Compacted(CompactionReport),
    /// A model request of the run, a turn request or a compaction or round
    /// summary request, failed transiently and is retried after `after`, as
    /// its provider's [`crate::provider::RetryPolicy`] allows. It is
    /// emitted before the wait, so cancelling the run then stops it. A turn
    /// request that still fails after its last retry fails the run; a
    /// summary request's failure is handled as that summary's failure.
    ///
    /// Only the built-in [`crate::model::ModelClient`] reports retries, and
    /// only from the task that sends the request; a custom
    /// [`crate::Model`] that retries on its own emits none. Later fields
    /// may be added, so match it with `..`.
    #[non_exhaustive]
    Retrying {
        /// Delay before the retry is sent, to the millisecond; on the wire,
        /// whole milliseconds.
        #[schemars(with = "u64")]
        #[serde(
            deserialize_with = "RunEventKind::deserialize_after",
            serialize_with = "RunEventKind::serialize_after"
        )]
        after: Duration,
        /// One-based number of this retry within its model request.
        #[schemars(range(min = 1))]
        attempt: u32,
        /// The failure being retried: [`ErrorKind::RateLimited`] for a
        /// `429`, [`ErrorKind::Overloaded`] for a `5xx`, or
        /// [`ErrorKind::Transport`] for a connection failure.
        kind: ErrorKind,
    },
    /// The run offers native-sandboxed `bash`, the sandbox cannot run on
    /// this host, and the harness runs the run's commands without isolation
    /// instead, as [`crate::sandbox::IfUnavailable::Unsandboxed`], the
    /// default, allows. It is emitted once per such run, before its first
    /// turn request, and names what is missing and how to fix it, as the
    /// turn's [`crate::TurnReport::sandbox_warning`] does; the model-facing
    /// `bash` description states that commands run without isolation.
    SandboxUnavailable(SandboxWarning),
    /// The run finished; always the last event. It follows the run's
    /// settlement, so once it arrives the session transcript holds the
    /// turn of a run that emitted [`Self::Started`] with its terminal
    /// status, unless persistence cleanup failed (see
    /// [`crate::RunOutcome::settlement`]). A run that stopped before
    /// [`Self::Started`] may have recorded no turn.
    Completed(RunSummary),
}

impl RunEventKind {
    /// Returns a [`Self::Retrying`] event whose delay is truncated to the
    /// millisecond, so the event reads the same in process and on the wire.
    #[cfg(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    ))]
    pub(crate) fn retrying(after: Duration, attempt: u32, kind: ErrorKind) -> Self {
        Self::Retrying {
            after: Duration::from_millis(Self::whole_millis(after)),
            attempt,
            kind,
        }
    }

    /// Encodes a [`Self::Retrying`] delay as whole milliseconds, saturating
    /// at `u64::MAX`.
    fn serialize_after<S: Serializer>(after: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u64(Self::whole_millis(*after))
    }

    /// Returns `duration` in whole milliseconds, saturating at `u64::MAX`.
    fn whole_millis(duration: Duration) -> u64 {
        u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
    }

    /// Decodes a [`Self::Retrying`] delay from milliseconds.
    fn deserialize_after<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Duration, D::Error> {
        Ok(Duration::from_millis(u64::deserialize(deserializer)?))
    }
}

/// What a compaction attempt did.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct CompactionReport {
    /// Whether a checkpoint was published.
    pub outcome: CompactionOutcome,
}

/// How an automatic compaction attempt ended.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum CompactionOutcome {
    /// A new checkpoint was published.
    Compacted {
        /// Newest turn the published checkpoint covers.
        covered_through: TurnIndex,
        /// Catalog identifier of the model that answered the compaction
        /// requests, as their [`RunEventKind::ModelRequestCompleted`]
        /// events name it; `None` for a model the host supplied directly.
        /// The checkpoint itself records the provider's model metadata.
        model: Option<ModelId>,
    },
    /// The store held no uncovered turn to compact, for example because
    /// another handle of the session published a checkpoint covering them
    /// first. No model request was sent and the turn replays the store's
    /// checkpoint.
    NothingToCover,
    /// Compaction failed, so the previous checkpoint stays and the turn
    /// fell back to eviction.
    Failed(RunFailure),
}

/// A fragment of streamed item content, carried by
/// [`RunEventKind::ItemDelta`].
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Delta {
    /// Assistant text of a [`ItemKind::Message`] item.
    Text {
        /// The text fragment.
        text: String,
    },
    /// Provider reasoning of a [`ItemKind::Reasoning`] item, such as a
    /// reasoning summary.
    Reasoning {
        /// The reasoning fragment.
        text: String,
    },
    /// Arguments of a [`ItemKind::ToolCall`] item's call, as JSON text that
    /// is incomplete until the last fragment.
    ToolArguments {
        /// The arguments fragment.
        arguments: String,
    },
}

/// What an item is, known when it starts.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ItemKind {
    /// Assistant text written beside tool calls, or streamed by a response.
    Message,
    /// The run's final output.
    Output,
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
    /// Assistant text the model wrote beside its tool calls, or the text a
    /// streamed response wrote as it went (see [`RunEventKind::ItemDelta`]):
    /// for a final answer, its raw text, which [`Self::Output`] then carries
    /// validated. A final answer that did not stream emits only
    /// [`Self::Output`].
    Message {
        /// The assistant text.
        text: String,
    },
    /// The validated final output of a successful run, emitted after the
    /// turn committed and directly before [`RunEventKind::Completed`]; a
    /// failed or cancelled run has none.
    Output {
        /// The run's output, as in [`crate::RunOutcome::result`].
        output: RunOutput,
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
    /// Result text the model received; empty for a call that was never
    /// answered.
    pub output: String,
    /// How the call was answered.
    pub status: ToolCallStatus,
    /// Bounded one-line description of what the call did; empty in a
    /// [`crate::TurnRecord`], since sessions do not store it.
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

    /// Describes `call` as a session recorded it, not yet answered.
    pub(crate) fn unanswered(call: &ToolCall) -> Self {
        Self {
            arguments: call.arguments_json().unwrap_or_default(),
            call_id: call.id().to_string(),
            name: call.name().to_string(),
            output: String::new(),
            status: ToolCallStatus::Unanswered,
            summary: String::new(),
        }
    }
}

/// How a tool call was answered.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, JsonSchema, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum ToolCallStatus {
    /// The tool ran; a `bash` result reports the command's own exit status,
    /// and a host tool's oversized output is dropped.
    Completed,
    /// The tool rejected the call, such as malformed arguments or a path
    /// outside the repository, and the model was told why.
    Rejected,
    /// The call was answered without running: a repeat of an earlier read,
    /// or a read deferred to a later response.
    Skipped,
    /// The tool is denied by policy or does not exist.
    Unavailable,
    /// The [`crate::approval::ApprovalHandler`] denied the call, which did
    /// not run; the model received the denial's feedback.
    Denied,
    /// The call was answered, as a [`crate::TurnRecord`] reports every
    /// answered call: sessions store the result text but not which of the
    /// statuses above the live run reported.
    Answered,
    /// The turn stopped before the call was answered, as a
    /// [`crate::TurnRecord`] reports it; the output is empty.
    Unanswered,
    /// A `bash` command ran inside the sandbox and failed with output
    /// suggesting the sandbox blocked it, as
    /// [`ToolActivity::BashSandboxDenied`] reports; the result told the
    /// model. A host implementing approval-based escalation can ask the
    /// user whether to rerun the command outside the sandbox.
    SandboxDenied,
}

impl From<&ToolActivity> for ToolCallStatus {
    fn from(activity: &ToolActivity) -> Self {
        match activity {
            ToolActivity::Bash { .. }
            | ToolActivity::Read { .. }
            | ToolActivity::ReadInspection { .. }
            | ToolActivity::Write { .. } => Self::Completed,
            ToolActivity::Host {
                outcome: HostToolOutcome::Denied,
                ..
            } => Self::Denied,
            ToolActivity::Host { outcome, .. } if !outcome.is_rejected() => Self::Completed,
            ToolActivity::MalformedArguments { .. }
            | ToolActivity::Host { .. }
            | ToolActivity::ReadInspectionRejected { .. }
            | ToolActivity::ReadRejected { .. }
            | ToolActivity::WriteRejected { .. } => Self::Rejected,
            ToolActivity::ReadDeferred { .. } | ToolActivity::ReadRepeated { .. } => Self::Skipped,
            ToolActivity::BashSandboxDenied { .. } => Self::SandboxDenied,
            ToolActivity::Unavailable { .. } => Self::Unavailable,
        }
    }
}

/// How a run finished, carried by [`RunEventKind::Completed`].
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[non_exhaustive]
pub struct RunSummary {
    /// Price of [`Self::usage`], as in [`crate::RunOutcome::cost`].
    pub cost: Option<Cost>,
    /// Why the run failed, or `None` when it produced its output.
    pub error: Option<RunFailure>,
    /// Tokens of every model request, as in [`crate::RunOutcome::usage`].
    pub usage: Usage,
}

impl RunSummary {
    /// Summarizes the settled `outcome`.
    pub(crate) fn new(outcome: &RunOutcome) -> Self {
        Self {
            cost: outcome.cost,
            error: outcome.result.as_ref().err().map(RunFailure::from),
            usage: outcome.usage,
        }
    }
}

/// A run's failure as plain data.
///
/// On the wire its `kind` is the [`ErrorKind::as_str`] name, and a
/// [`ErrorKind::BudgetExhausted`] failure adds the reached `limit`.
#[derive(Clone, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
#[serde(into = "RunFailureFields", try_from = "RunFailureFields")]
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

impl TryFrom<RunFailureFields> for RunFailure {
    type Error = ErrorKindWireError;

    fn try_from(fields: RunFailureFields) -> Result<Self, Self::Error> {
        Ok(Self {
            kind: fields.kind.with_limit(fields.limit)?,
            message: fields.message,
            retryable: fields.retryable,
        })
    }
}

/// Wire form of a [`RunFailure`].
#[derive(Deserialize, JsonSchema, Serialize)]
#[schemars(rename = "RunFailure")]
struct RunFailureFields {
    /// Failure classification; `cancelled` for a cancelled run.
    kind: ErrorKindName,
    /// The run limit a `budget_exhausted` failure reached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    limit: Option<LimitKind>,
    /// Bounded failure description.
    message: String,
    /// Whether retrying the run may succeed.
    retryable: bool,
}

impl From<RunFailure> for RunFailureFields {
    fn from(failure: RunFailure) -> Self {
        Self {
            kind: failure.kind.into(),
            limit: match failure.kind {
                ErrorKind::BudgetExhausted(limit) => Some(limit),
                _ => None,
            },
            message: failure.message,
            retryable: failure.retryable,
        }
    }
}

/// Ordered events of one run, from [`crate::Run::into_parts`], read as a
/// [`Stream`], for example with `futures::StreamExt::next`.
///
/// Delivery is lossless: the run waits at its next event while the stream's
/// bounded buffer is full, so read the stream concurrently with awaiting
/// the run's outcome. The events that end the run never wait, so
/// cancelling a run whose stream is full and unread still resolves its
/// outcome. Dropping the stream never stalls or cancels the run; later
/// events are discarded. The stream ends after [`RunEventKind::Completed`].
#[must_use = "a run waits for its events to be read; drop the stream to discard them"]
#[non_exhaustive]
pub struct EventStream {
    receiver: mpsc::Receiver<RunEvent>,
    run: RunId,
}

impl EventStream {
    /// Returns the identifier every event of this stream carries, known
    /// before the run starts.
    pub fn run_id(&self) -> &RunId {
        &self.run
    }
}

impl Stream for EventStream {
    type Item = RunEvent;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<RunEvent>> {
        self.receiver.poll_recv(context)
    }
}

#[cfg(any(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
))]
tokio::task_local! {
    /// Events of the streamed run whose model request is in flight, which
    /// a built-in client reports that request's retries and deltas to.
    static REQUEST_EVENTS: RequestEvents;
}

/// Where a built-in client reports one model request of a streamed run.
#[cfg(any(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
))]
#[derive(Clone)]
pub(crate) struct RequestEvents {
    /// Sink of the run, which receives the request's retries and deltas.
    pub(crate) sink: Arc<EventSink>,
    /// Items the request's streamed deltas open, or `None` for a request
    /// whose response is no item of the run, such as a summary request.
    pub(crate) streamed: Option<Arc<StreamedItems>>,
}

/// Runs `request`, a model request of the run streaming to `events`, so a
/// built-in client reports its retries there as [`RunEventKind::Retrying`]
/// and, with `streamed`, its deltas as [`RunEventKind::ItemDelta`]. Without
/// a built-in client, nothing reports either.
pub(crate) async fn with_request_events<F: Future>(
    events: Option<Arc<EventSink>>,
    streamed: Option<Arc<StreamedItems>>,
    request: F,
) -> F::Output {
    #[cfg(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    ))]
    if let Some(sink) = events {
        return REQUEST_EVENTS
            .scope(RequestEvents { sink, streamed }, request)
            .await;
    }
    #[cfg(not(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    )))]
    let _ = (events, streamed);

    request.await
}

/// Returns the events [`with_request_events`] scoped the current model
/// request to, if any.
#[cfg(any(
    feature = "providers-anthropic",
    feature = "providers-compat",
    feature = "providers-openai"
))]
pub(crate) fn request_events() -> Option<RequestEvents> {
    REQUEST_EVENTS.try_with(RequestEvents::clone).ok()
}

/// Items one streamed model response opened, each at its first delta: its
/// reasoning, its assistant text, and its tool calls by position.
pub(crate) struct StreamedItems {
    state: Mutex<OpenItems>,
}

impl StreamedItems {
    /// Creates the items of one response of a run streaming to `events`,
    /// or `None` for a run that streams no events.
    pub(crate) fn for_run(events: Option<&Arc<EventSink>>) -> Option<Arc<Self>> {
        events.map(|_| {
            Arc::new(Self {
                state: Mutex::new(OpenItems::default()),
            })
        })
    }

    /// Streams reasoning `text` into the response's reasoning item, sending
    /// its events to `sink`.
    #[cfg(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    ))]
    pub(crate) async fn reasoning(&self, sink: &EventSink, text: String) {
        self.stream_text(sink, ItemKind::Reasoning, text).await;
    }

    /// Streams assistant `text` into the response's message item, sending
    /// its events to `sink`.
    #[cfg(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    ))]
    pub(crate) async fn text(&self, sink: &EventSink, text: String) {
        self.stream_text(sink, ItemKind::Message, text).await;
    }

    /// Streams `arguments` into the item of the response's call at
    /// position `index`, sending its events to `sink`.
    #[cfg(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    ))]
    pub(crate) async fn arguments(&self, sink: &EventSink, index: usize, arguments: String) {
        let mut state = self.state.lock().await;
        let id = if let Some(id) = state.calls.get(&index) {
            *id
        } else {
            let id = sink.start_item(ItemKind::ToolCall).await;
            state.calls.insert(index, id);

            id
        };
        sink.emit(RunEventKind::ItemDelta {
            delta: Delta::ToolArguments { arguments },
            id,
        })
        .await;
    }

    /// Streams `text` into the response's reasoning item, for
    /// [`ItemKind::Reasoning`], or else its message item, starting it at
    /// the first fragment.
    #[cfg(any(
        feature = "providers-anthropic",
        feature = "providers-compat",
        feature = "providers-openai"
    ))]
    async fn stream_text(&self, sink: &EventSink, item_kind: ItemKind, text: String) {
        let mut state = self.state.lock().await;
        let reasoning = item_kind == ItemKind::Reasoning;
        let open = if reasoning {
            &mut state.reasoning
        } else {
            &mut state.message
        };
        let id = if let Some(item) = open {
            item.text.push_str(&text);

            item.id
        } else {
            let id = sink.start_item(item_kind).await;
            *open = Some(StreamedText {
                id,
                text: text.clone(),
            });

            id
        };
        let delta = if reasoning {
            Delta::Reasoning { text }
        } else {
            Delta::Text { text }
        };
        sink.emit(RunEventKind::ItemDelta { delta, id }).await;
    }
}

/// Items a streamed response opened and the engine completes.
#[derive(Default)]
pub(crate) struct OpenItems {
    /// Tool call items by the call's position in the response.
    pub(crate) calls: HashMap<usize, ItemId>,
    pub(crate) message: Option<StreamedText>,
    pub(crate) reasoning: Option<StreamedText>,
}

impl OpenItems {
    /// Takes the items `streamed` opened, leaving none open there; none
    /// without a tracker.
    pub(crate) async fn take(streamed: Option<&StreamedItems>) -> Self {
        match streamed {
            Some(streamed) => std::mem::take(&mut *streamed.state.lock().await),
            None => Self::default(),
        }
    }
}

/// An open text item and the text streamed into it.
pub(crate) struct StreamedText {
    pub(crate) id: ItemId,
    pub(crate) text: String,
}

/// Producing side of one run's [`EventStream`].
pub(crate) struct EventSink {
    run: RunId,
    state: Mutex<SinkState>,
}

impl EventSink {
    /// Creates a sink for the run `run` and the stream that reads it.
    pub(crate) fn channel(run: RunId) -> (Arc<Self>, EventStream) {
        let (sender, receiver) = mpsc::channel(EVENT_BUFFER + FINAL_EVENTS);
        let final_permits = (0..FINAL_EVENTS)
            .filter_map(|_| sender.clone().try_reserve_owned().ok())
            .collect();
        let stream = EventStream {
            receiver,
            run: run.clone(),
        };
        let sink = Self {
            run,
            state: Mutex::new(SinkState {
                final_permits,
                next_item: 0,
                next_seq: 0,
                sender: Some(sender),
            }),
        };

        (Arc::new(sink), stream)
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
        let id = state.next_item();
        self.send(&mut state, RunEventKind::ItemStarted { id, item_kind })
            .await;

        id
    }

    /// Sends the settled `outcome`'s output item, when it succeeded, and
    /// [`RunEventKind::Completed`] through the slots reserved for them, so
    /// a full stream never holds the run back; then ends the stream.
    pub(crate) async fn complete(&self, outcome: &RunOutcome) {
        let mut state = self.state.lock().await;
        state.sender = None;
        let mut kinds = Vec::with_capacity(FINAL_EVENTS);
        if let Ok(output) = &outcome.result {
            let id = state.next_item();
            kinds.push(RunEventKind::ItemStarted {
                id,
                item_kind: ItemKind::Output,
            });
            kinds.push(RunEventKind::ItemCompleted {
                id,
                item: Item::Output {
                    output: output.clone(),
                },
            });
        }
        kinds.push(RunEventKind::Completed(RunSummary::new(outcome)));
        let permits = std::mem::take(&mut state.final_permits);
        for (permit, kind) in permits.into_iter().zip(kinds) {
            let event = self.event(&mut state, kind);
            permit.send(event);
        }
    }

    /// Sends one event under the held `state`, so sequence numbers follow
    /// delivery order. The event is numbered only once the stream has room,
    /// so a run dropped while waiting leaves no gap.
    async fn send(&self, state: &mut SinkState, kind: RunEventKind) {
        let Some(sender) = state.sender.clone() else {
            return;
        };
        // A dropped stream discards the event; the run continues.
        let Ok(permit) = sender.reserve().await else {
            return;
        };
        let event = self.event(state, kind);
        permit.send(event);
    }

    /// Numbers `kind` as the run's next event, timestamped now.
    fn event(&self, state: &mut SinkState, kind: RunEventKind) -> RunEvent {
        let now = SystemTime::now();
        let at = now.duration_since(UNIX_EPOCH).map_or(now, |elapsed| {
            UNIX_EPOCH + Duration::new(elapsed.as_secs(), elapsed.subsec_millis() * 1_000_000)
        });
        let seq = state.next_seq;
        state.next_seq += 1;

        RunEvent {
            at,
            kind,
            run: self.run.clone(),
            seq,
        }
    }
}

/// Sequence counters and the sender of one run's events.
struct SinkState {
    /// Reserved slots for the events that end the run.
    final_permits: Vec<mpsc::OwnedPermit<RunEvent>>,
    next_item: u64,
    next_seq: u64,
    /// `None` once the run completed.
    sender: Option<mpsc::Sender<RunEvent>>,
}

impl SinkState {
    /// Assigns the next item identifier.
    fn next_item(&mut self) -> ItemId {
        let id = ItemId(self.next_item);
        self.next_item += 1;

        id
    }
}

#[cfg(test)]
#[path = "event_test.rs"]
mod tests;
