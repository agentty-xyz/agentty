//! Wire types of the unstable `serve` protocol.
//!
//! Requests are parsed in two steps: [`Envelope::parse`] checks the
//! JSON-RPC 2.0 envelope, and [`Call::parse`] decodes the method's
//! parameters, so each failure maps to its JSON-RPC error code. Every
//! message the server writes is a [`Response`], an [`ErrorResponse`], a
//! [`Notification`], or a [`ServerRequest`], which the client answers;
//! [`Answer::parse`] reads those answers.
//!
//! A message without `id` is a client notification: the server executes it
//! like a request but never answers it, not even with an error.

use std::borrow::Cow;
use std::num::{NonZeroU32, NonZeroU64};
use std::path::PathBuf;
use std::time::Duration;

use ag_harness::event::{RunEvent, RunId};
use ag_harness::{
    ApprovalRequest, Decision, ErrorKind, GenerationParams, LimitKind, OutputMode, OutputSchema,
    OutputSchemaError, RunLimits, TurnRange, TurnRecord, UsageTotals,
};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// JSON Schema of every protocol message, as committed beside the crate and
/// printed by `serve --print-schema`.
pub(crate) const PROTOCOL_SCHEMA: &str = include_str!("../../schema/serve_protocol.json");

/// JSON-RPC code of a line that is not valid JSON.
pub(crate) const PARSE_ERROR: i64 = -32_700;
/// JSON-RPC code of a message that is not a valid request.
pub(crate) const INVALID_REQUEST: i64 = -32_600;
/// JSON-RPC code of an unknown method.
pub(crate) const METHOD_NOT_FOUND: i64 = -32_601;
/// JSON-RPC code of parameters that do not match the method.
pub(crate) const INVALID_PARAMS: i64 = -32_602;
/// JSON-RPC code of a harness or session failure, classified by
/// [`ErrorData`].
pub(crate) const SERVER_ERROR: i64 = -32_000;

/// The JSON-RPC version marker, always `"2.0"`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, JsonSchema, PartialEq, Serialize)]
pub(crate) enum JsonRpcVersion {
    /// JSON-RPC 2.0.
    #[serde(rename = "2.0")]
    V2,
}

/// Identifier a client gives a request, echoed by its response: an integer
/// or a string.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(transparent)]
pub(crate) struct RequestId(Value);

impl JsonSchema for RequestId {
    fn schema_name() -> Cow<'static, str> {
        "RequestId".into()
    }

    fn json_schema(_generator: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "description": "Identifier a client gives a request, echoed by its response: an \
                            integer or a string.",
            "type": ["integer", "string"]
        })
    }
}

/// A request line whose JSON-RPC envelope is valid.
#[derive(Debug, PartialEq)]
pub(crate) struct Envelope {
    /// Request identifier, or `None` for a client notification.
    pub(crate) id: Option<RequestId>,
    /// Method name.
    pub(crate) method: String,
    /// Method parameters; an empty object when the client sent none.
    pub(crate) params: Value,
}

impl Envelope {
    /// Checks the JSON-RPC 2.0 envelope of `message`.
    ///
    /// # Errors
    /// Returns an [`INVALID_REQUEST`] error, with the request's identifier
    /// when it is valid, for a message that is not a request object.
    pub(crate) fn parse(message: Value) -> Result<Self, ErrorResponse> {
        let Value::Object(mut fields) = message else {
            return Err(ErrorResponse::invalid_request(
                None,
                "message is not an object",
            ));
        };
        let id = match fields.remove("id") {
            None => None,
            Some(Value::String(id)) => Some(RequestId(Value::String(id))),
            Some(Value::Number(id)) if id.is_i64() || id.is_u64() => {
                Some(RequestId(Value::Number(id)))
            }
            Some(_) => {
                return Err(ErrorResponse::invalid_request(
                    None,
                    "`id` must be an integer or a string",
                ));
            }
        };
        if fields.get("jsonrpc").and_then(Value::as_str) != Some("2.0") {
            return Err(ErrorResponse::invalid_request(
                id,
                "`jsonrpc` must be \"2.0\"",
            ));
        }
        let Some(Value::String(method)) = fields.remove("method") else {
            return Err(ErrorResponse::invalid_request(
                id,
                "`method` must be a string",
            ));
        };
        let params = match fields.remove("params") {
            None => Value::Object(Map::new()),
            Some(params @ Value::Object(_)) => params,
            Some(_) => {
                return Err(ErrorResponse::invalid_request(
                    id,
                    "`params` must be an object",
                ));
            }
        };

        Ok(Self { id, method, params })
    }
}

