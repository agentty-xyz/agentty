//! `OpenAI` Responses API backend.
//!
//! Requests are stateless and streamed: `store: false` keeps no response on
//! the provider, so every request carries its whole history, and reasoning
//! items return encrypted for verbatim replay. Text, reasoning summary, and
//! function argument deltas reach an observer as they arrive, while the
//! completion is decoded from the completed items. A response's reasoning items
//! travel as one JSON array in the provider-neutral reasoning content of its
//! first call or output, and are replayed before that assistant turn; other
//! reasoning content, such as text the caller cleared to save context or
//! another provider's reasoning, is omitted.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{
    self, HttpTransport, ProviderRequest, SUCCESS_BODY_LIMIT_BYTES, TransportError,
    deserialize_optional,
};
use crate::model::GeneratedResponse;
use crate::provider_spec::{ProviderSpec, ReasoningMapping};
use crate::reasoning::ReasoningView;
use crate::retry::RetryObserver;
use crate::stream::{DeltaObserver, ResponseDelta, SseDecoder, StreamClosed};
use crate::{input, model, schema as schema_contract, tool};

/// Stream events whose `response` ends the stream.
const TERMINAL_EVENTS: [&str; 3] = [
    "response.completed",
    "response.failed",
    "response.incomplete",
];
/// Path of the Responses endpoint under a provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "responses";

/// Structured-output backend for the `OpenAI` Responses API, applying the
/// request policy of one [`ProviderSpec`].
pub(crate) struct ResponsesBackend<'spec> {
    deltas: Option<Arc<dyn DeltaObserver>>,
    headers: HeaderMap,
    http: Arc<HttpTransport>,
    model: String,
    observer: Option<Arc<dyn RetryObserver>>,
    policy: &'spec ProviderSpec,
}

impl<'spec> ResponsesBackend<'spec> {
    /// Creates a backend sending `headers` through `http`, reporting
    /// retries to `observer` and streamed content to `deltas`.
    pub(crate) fn new(
        policy: &'spec ProviderSpec,
        model: String,
        headers: HeaderMap,
        http: Arc<HttpTransport>,
        observer: Option<Arc<dyn RetryObserver>>,
        deltas: Option<Arc<dyn DeltaObserver>>,
    ) -> Self {
        Self {
            deltas,
            headers,
            http,
            model,
            observer,
            policy,
        }
    }

    /// Generates raw structured output, or plain text for a request without
    /// a response format, from one streamed response.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        self.policy
            .validate_inputs(&self.model, request.messages())?;
        let payload = ResponsesPayload {
            include: ["reasoning.encrypted_content"],
            input: Self::input(request),
            max_output_tokens: request.options.max_output_tokens.map(NonZeroU32::get),
            model: &self.model,
            parallel_tool_calls: true,
            prompt_cache_key: self
                .policy
                .prompt_cache_key
                .then_some(request.options.prompt_cache_key.as_deref())
                .flatten(),
            reasoning: match &self.policy.reasoning {
                ReasoningMapping::Effort(names) => {
                    request
                        .model_reasoning_effort()
                        .map(|effort| ReasoningPayload {
                            effort: names.name(effort),
                        })
                }
                // The router rejects the other mappings for this protocol.
                ReasoningMapping::None
                | ReasoningMapping::EnableThinking
                | ReasoningMapping::Thinking(_) => None,
            },
            store: false,
            stream: true,
            text: request.response_format.as_ref().map(|format| TextPayload {
                format: TextFormat {
                    kind: "json_schema",
                    name: format.name(),
                    schema: format.schema().value(),
                    strict: false,
                },
            }),
            tools: request.tools().iter().map(FunctionTool::from).collect(),
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let request_body = ProviderRequest::new(
            self.headers.clone(),
            self.policy.endpoint(ENDPOINT_PATH),
            payload,
        )
        .with_retry(self.policy.retry, self.observer.clone());
        let response = self.create(&request_body).await.map_err(|error| {
            error.into_model_error(&self.policy.display_name, &self.policy.overflow_markers)
        })?;

        Ok(self.decode(response))
    }

    async fn create(&self, request: &ProviderRequest) -> Result<ResponsesResponse, TransportError> {
        let mut response = self.http.send(request).await?;
        let mut events = EventStream::new(self.deltas.is_some());
        while let Some(chunk) = response.chunk().await.map_err(TransportError::reqwest)? {
            let terminal = events.push(&chunk)?;
            self.report(&mut events).await;
            if let Some(response) = terminal {
                return Ok(response);
            }
        }

        events.finish()
    }

