//! `OpenAI` Responses API backend.
//!
//! Requests are stateless and streamed: `store: false` keeps no response on
//! the provider, so every request carries its whole history, and reasoning
//! items return encrypted for verbatim replay. A response's reasoning items
//! travel as one JSON array in the provider-neutral reasoning content of its
//! first call or output, and are replayed before that assistant turn; other
//! reasoning content, such as text the caller cleared to save context or
//! another provider's reasoning, is omitted.

use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{
    self, HttpTransport, ProviderRequest, SUCCESS_BODY_LIMIT_BYTES, TransportError,
    deserialize_optional,
};
use crate::model::{GeneratedResponse, ModelErrorType};
use crate::{input, model, schema as schema_contract, tool};

/// Bound for one whole streamed response. Long reasoning at the highest
/// efforts can run for many minutes, so silence is bounded separately.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(30);
/// Longest silence tolerated on an open stream, matching Codex CLI's default
/// stream idle timeout.
const STREAM_IDLE_TIMEOUT: Duration = Duration::from_mins(5);
/// Stream events whose `response` ends the stream.
const TERMINAL_EVENTS: [&str; 3] = [
    "response.completed",
    "response.failed",
    "response.incomplete",
];

/// Provider policy applied by the Responses backend.
#[derive(Clone, Copy)]
pub(crate) struct ResponsesProviderPolicy {
    /// Lowercase provider-specific body markers of a context-window
    /// overflow, checked beside the markers every provider shares.
    pub(crate) context_overflow_markers: &'static [&'static str],
    pub(crate) display_name: &'static str,
    pub(crate) reasoning_effort: fn(model::ReasoningEffort) -> &'static str,
    pub(crate) unsupported_schema_reason: &'static str,
}

/// Structured-output backend for the `OpenAI` Responses API.
pub(crate) struct ResponsesBackend {
    api_key: Option<String>,
    base_url: String,
    http: Arc<HttpTransport>,
    model: String,
    policy: ResponsesProviderPolicy,
}

impl ResponsesBackend {
    /// Creates a backend sending requests through `http`.
    pub(crate) fn new(
        api_key: Option<String>,
        base_url: String,
        model: String,
        policy: ResponsesProviderPolicy,
        http: Arc<HttpTransport>,
    ) -> Self {
        Self {
            api_key,
            base_url,
            http,
            model,
            policy,
        }
    }

    /// Requires an object root, the only root structured outputs accept.
    pub(crate) fn validate_schema(
        &self,
        schema: &schema_contract::OutputSchema,
    ) -> Result<(), model::ModelError> {
        if !schema.has_object_root() {
            return Err(model::ModelError::UnsupportedOutputSchema {
                reason: self.policy.unsupported_schema_reason.to_string(),
            });
        }

        Ok(())
    }

    /// Generates raw structured output from one streamed response.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        self.validate_schema(request.schema())?;
        let payload = ResponsesPayload {
            include: ["reasoning.encrypted_content"],
            input: Self::input(request),
            model: &self.model,
            parallel_tool_calls: true,
            prompt_cache_key: request.options.prompt_cache_key.as_deref(),
            reasoning: request
                .model_reasoning_effort()
                .map(|effort| ReasoningPayload {
                    effort: (self.policy.reasoning_effort)(effort),
                }),
            store: false,
            stream: true,
            text: TextPayload {
                format: TextFormat {
                    kind: "json_schema",
                    name: request.response_format.name(),
                    schema: request.schema().value(),
                    strict: false,
                },
            },
            tools: request.tools().iter().map(FunctionTool::from).collect(),
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let request_body =
            ProviderRequest::new(self.api_key.as_deref(), endpoint(&self.base_url), payload);
        let response = self.create(&request_body).await.map_err(|error| {
            error.into_model_error(
                self.policy.display_name,
                self.policy.context_overflow_markers,
            )
        })?;

        Ok(self.decode(request, response))
    }

    async fn create(
        &self,
        request: &ProviderRequest<'_>,
    ) -> Result<ResponsesResponse, TransportError> {
        let mut response = self.http.send(request).await?;
        let mut events = EventStream::default();
        while let Some(chunk) = response.chunk().await.map_err(TransportError::transport)? {
            if let Some(response) = events.push(&chunk)? {
                return Ok(response);
            }
        }

        events.finish()
    }