/// A request's method with its decoded parameters.
///
/// On the wire, a request object carries `method` and `params` beside
/// `jsonrpc` and `id`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "method", content = "params")]
pub(crate) enum Call {
    /// Creates a durable session and opens it on this server.
    #[serde(rename = "session/create")]
    SessionCreate(SessionCreateParams),
    /// Opens an existing session on this server.
    #[serde(rename = "session/resume")]
    SessionResume(SessionResumeParams),
    /// Starts a run in an open session and returns its identifier at once;
    /// its events follow as `run/event` notifications.
    #[serde(rename = "run/start")]
    RunStart(RunStartParams),
    /// Cancels a run this server started; a run that already finished is
    /// left as it is.
    #[serde(rename = "run/cancel")]
    RunCancel(RunCancelParams),
    /// Reads settled turns of an open session.
    #[serde(rename = "session/transcript")]
    SessionTranscript(SessionTranscriptParams),
    /// Sums the tokens and cost of an open session's settled turns.
    #[serde(rename = "session/usage")]
    SessionUsage(SessionParams),
    /// Cancels every run in flight, waits for their last events, answers,
    /// and exits.
    #[serde(rename = "shutdown")]
    Shutdown(ShutdownParams),
}

impl Call {
    /// Every method name the server answers.
    pub(crate) const METHODS: [&str; 7] = [
        "session/create",
        "session/resume",
        "run/start",
        "run/cancel",
        "session/transcript",
        "session/usage",
        "shutdown",
    ];

    /// Decodes `params` for `method`.
    ///
    /// # Errors
    /// Returns a [`METHOD_NOT_FOUND`] error for an unknown method and an
    /// [`INVALID_PARAMS`] error for parameters the method does not accept.
    pub(crate) fn parse(method: &str, params: Value) -> Result<Self, ErrorObject> {
        if !Self::METHODS.contains(&method) {
            return Err(ErrorObject::new(
                METHOD_NOT_FOUND,
                format!("unknown method `{method}`"),
            ));
        }
        let call = Value::Object(Map::from_iter([
            ("method".to_string(), Value::String(method.to_string())),
            ("params".to_string(), params),
        ]));

        serde_json::from_value(call).map_err(|error| {
            ErrorObject::new(
                INVALID_PARAMS,
                format!("invalid `{method}` params: {error}"),
            )
        })
    }
}

/// Parameters of `session/create`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionCreateParams {
    /// Model the session runs.
    pub(crate) model: ModelChoice,
    /// Default form of each run's final answer.
    pub(crate) output: OutputChoice,
    /// Session identifier; a random one is generated when omitted.
    #[serde(default)]
    pub(crate) session: Option<String>,
    /// Where the session is stored; in this server's memory when omitted.
    #[serde(default)]
    pub(crate) store: StoreChoice,
    /// System prompt restored with the session.
    #[serde(default)]
    pub(crate) system_prompt: Option<String>,
}

/// Parameters of `session/resume`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionResumeParams {
    /// Model the session was created with; read from a SQLite store when
    /// omitted. A model that differs from the stored one is rejected.
    #[serde(default)]
    pub(crate) model: Option<ModelChoice>,
    /// Session identifier.
    pub(crate) session: String,
    /// Where the session is stored; in this server's memory when omitted.
    #[serde(default)]
    pub(crate) store: StoreChoice,
}