    /// Reports the deltas `events` read so far to the delta observer.
    async fn report(&self, events: &mut EventStream) {
        if let Some(observer) = &self.deltas {
            for delta in std::mem::take(&mut events.deltas) {
                observer.delta(delta).await;
            }
        }
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
                model::ModelMessage::Instructions(content)
                | model::ModelMessage::System(content) => {
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
    fn decode(&self, response: ResponsesResponse) -> GeneratedResponse {
        let status = response.status.as_deref().unwrap_or_default();
        match status {
            "completed" => Self::decode_completed(&response),
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
        http::streamed_failure(
            &self.policy.display_name,
            &self.policy.overflow_markers,
            failure.code,
            failure.message,
        )
    }

    fn decode_completed(response: &ResponsesResponse) -> GeneratedResponse {
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

            return match Self::decode_calls(&calls, &text, reasoning_content) {
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

/// Returns the readable view of reasoning content holding Responses
/// reasoning items, exactly as requests replay them, or `None` for other
/// content: their summary text, with parts separated by blank lines, or
/// [`ReasoningView::Opaque`] when no item has any.
pub(crate) fn reasoning_view(reasoning_content: &str) -> Option<ReasoningView<'static>> {
    let items = reasoning_items(reasoning_content)?;
    let summaries: Vec<&str> = items
        .iter()
        .filter_map(|item| item.get("summary").and_then(Value::as_array))
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect();
    if summaries.is_empty() {
        return Some(ReasoningView::Opaque);
    }

    Some(ReasoningView::Summary(summaries.join("\n\n")))
}

/// Returns the reasoning items a prior response stored in its reasoning
/// content, or none for content that is not such an array.
fn replayed_reasoning(reasoning_content: Option<&str>) -> Vec<InputItem> {
    reasoning_content
        .and_then(reasoning_items)
        .unwrap_or_default()
        .into_iter()
        .map(InputItem::Reasoning)
        .collect()
}

/// Parses content that is a nonempty JSON array of Responses reasoning
/// items, the form a response's reasoning is stored in.
fn reasoning_items(content: &str) -> Option<Vec<Value>> {
    serde_json::from_str::<Vec<Value>>(content)
        .ok()
        .filter(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
        })
}

/// Incremental reader of a Responses server-sent event stream that keeps
/// only completed output items and the terminal response, and, when
/// `observed`, the content deltas not yet reported. Each pending event and
/// the completed items together stay within the success body limit, so
/// memory is bounded however long the stream runs.
#[derive(Default)]
struct EventStream {
    /// Position among the response's calls of each function call, by its
    /// output index.
    call_indexes: HashMap<u64, usize>,
    /// Deltas read but not yet reported.
    deltas: Vec<ResponseDelta>,
    events: SseDecoder,
    items: Vec<Value>,
    /// Serialized bytes of the events behind `items`.
    items_bytes: usize,
    observed: bool,
}

impl EventStream {
    /// Creates a reader that keeps content deltas when `observed`.
    fn new(observed: bool) -> Self {
        Self {
            observed,
            ..Self::default()
        }
    }

    /// Reads `chunk`, returning the terminal response once its event
    /// arrives; later events are not read.
    fn push(&mut self, chunk: &[u8]) -> Result<Option<ResponsesResponse>, TransportError> {
        for data in self.events.push(chunk)? {
            if let Some(response) = self.dispatch(&data)? {
                return Ok(Some(response));
            }
        }

        Ok(None)
    }

    /// Reads the event left when the stream closed, failing when the stream
    /// ended before its terminal event.
    fn finish(mut self) -> Result<ResponsesResponse, TransportError> {
        let data = std::mem::take(&mut self.events).finish()?;
        let response = match data {
            Some(data) => self.dispatch(&data)?,
            None => None,
        };

        response.ok_or_else(|| TransportError::transport(StreamClosed))
    }

    fn dispatch(&mut self, data: &str) -> Result<Option<ResponsesResponse>, TransportError> {
        if data == "[DONE]" {
            return Ok(None);
        }
        let event =
            serde_json::from_str::<StreamEvent>(data).map_err(TransportError::InvalidResponse)?;
        if self.observed {
            self.read_delta(&event);
        }
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

    /// Keeps the content delta `event` carries, numbering function calls by
    /// the order they start: when announced, or else at their first
    /// arguments delta.
    fn read_delta(&mut self, event: &StreamEvent) {
        let delta = event.delta.clone().filter(|delta| !delta.is_empty());
        match (event.kind.as_str(), delta) {
            ("response.output_item.added", _) => {
                let is_call = event
                    .item
                    .as_ref()
                    .and_then(|item| item.get("type"))
                    .and_then(Value::as_str)
                    == Some("function_call");
                if is_call {
                    self.call_index(event.output_index);
                }
            }
            // Later summary parts are separated as the readable reasoning
            // view joins them.
            ("response.reasoning_summary_part.added", _)
                if event.summary_index.is_some_and(|index| index > 0) =>
            {
                self.deltas
                    .push(ResponseDelta::Reasoning("\n\n".to_string()));
            }
            ("response.output_text.delta", Some(text)) => {
                self.deltas.push(ResponseDelta::Text(text));
            }
            (
                "response.reasoning_summary_text.delta" | "response.reasoning_text.delta",
                Some(text),
            ) => self.deltas.push(ResponseDelta::Reasoning(text)),
            ("response.function_call_arguments.delta", Some(arguments)) => {
                let index = self.call_index(event.output_index);
                self.deltas
                    .push(ResponseDelta::ToolArguments { arguments, index });
            }
            _ => {}
        }
    }

    /// Returns the position of the function call at `output_index`,
    /// numbering a call not seen before as the next one.
    fn call_index(&mut self, output_index: Option<u64>) -> usize {
        let next = self.call_indexes.len();

        *self
            .call_indexes
            .entry(output_index.unwrap_or_default())
            .or_insert(next)
    }
}

#[derive(Serialize)]
struct ResponsesPayload<'a> {
    include: [&'static str; 1],
    input: Vec<InputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
    model: &'a str,
    parallel_tool_calls: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning: Option<ReasoningPayload<'a>>,
    store: bool,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<TextPayload<'a>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<FunctionTool<'a>>,
}

#[derive(Serialize)]
struct ReasoningPayload<'a> {
    effort: &'a str,
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
    delta: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    error: Option<ResponseFailure>,
    #[serde(default)]
    item: Option<Value>,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "deserialize_optional")]
    message: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    output_index: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    response: Option<ResponsesResponse>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    summary_index: Option<u64>,
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