    fn input(request: &model::ModelRequest) -> Vec<InputItem> {
        let mut items = Vec::with_capacity(request.messages().len());
        for message in request.messages() {
            match message {
                model::ModelMessage::Assistant(content) => {
                    items.push(InputItem::message("assistant", content));
                }
                model::ModelMessage::AssistantReasoning {
                    content,
                    reasoning_content,
                } => {
                    items.extend(replayed_reasoning(Some(reasoning_content)));
                    items.push(InputItem::message("assistant", content));
                }
                model::ModelMessage::AssistantToolCall(call) => {
                    items.extend(Self::tool_call_items(std::slice::from_ref(call)));
                }
                model::ModelMessage::AssistantToolCalls(calls) => {
                    items.extend(Self::tool_call_items(calls));
                }
                model::ModelMessage::System(content) => {
                    items.push(InputItem::message("developer", content));
                }
                model::ModelMessage::ToolResult {
                    call_id, content, ..
                } => items.push(InputItem::FunctionCallOutput {
                    call_id: call_id.clone(),
                    kind: "function_call_output",
                    output: content.clone(),
                }),
                model::ModelMessage::User(content) => {
                    items.push(InputItem::message("user", content));
                }
                model::ModelMessage::UserInput(input) => {
                    items.push(Self::user_input_item(input));
                }
            }
        }

        items
    }

    /// Returns ordered text and image user content; every current `OpenAI`
    /// model reads images.
    fn user_input_item(input: &input::TurnInput) -> InputItem {
        let content = input
            .blocks()
            .iter()
            .map(|block| match block {
                input::InputBlock::Image(image) => ContentPart::Image {
                    image_url: image.to_data_url(),
                    kind: "input_image",
                },
                input::InputBlock::Text(text) => ContentPart::Text {
                    kind: "input_text",
                    text: text.clone(),
                },
            })
            .collect();

        InputItem::Parts {
            content,
            kind: "message",
            role: "user",
        }
    }

    /// Replays one assistant turn of calls: its reasoning, its text, then
    /// each call in order.
    fn tool_call_items(calls: &[tool::ToolCall]) -> Vec<InputItem> {
        let first = calls.first();
        let mut items = replayed_reasoning(first.and_then(tool::ToolCall::reasoning_content));
        if let Some(text) = first.and_then(tool::ToolCall::assistant_text) {
            items.push(InputItem::message("assistant", text));
        }
        items.extend(calls.iter().map(|call| InputItem::FunctionCall {
            arguments: call.wire_arguments(),
            call_id: call.id().to_string(),
            kind: "function_call",
            name: call.name().to_string(),
        }));

        items
    }

    /// Decodes a terminal response: a completed one into output or calls, an
    /// incomplete one by its reason, and a failed one by its error.
    fn decode(
        &self,
        request: &model::ModelRequest,
        response: ResponsesResponse,
    ) -> GeneratedResponse {
        let status = response.status.as_deref().unwrap_or_default();
        match status {
            "completed" => Self::decode_completed(request, &response),
            "failed" => {
                let metadata = response.metadata("failed".to_string());

                GeneratedResponse::failed(
                    self.failure_error(response.error.unwrap_or_default()),
                    metadata,
                )
            }
            _ => {
                let reason = response
                    .incomplete_details
                    .as_ref()
                    .and_then(|details| details.reason.as_deref())
                    .unwrap_or(status);
                let reason = schema_contract::bounded_diagnostic(reason);
                let metadata = response.metadata(reason.clone());

                GeneratedResponse::failed(
                    model::ModelError::IncompleteResponse { reason },
                    metadata,
                )
            }
        }
    }

    /// Classifies a response that failed after streaming began, such as an
    /// input over the context window.
    fn failure_error(&self, failure: ResponseFailure) -> model::ModelError {
        let description = format!(
            "{} {}",
            failure.code.as_deref().unwrap_or_default(),
            failure.message.as_deref().unwrap_or_default()
        );
        let error_type = if http::has_context_overflow_marker(
            &description,
            self.policy.context_overflow_markers,
        ) {
            ModelErrorType::ContextOverflow
        } else {
            ModelErrorType::Provider
        };

        model::ModelError::classified_request(
            error_type,
            None,
            Box::new(StreamedFailure {
                code: failure.code.unwrap_or_else(|| "unknown".to_string()),
                message: schema_contract::bounded_diagnostic(failure.message.unwrap_or_default()),
                provider: self.policy.display_name,
            }),
        )
    }