/// Parameters of `run/start`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunStartParams {
    /// Provider generation settings for the run's model requests.
    #[serde(default)]
    pub(crate) generation: GenerationChoice,
    /// Text input of the run.
    pub(crate) input: String,
    /// Host instructions for this run only.
    #[serde(default)]
    pub(crate) instructions: Option<String>,
    /// Bounds on what the run may spend.
    #[serde(default)]
    pub(crate) limits: LimitsChoice,
    /// Form of this run's final answer instead of the session default.
    #[serde(default)]
    pub(crate) output: Option<OutputChoice>,
    /// Identifier of an open session.
    pub(crate) session: String,
}

/// Parameters of `run/cancel`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct RunCancelParams {
    /// Identifier `run/start` returned.
    pub(crate) run: RunId,
}

/// Parameters of `session/transcript`.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionTranscriptParams {
    /// Which settled turns to return; every one when omitted.
    #[serde(default = "SessionTranscriptParams::all_turns")]
    pub(crate) range: TurnRange,
    /// Identifier of an open session.
    pub(crate) session: String,
}

impl SessionTranscriptParams {
    fn all_turns() -> TurnRange {
        TurnRange::All
    }
}

/// Parameters naming one open session.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct SessionParams {
    /// Identifier of an open session.
    pub(crate) session: String,
}

/// Parameters of `shutdown`: none.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct ShutdownParams {}

/// Model a session runs: a model catalog role or identifier, as
/// `{"role": "smart"}` or `{"id": "openai/gpt-6-luna"}`.
///
/// An identifier missing from the catalog that names a known provider as
/// `provider/model` runs that provider's model.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ModelChoice {
    /// A catalog identifier, or `provider/model`.
    Id(String),
    /// The first catalog candidate of this role, such as `smart`, `fast`,
    /// or `cheap`.
    Role(String),
}

/// Form of a run's final answer.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum OutputChoice {
    /// A JSON value validated against `schema`.
    Json {
        /// JSON Schema of the answer.
        schema: Value,
    },
    /// Plain assistant text.
    Text,
}

impl OutputChoice {
    /// Converts to the harness output mode.
    ///
    /// # Errors
    /// Returns the schema's rejection when it is not a usable JSON Schema.
    pub(crate) fn into_mode(self) -> Result<OutputMode, OutputSchemaError> {
        match self {
            Self::Json { schema } => Ok(OutputMode::Json(OutputSchema::new(schema)?)),
            Self::Text => Ok(OutputMode::Text),
        }
    }
}

/// Where a session is stored.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum StoreChoice {
    /// In this server's memory; sessions end with the server.
    #[default]
    Memory,
    /// In the SQLite database at `path`, created when missing.
    Sqlite {
        /// Database file.
        path: PathBuf,
    },
}

/// Provider generation settings; every unset one keeps the provider default.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct GenerationChoice {
    /// Upper bound on the tokens each model request generates.
    #[serde(default)]
    pub(crate) max_output_tokens: Option<NonZeroU32>,
}

impl From<GenerationChoice> for GenerationParams {
    fn from(choice: GenerationChoice) -> Self {
        let generation = Self::default();

        match choice.max_output_tokens {
            Some(tokens) => generation.with_max_output_tokens(tokens),
            None => generation,
        }
    }
}

/// Bounds on what one run may spend; every unset limit is unlimited.
#[derive(Debug, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub(crate) struct LimitsChoice {
    /// Model requests the run may send.
    #[serde(default, rename = "max_model_requests")]
    pub(crate) model_requests: Option<NonZeroU32>,
    /// Tool calls the model may request.
    #[serde(default, rename = "max_tool_calls")]
    pub(crate) tool_calls: Option<NonZeroU32>,
    /// Input plus output tokens of the run's model requests.
    #[serde(default, rename = "max_total_tokens")]
    pub(crate) total_tokens: Option<NonZeroU64>,
    /// Wall-clock milliseconds from the start of the run's turn.
    #[serde(default, rename = "max_wall_time_ms")]
    pub(crate) wall_time_ms: Option<NonZeroU64>,
}

impl From<LimitsChoice> for RunLimits {
    fn from(choice: LimitsChoice) -> Self {
        let mut limits = Self::default();
        if let Some(requests) = choice.model_requests {
            limits = limits.with_max_model_requests(requests);
        }
        if let Some(tool_calls) = choice.tool_calls {
            limits = limits.with_max_tool_calls(tool_calls);
        }
        if let Some(tokens) = choice.total_tokens {
            limits = limits.with_max_total_tokens(tokens);
        }
        if let Some(millis) = choice.wall_time_ms {
            limits = limits.with_max_wall_time(Duration::from_millis(millis.get()));
        }

        limits
    }
}

/// Result of a successful request: one closed object per method, so no
/// result shape validates as another.
#[derive(Debug, JsonSchema, Serialize)]
#[serde(untagged)]
pub(crate) enum MethodResult {
    /// `session/create` and `session/resume`.
    SessionOpened(SessionOpened),
    /// `run/start`.
    RunStarted(RunStarted),
    /// `session/transcript`.
    Transcript(Transcript),
    /// `session/usage`.
    Usage(SessionUsage),
    /// `run/cancel` and `shutdown`.
    Empty(Empty),
}

/// Result of `session/create` and `session/resume`: the opened session.
#[derive(Debug, JsonSchema, Serialize)]
#[schemars(deny_unknown_fields)]
pub(crate) struct SessionOpened {
    /// Identifier of the open session.
    pub(crate) session: String,
}

/// Result of `run/start`: the started run, whose events follow as
/// `run/event` notifications.
#[derive(Debug, JsonSchema, Serialize)]
#[schemars(deny_unknown_fields)]
pub(crate) struct RunStarted {
    /// Identifier every event of the run carries.
    pub(crate) run: RunId,
}

/// Result of `session/transcript`: the selected settled turns, oldest
/// first.
#[derive(Debug, JsonSchema, Serialize)]
#[schemars(deny_unknown_fields)]
pub(crate) struct Transcript {
    /// Settled turns.
    pub(crate) turns: Vec<TurnRecord>,
}

/// Result of `session/usage`: summed tokens and cost of the settled turns.
#[derive(Debug, Serialize)]
#[serde(transparent)]
pub(crate) struct SessionUsage(pub(crate) UsageTotals);

impl JsonSchema for SessionUsage {
    fn schema_name() -> Cow<'static, str> {
        "SessionUsage".into()
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let mut schema = UsageTotals::json_schema(generator);
        schema.insert(
            "description".to_string(),
            Value::String(
                "Result of `session/usage`: summed tokens and cost of the settled turns."
                    .to_string(),
            ),
        );
        schema.insert("additionalProperties".to_string(), Value::Bool(false));

        schema
    }
}

/// Result of `run/cancel` and `shutdown`: an empty object.
#[derive(Debug, JsonSchema, Serialize)]
#[schemars(deny_unknown_fields)]
pub(crate) struct Empty {}

/// Successful response to a request.
#[derive(Debug, JsonSchema, Serialize)]
pub(crate) struct Response {
    /// Identifier of the request.
    pub(crate) id: RequestId,
    /// JSON-RPC version.
    pub(crate) jsonrpc: JsonRpcVersion,
    /// Method result.
    pub(crate) result: MethodResult,
}

impl Response {
    /// Answers request `id` with `result`.
    pub(crate) fn new(id: RequestId, result: MethodResult) -> Self {
        Self {
            id,
            jsonrpc: JsonRpcVersion::V2,
            result,
        }
    }
}