    fn decode_completed(
        request: &model::ModelRequest,
        response: &ResponsesResponse,
    ) -> GeneratedResponse {
        let mut calls = Vec::new();
        let mut reasoning = Vec::new();
        let mut refusal = None;
        let mut text = String::new();
        let mut unsupported_call = None;
        for item in response.output.iter().flatten() {
            match item.get("type").and_then(Value::as_str).unwrap_or_default() {
                "function_call" => calls.push(item),
                "message" => {
                    for part in item
                        .get("content")
                        .and_then(Value::as_array)
                        .into_iter()
                        .flatten()
                    {
                        match part.get("type").and_then(Value::as_str) {
                            Some("output_text") => {
                                text.push_str(
                                    part.get("text").and_then(Value::as_str).unwrap_or_default(),
                                );
                            }
                            Some("refusal") => {
                                refusal = part.get("refusal").and_then(Value::as_str);
                            }
                            _ => {}
                        }
                    }
                }
                "reasoning" => reasoning.push(item),
                kind if kind.ends_with("_call") => {
                    unsupported_call.get_or_insert(kind);
                }
                _ => {}
            }
        }
        let reasoning_content = (!reasoning.is_empty())
            .then(|| serde_json::to_string(&reasoning).ok())
            .flatten();
        if let Some(kind) = unsupported_call {
            let kind = schema_contract::bounded_diagnostic(kind);

            return GeneratedResponse::failed(
                model::ModelError::UnsupportedToolType { kind },
                response.metadata("tool_calls".to_string()),
            );
        }
        if !calls.is_empty() {
            let metadata = response.metadata("tool_calls".to_string());

            return match Self::decode_calls(request, &calls, &text, reasoning_content) {
                Ok(calls) => GeneratedResponse::tool_calls(calls, metadata),
                Err(error) => GeneratedResponse::failed(error, metadata),
            };
        }
        if text.is_empty() {
            let error = match refusal {
                Some(refusal) => model::ModelError::IncompleteResponse {
                    reason: schema_contract::bounded_diagnostic(format!("refusal: {refusal}")),
                },
                None => model::ModelError::InvalidResponse,
            };

            return GeneratedResponse::failed(error, response.metadata("stop".to_string()));
        }

        GeneratedResponse::Output {
            metadata: response.metadata("stop".to_string()),
            output: text,
            reasoning_content,
        }
    }

    /// Decodes function calls; the first carries the response's reasoning
    /// and text, like a Chat Completions assistant message.
    fn decode_calls(
        request: &model::ModelRequest,
        calls: &[&Value],
        text: &str,
        reasoning_content: Option<String>,
    ) -> Result<Vec<tool::ToolCall>, model::ModelError> {
        schema_contract::ensure_content_size(text).map_err(model::ModelError::from)?;
        let mut reasoning_content = reasoning_content;
        let mut assistant_text = Some(text.to_string());
        let mut decoded = Vec::with_capacity(calls.len());
        for call in calls {
            let call =
                serde_json::from_value::<FunctionCall>((*call).clone()).map_err(|error| {
                    model::ModelError::InvalidToolArguments {
                        reason: schema_contract::bounded_diagnostic(error),
                    }
                })?;
            if !request.advertises_tool(&call.name) {
                return Err(model::ModelError::UnsupportedToolName {
                    name: schema_contract::bounded_diagnostic(call.name),
                });
            }
            decoded.push(
                tool::ToolCall::from_json(
                    call.call_id,
                    &call.name,
                    &call.arguments,
                    reasoning_content.take(),
                )?
                .with_assistant_text(assistant_text.take())?,
            );
        }
        model::ensure_unique_tool_call_ids(&decoded)?;

        Ok(decoded)
    }
}

/// Builds the Responses endpoint for a provider base URL.
pub(crate) fn endpoint(base_url: &str) -> String {
    format!("{}/responses", base_url.trim_end_matches('/'))
}