/// Failed response to a request, or to a line that was not one.
#[derive(Debug, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ErrorResponse {
    /// What failed.
    pub(crate) error: ErrorObject,
    /// Identifier of the request, or `null` when it could not be read.
    pub(crate) id: Option<RequestId>,
    /// JSON-RPC version.
    pub(crate) jsonrpc: JsonRpcVersion,
}

impl ErrorResponse {
    /// Answers request `id`, or an unreadable line, with `error`.
    pub(crate) fn new(id: Option<RequestId>, error: ErrorObject) -> Self {
        Self {
            error,
            id,
            jsonrpc: JsonRpcVersion::V2,
        }
    }

    /// Rejects a message that is not a valid request.
    fn invalid_request(id: Option<RequestId>, reason: &str) -> Self {
        Self::new(
            id,
            ErrorObject::new(INVALID_REQUEST, format!("invalid request: {reason}")),
        )
    }
}

/// JSON-RPC error object.
#[derive(Debug, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ErrorObject {
    /// JSON-RPC error code: `-32700` for unreadable JSON, `-32600` for an
    /// invalid request, `-32601` for an unknown method, `-32602` for
    /// invalid parameters, and `-32000` for a harness failure.
    pub(crate) code: i64,
    /// Classification of a `-32000` harness failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) data: Option<ErrorData>,
    /// Bounded failure description.
    pub(crate) message: String,
}

impl ErrorObject {
    /// Describes a protocol failure without classification.
    pub(crate) fn new(code: i64, message: String) -> Self {
        Self {
            code,
            data: None,
            message,
        }
    }
}

/// Classification of a harness failure.
#[derive(Debug, JsonSchema, PartialEq, Serialize)]
pub(crate) struct ErrorData {
    /// Failure classification, by its stable name.
    pub(crate) kind: ErrorKind,
    /// The run limit a `budget_exhausted` failure reached.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) limit: Option<LimitKind>,
    /// Whether retrying the same request later may succeed.
    pub(crate) retryable: bool,
}

impl ErrorData {
    /// Classifies a failure of `kind`.
    pub(crate) fn new(kind: ErrorKind, retryable: bool) -> Self {
        let limit = match kind {
            ErrorKind::BudgetExhausted(limit) => Some(limit),
            _ => None,
        };

        Self {
            kind,
            limit,
            retryable,
        }
    }
}

/// Server notification carrying one run event.
#[derive(Debug, JsonSchema, Serialize)]
pub(crate) struct Notification {
    /// JSON-RPC version.
    pub(crate) jsonrpc: JsonRpcVersion,
    /// Notification method.
    pub(crate) method: NotificationMethod,
    /// The event and the session of its run.
    pub(crate) params: RunEventParams,
}

impl Notification {
    /// Notifies `event` of a run in `session`.
    pub(crate) fn run_event(session: String, event: RunEvent) -> Self {
        Self {
            jsonrpc: JsonRpcVersion::V2,
            method: NotificationMethod::RunEvent,
            params: RunEventParams { event, session },
        }
    }
}

/// Method of a server notification.
#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
pub(crate) enum NotificationMethod {
    /// One event of a run, in order; the run's `completed` event is its
    /// last.
    #[serde(rename = "run/event")]
    RunEvent,
}

/// Parameters of a `run/event` notification.
#[derive(Debug, JsonSchema, Serialize)]
pub(crate) struct RunEventParams {
    /// The run event, as `ag-harness` streams it.
    pub(crate) event: RunEvent,
    /// Identifier of the run's session.
    pub(crate) session: String,
}

/// Server request asking the client to decide on one tool call that
/// changes state. The client answers with a response carrying the same
/// `id` and an [`ApprovalDecision`] as its `result`; an error response
/// denies the call. A run whose answer does not arrive within the
/// server's approval timeout fails with `approval_timeout`.
#[derive(Debug, JsonSchema, Serialize)]
pub(crate) struct ServerRequest {
    /// Identifier the client's answer must carry, unique within this
    /// server.
    pub(crate) id: RequestId,
    /// JSON-RPC version.
    pub(crate) jsonrpc: JsonRpcVersion,
    /// Request method.
    pub(crate) method: ServerMethod,
    /// The call waiting for a decision and the session of its run.
    pub(crate) params: ApprovalParams,
}

impl ServerRequest {
    /// Asks the client, under identifier `id`, to decide on `request`
    /// made by a run in `session`.
    pub(crate) fn approval(id: String, session: String, request: ApprovalRequest) -> Self {
        Self {
            id: RequestId(Value::String(id)),
            jsonrpc: JsonRpcVersion::V2,
            method: ServerMethod::ApprovalDecide,
            params: ApprovalParams { request, session },
        }
    }
}

/// Method of a server request.
#[derive(Clone, Copy, Debug, JsonSchema, Serialize)]
pub(crate) enum ServerMethod {
    /// Asks the client to decide on a tool call; it follows the run's
    /// `approval_requested` event for the call.
    #[serde(rename = "approval/decide")]
    ApprovalDecide,
}

/// Parameters of an `approval/decide` request.
#[derive(Debug, JsonSchema, Serialize)]
pub(crate) struct ApprovalParams {
    /// The call waiting for a decision, as the run's `approval_requested`
    /// event carries it.
    pub(crate) request: ApprovalRequest,
    /// Identifier of the run's session.
    pub(crate) session: String,
}

/// A client's decision on an `approval/decide` request, the `result` of
/// its answer.
#[derive(Debug, Deserialize, JsonSchema, PartialEq)]
#[serde(tag = "decision", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum ApprovalDecision {
    /// Runs the call with the model's arguments.
    // A struct variant, so serde rejects other fields as the schema states.
    Allow {},
    /// Runs the call with `arguments` instead, which must match the tool's
    /// parameters or the run fails as a tool failure.
    AllowWith {
        /// Replacement arguments, a JSON object.
        arguments: Value,
    },
    /// Does not run the call; the model receives `feedback` as its result.
    Deny {
        /// Why the call was denied, sent to the model.
        feedback: String,
    },
}

impl From<ApprovalDecision> for Decision {
    fn from(decision: ApprovalDecision) -> Self {
        match decision {
            ApprovalDecision::Allow {} => Self::Allow,
            ApprovalDecision::AllowWith { arguments } => Self::AllowWith(arguments),
            ApprovalDecision::Deny { feedback } => Self::Deny { feedback },
        }
    }
}

/// A client's answer to a server request.
#[derive(Debug, PartialEq)]
pub(crate) struct Answer {
    /// Decision, or why the answer's `result` is not one.
    pub(crate) decision: Result<Decision, String>,
    /// Identifier of the answered server request.
    pub(crate) id: Value,
}

impl Answer {
    /// Reads `message` as an answer to a server request: an object without
    /// `method` that carries `id` and exactly one of `result` and `error`.
    /// An error answer denies the call with the error's message. Returns
    /// `None` for any other message, which is read as a request.
    pub(crate) fn parse(message: &Value) -> Option<Self> {
        let fields = message.as_object()?;
        if fields.contains_key("method") {
            return None;
        }
        let id = fields.get("id")?.clone();
        let decision = match (fields.get("result"), fields.get("error")) {
            (Some(result), None) => serde_json::from_value::<ApprovalDecision>(result.clone())
                .map(Decision::from)
                .map_err(|error| error.to_string()),
            (None, Some(error)) => Ok(Decision::Deny {
                feedback: error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("the client did not decide")
                    .to_string(),
            }),
            _ => return None,
        };

        Some(Self { decision, id })
    }
}

/// Serializes `message` as one line of JSON, newline included.
///
/// # Errors
/// Returns the serialization failure, such as an event time before the
/// Unix epoch.
pub(crate) fn line(message: &impl Serialize) -> Result<String, serde_json::Error> {
    let mut line = serde_json::to_string(message)?;
    line.push('\n');

    Ok(line)
}

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