/// Creates the production streaming transport for Responses requests.
pub(crate) fn default_transport() -> Arc<HttpTransport> {
    Arc::new(HttpTransport::streaming(
        STREAM_IDLE_TIMEOUT,
        REQUEST_TIMEOUT,
    ))
}

/// Returns the reasoning items a prior response stored in its reasoning
/// content, or none for content that is not such an array.
fn replayed_reasoning(reasoning_content: Option<&str>) -> Vec<InputItem> {
    reasoning_content
        .and_then(|content| serde_json::from_str::<Vec<Value>>(content).ok())
        .filter(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
        .unwrap_or_default()
        .into_iter()
        .map(InputItem::Reasoning)
        .collect()
}

/// Incremental reader of a Responses server-sent event stream that keeps
/// only completed output items and the terminal response. Each pending event
/// and the completed items together stay within the success body limit, so
/// memory is bounded however long the stream runs.
#[derive(Default)]
struct EventStream {
    buffer: Vec<u8>,
    data: String,
    items: Vec<Value>,
    /// Serialized bytes of the events behind `items`.
    items_bytes: usize,
}

impl EventStream {
    /// Reads `chunk`, returning the terminal response once its event
    /// arrives.
    fn push(&mut self, chunk: &[u8]) -> Result<Option<ResponsesResponse>, TransportError> {
        self.buffer.extend_from_slice(chunk);
        while let Some(end) = self.buffer.iter().position(|byte| *byte == b'\n') {
            let line = self.buffer.drain(..=end).collect::<Vec<_>>();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if let Some(response) = self.dispatch()? {
                    return Ok(Some(response));
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                if !self.data.is_empty() {
                    self.data.push('\n');
                }
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
            }
            if self.data.len() > SUCCESS_BODY_LIMIT_BYTES {
                return Err(TransportError::ResponseBodyTooLarge);
            }
        }
        if self.buffer.len() > SUCCESS_BODY_LIMIT_BYTES {
            return Err(TransportError::ResponseBodyTooLarge);
        }

        Ok(None)
    }

    /// Reads the event left when the stream closed, failing when the stream
    /// ended before its terminal event.
    fn finish(mut self) -> Result<ResponsesResponse, TransportError> {
        // A trailing line without its newline cannot complete an event, so
        // reading it only adds its data to the pending event.
        if !self.buffer.is_empty() {
            self.push(b"\n")?;
        }

        self.dispatch()?
            .ok_or_else(|| TransportError::transport(StreamClosed))
    }

    fn dispatch(&mut self) -> Result<Option<ResponsesResponse>, TransportError> {
        let data = std::mem::take(&mut self.data);
        if data.is_empty() || data == "[DONE]" {
            return Ok(None);
        }
        let event =
            serde_json::from_str::<StreamEvent>(&data).map_err(TransportError::InvalidResponse)?;
        if event.kind == "response.output_item.done" {
            self.items_bytes = self.items_bytes.saturating_add(data.len());
            if self.items_bytes > SUCCESS_BODY_LIMIT_BYTES {
                return Err(TransportError::ResponseBodyTooLarge);
            }
            self.items.extend(event.item);

            return Ok(None);
        }
        if event.kind == "error" {
            return Ok(Some(ResponsesResponse {
                error: Some(event.error.unwrap_or(ResponseFailure {
                    code: event.code,
                    message: event.message,
                })),
                status: Some("failed".to_string()),
                ..ResponsesResponse::default()
            }));
        }
        if !TERMINAL_EVENTS.contains(&event.kind.as_str()) {
            return Ok(None);
        }
        let mut response = event.response.unwrap_or_default();
        if !self.items.is_empty() {
            response.output = Some(std::mem::take(&mut self.items));
        }

        Ok(Some(response))
    }
}

/// The stream closed before a terminal event.
#[derive(Debug, thiserror::Error)]
#[error("the response stream closed before the response completed")]
struct StreamClosed;

/// A response that failed after its stream began.
#[derive(Debug, thiserror::Error)]
#[error("{provider} response failed with `{code}`: {message}")]
struct StreamedFailure {
    code: String,
    message: String,
    provider: &'static str,
}

#[derive(Serialize)]
struct ResponsesPayload<'a> {
    include: [&'static str; 1],
    input: Vec<InputItem>,
    model: &'a str,
    parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningPayload>,
    store: bool,
    stream: bool,
    text: TextPayload<'a>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<FunctionTool<'a>>,
}

#[derive(Serialize)]
struct ReasoningPayload {
    effort: &'static str,
}

#[derive(Serialize)]
struct TextPayload<'a> {
    format: TextFormat<'a>,
}

#[derive(Serialize)]
struct TextFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    schema: &'a Value,
    strict: bool,
}

#[derive(Serialize)]
struct FunctionTool<'a> {
    description: &'a str,
    #[serde(rename = "type")]
    kind: &'static str,
    name: &'a str,
    parameters: &'a Value,
    strict: bool,
}

impl<'a> From<&'a tool::ToolDefinition> for FunctionTool<'a> {
    fn from(definition: &'a tool::ToolDefinition) -> Self {
        Self {
            description: definition.description(),
            kind: "function",
            name: definition.name(),
            parameters: definition.parameters(),
            strict: false,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum InputItem {
    FunctionCall {
        arguments: String,
        call_id: String,
        #[serde(rename = "type")]
        kind: &'static str,
        name: String,
    },
    FunctionCallOutput {
        call_id: String,
        #[serde(rename = "type")]
        kind: &'static str,
        output: String,
    },
    Parts {
        content: Vec<ContentPart>,
        #[serde(rename = "type")]
        kind: &'static str,
        role: &'static str,
    },
    /// A reasoning item replayed exactly as the provider returned it.
    Reasoning(Value),
    Text {
        content: String,
        #[serde(rename = "type")]
        kind: &'static str,
        role: &'static str,
    },
}

impl InputItem {
    fn message(role: &'static str, content: &str) -> Self {
        Self::Text {
            content: content.to_string(),
            kind: "message",
            role,
        }
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum ContentPart {
    Image {
        image_url: String,
        #[serde(rename = "type")]
        kind: &'static str,
    },
    Text {
        #[serde(rename = "type")]
        kind: &'static str,
        text: String,
    },
}

#[derive(Deserialize)]
struct StreamEvent {
    #[serde(default, deserialize_with = "deserialize_optional")]
    code: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    error: Option<ResponseFailure>,
    #[serde(default)]
    item: Option<Value>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "deserialize_optional")]
    message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    response: Option<ResponsesResponse>,
}

#[derive(Clone, Default, Deserialize)]
struct ResponsesResponse {
    #[serde(default, deserialize_with = "deserialize_optional")]
    error: Option<ResponseFailure>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    incomplete_details: Option<IncompleteDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    output: Option<Vec<Value>>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    status: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<ResponsesUsage>,
}

impl ResponsesResponse {
    fn metadata(&self, finish_reason: String) -> model::CompletionMetadata {
        model::CompletionMetadata::new(
            finish_reason,
            self.id.clone(),
            self.model.clone(),
            None,
            self.usage.as_ref().map(model::CompletionUsage::from),
        )
    }
}

#[derive(Clone, Default, Deserialize)]
struct ResponseFailure {
    #[serde(default, deserialize_with = "deserialize_optional")]
    code: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    message: Option<String>,
}

#[derive(Clone, Deserialize)]
struct IncompleteDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    reason: Option<String>,
}

#[derive(Clone, Deserialize)]
struct ResponsesUsage {
    #[serde(default, deserialize_with = "deserialize_optional")]
    input_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    input_tokens_details: Option<InputTokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    output_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    output_tokens_details: Option<OutputTokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    total_tokens: Option<u64>,
}

impl From<&ResponsesUsage> for model::CompletionUsage {
    fn from(usage: &ResponsesUsage) -> Self {
        Self::new(
            usage
                .input_tokens_details
                .as_ref()
                .and_then(|details| details.cached_tokens),
            None,
            usage.input_tokens,
            usage.output_tokens,
            usage
                .output_tokens_details
                .as_ref()
                .and_then(|details| details.reasoning_tokens),
            usage.total_tokens,
        )
    }
}

#[derive(Clone, Deserialize)]
struct InputTokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    cached_tokens: Option<u64>,
}

#[derive(Clone, Deserialize)]
struct OutputTokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct FunctionCall {
    arguments: String,
    call_id: String,
    name: String,
}

#[cfg(test)]
#[path = "responses_test.rs"]
mod tests;
