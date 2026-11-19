//! Anthropic Messages API backend.
//!
//! Requests are streamed `POST {base_url}/messages` calls carrying the
//! whole history. Each system message joins the top-level `system` blocks,
//! consecutive messages of one role merge into one message, and turn
//! instructions become a user text block under the shared instructions
//! heading. Unless the policy turns it off, the last tool definition, the
//! last `system` block, and the last message's last block that is not
//! signed thinking carry a `cache_control` breakpoint, so a request that
//! repeats an earlier request's prefix reads it from the prompt cache.
//!
//! Text, thinking, and tool-argument deltas reach an observer as they
//! arrive, and the events accumulate into the same message an unstreamed
//! response carries, which is then decoded exactly as one. A server that
//! ignores `stream` and answers with one JSON body is read as before, and an
//! `error` event fails as the HTTP status its error type stands for.
//!
//! A response's `thinking` and `redacted_thinking` blocks are signed and must
//! return unchanged, in their place among the text and `tool_use` blocks, on
//! the next request. They travel as one JSON array in the provider-neutral
//! reasoning content of the response's first call or output: each thinking
//! block verbatim, with a `{"type":"text"}` marker where the text went and a
//! `{"type":"tool_use","id":…}` marker where each call went, so a replay
//! rebuilds the original block order. Other reasoning content, such as text
//! the caller cleared to save context or another provider's reasoning, is
//! omitted.

use std::sync::Arc;

use reqwest::StatusCode;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::chat_completion::TURN_INSTRUCTIONS_HEADING;
use crate::http::{
    self, HttpTransport, ProviderRequest, SUCCESS_BODY_LIMIT_BYTES, TransportError,
    deserialize_optional,
};
use crate::model::{GeneratedResponse, ModelErrorType};
use crate::provider_spec::{
    CacheBreakpoints, DEFAULT_FALLBACK_MAX_TOKENS, ProviderSpec, ReasoningMapping,
    StructuredOutputMode,
};
use crate::reasoning::ReasoningView;
use crate::retry::RetryObserver;
use crate::stream::{self, DeltaObserver, EventReader, ResponseDelta, SseDecoder, StreamClosed};
use crate::{input, model, schema as schema_contract, tool};

/// Path of the Messages endpoint under a provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "messages";
/// Name of the tool whose input is the answer under
/// [`StructuredOutputMode::ForcedTool`].
pub(crate) const OUTPUT_TOOL_NAME: &str = "structured_output";
/// Description of the [`OUTPUT_TOOL_NAME`] tool.
const OUTPUT_TOOL_DESCRIPTION: &str =
    "Give your final answer as this tool's input once you need no other tool.";
/// HTTP status each stream `error` type stands for, as the Messages API
/// answers an unstreamed request failing the same way.
const ERROR_STATUSES: [(&str, u16); 10] = [
    ("api_error", 500),
    ("authentication_error", 401),
    ("billing_error", 402),
    ("invalid_request_error", 400),
    ("not_found_error", 404),
    ("overloaded_error", 529),
    ("permission_error", 403),
    ("rate_limit_error", 429),
    ("request_too_large", 413),
    ("timeout_error", 504),
];
/// Content block types a reasoning-content array may hold.
const REPLAYED_BLOCK_TYPES: [&str; 4] = ["redacted_thinking", "text", "thinking", "tool_use"];
/// Signed reasoning block types, replayed verbatim.
const THINKING_BLOCK_TYPES: [&str; 2] = ["redacted_thinking", "thinking"];

/// Structured-output backend for the Anthropic Messages API, applying the
/// request policy of one [`ProviderSpec`].
pub(crate) struct AnthropicBackend<'spec> {
    deltas: Option<Arc<dyn DeltaObserver>>,
    headers: HeaderMap,
    http: Arc<HttpTransport>,
    model: String,
    observer: Option<Arc<dyn RetryObserver>>,
    policy: &'spec ProviderSpec,
}

impl<'spec> AnthropicBackend<'spec> {
    /// Creates a backend sending `headers` through `http`, retrying under
    /// the spec's [`crate::RetryPolicy`], and reporting retries to
    /// `observer` and streamed content to `deltas`.
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
    /// a response format, from one response.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        self.policy
            .validate_inputs(&self.model, request.messages())?;
        let output_tool = request
            .response_format
            .as_ref()
            .filter(|_| self.policy.structured_output == StructuredOutputMode::ForcedTool);
        if output_tool.is_some()
            && request
                .tools()
                .iter()
                .any(|definition| definition.name() == OUTPUT_TOOL_NAME)
        {
            return Err(model::ModelError::UnsupportedOutputSchema {
                reason: format!(
                    "{} forced-tool output reserves the tool name `{OUTPUT_TOOL_NAME}`",
                    self.policy.display_name
                ),
            });
        }
        let effort = match &self.policy.reasoning {
            ReasoningMapping::Effort(names) => request
                .model_reasoning_effort()
                .map(|effort| names.name(effort)),
            // The router rejects the other mappings for this protocol.
            ReasoningMapping::None
            | ReasoningMapping::EnableThinking
            | ReasoningMapping::Thinking(_) => None,
        };
        let format = request
            .response_format
            .as_ref()
            .filter(|_| output_tool.is_none())
            .map(|format| OutputFormat {
                kind: "json_schema",
                schema: format.schema().value(),
            });
        let mut tools: Vec<_> = request.tools().iter().map(ToolPayload::from).collect();
        // A model with a reasoning mapping may think without an effort, and
        // thinking forbids forcing a tool.
        let tool_choice = output_tool.map(|format| {
            let choice = if self.policy.reasoning != ReasoningMapping::None {
                ToolChoice::Auto
            } else if tools.is_empty() {
                ToolChoice::Tool {
                    name: OUTPUT_TOOL_NAME,
                }
            } else {
                ToolChoice::Any
            };
            tools.push(ToolPayload {
                cache_control: None,
                description: OUTPUT_TOOL_DESCRIPTION,
                input_schema: format.schema().value(),
                name: OUTPUT_TOOL_NAME,
            });

            choice
        });
        let (mut system, mut messages) = Self::messages(request);
        if self.policy.cache_breakpoints == CacheBreakpoints::Marked {
            mark_cache_breakpoints(&mut tools, &mut system, &mut messages);
        }
        let payload = MessagesPayload {
            max_tokens: request
                .options
                .max_output_tokens
                .or(self.policy.fallback_max_tokens)
                .unwrap_or(DEFAULT_FALLBACK_MAX_TOKENS)
                .get(),
            messages,
            model: &self.model,
            output_config: (effort.is_some() || format.is_some())
                .then_some(OutputConfig { effort, format }),
            stream: true,
            system,
            thinking: effort.map(|_| Thinking {
                display: "summarized",
                kind: "adaptive",
            }),
            tool_choice,
            tools,
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let request_body = ProviderRequest::new(
            self.headers.clone(),
            self.policy.endpoint(ENDPOINT_PATH),
            payload,
        )
        .with_retry(self.policy.retry, self.observer.clone());
        let response = self
            .create(&request_body, output_tool.is_some())
            .await
            .map_err(|error| {
                error.into_model_error(&self.policy.display_name, &self.policy.overflow_markers)
            })?;

        Ok(self.decode(&response, output_tool.is_some()))
    }

    /// Sends `request` and accumulates its streamed message, reporting
    /// deltas as they arrive and the input of a call to [`OUTPUT_TOOL_NAME`]
    /// as answer text when `output_tool` holds, or reads the one JSON
    /// message of a server that does not stream. A stream that fails
    /// before any content, such as on an `overloaded_error` event, is sent
    /// again as the request's retry policy retries the matching status.
    async fn create(
        &self,
        request: &ProviderRequest,
        output_tool: bool,
    ) -> Result<MessagesResponse, TransportError> {
        let mut stream_retries = 0;
        loop {
            let mut response = self.http.send(request).await?;
            if !stream::is_event_stream(&response) {
                return http::decode_success_body(&mut response).await;
            }
            let mut events = MessageStream::new(self.deltas.is_some(), output_tool);
            match self.read(&mut response, &mut events).await {
                Ok(()) => return events.finish(),
                Err(failure)
                    if events.blocks.is_empty()
                        && self
                            .http
                            .retry_streamed(request, &failure, stream_retries)
                            .await =>
                {
                    stream_retries += 1;
                }
                Err(failure) => return Err(failure),
            }
        }
    }

    /// Reads `response` into `events` until its `message_stop` or its end,
    /// reporting deltas as they arrive.
    async fn read(
        &self,
        response: &mut reqwest::Response,
        events: &mut MessageStream,
    ) -> Result<(), TransportError> {
        while let Some(chunk) = response.chunk().await.map_err(TransportError::reqwest)? {
            let done = events.push(&chunk)?;
            self.report(events).await;
            if done {
                return Ok(());
            }
        }
        events.close()?;
        self.report(events).await;

        Ok(())
    }

    /// Reports the deltas `events` read so far to the delta observer.
    async fn report(&self, events: &mut MessageStream) {
        if let Some(observer) = &self.deltas {
            for delta in std::mem::take(&mut events.deltas) {
                observer.delta(delta).await;
            }
        }
    }

    /// Returns the `system` blocks and the messages of `request`.
    fn messages(request: &model::ModelRequest) -> (Vec<Value>, Vec<MessagePayload>) {
        let mut system = Vec::new();
        let mut messages = Vec::with_capacity(request.messages().len());
        for message in request.messages() {
            let (role, blocks) = match message {
                model::ModelMessage::Assistant(content) => {
                    ("assistant", text_block(content).into_iter().collect())
                }
                model::ModelMessage::AssistantReasoning {
                    content,
                    reasoning_content,
                } => (
                    "assistant",
                    replayed_turn(Some(reasoning_content), Some(content), &[]),
                ),
                model::ModelMessage::AssistantToolCall(call) => (
                    "assistant",
                    replayed_turn(
                        call.reasoning_content(),
                        call.assistant_text(),
                        std::slice::from_ref(call),
                    ),
                ),
                model::ModelMessage::AssistantToolCalls(calls) => {
                    let first = calls.first();
                    (
                        "assistant",
                        replayed_turn(
                            first.and_then(tool::ToolCall::reasoning_content),
                            first.and_then(tool::ToolCall::assistant_text),
                            calls,
                        ),
                    )
                }
                model::ModelMessage::Instructions(content) => (
                    "user",
                    text_block(&format!("{TURN_INSTRUCTIONS_HEADING}{content}"))
                        .into_iter()
                        .collect(),
                ),
                model::ModelMessage::System(content) => {
                    system.extend(text_block(content));

                    continue;
                }
                model::ModelMessage::ToolResult {
                    call_id, content, ..
                } => (
                    "user",
                    vec![json!({
                        "type": "tool_result",
                        "tool_use_id": call_id,
                        "content": content,
                    })],
                ),
                model::ModelMessage::User(content) => {
                    ("user", text_block(content).into_iter().collect())
                }
                model::ModelMessage::UserInput(input) => ("user", user_input_blocks(input)),
            };
            push_blocks(&mut messages, role, blocks);
        }

        (system, messages)
    }

    /// Decodes a response by its stop reason: an answer, calls, or the
    /// failure the stop reason reports.
    fn decode(&self, response: &MessagesResponse, output_tool: bool) -> GeneratedResponse {
        let stop_reason = response.stop_reason.clone().unwrap_or_default();
        let content = DecodedContent::read(response.content.iter().flatten(), output_tool);
        let failure = match stop_reason.as_str() {
            "max_tokens" | "pause_turn" => Some(model::ModelError::IncompleteResponse {
                reason: stop_reason.clone(),
            }),
            "model_context_window_exceeded" => Some(model::ModelError::classified_request(
                ModelErrorType::ContextOverflow,
                None,
                Box::new(StoppedResponse {
                    provider: self.policy.display_name.clone(),
                    reason: stop_reason.clone(),
                }),
            )),
            "refusal" => Some(model::ModelError::IncompleteResponse {
                reason: schema_contract::bounded_diagnostic(response.refusal_reason()),
            }),
            _ => None,
        };
        if let Some(error) = failure {
            let reason = schema_contract::bounded_diagnostic(&stop_reason);

            return GeneratedResponse::failed(error, response.metadata(reason));
        }

        content.into_response(response)
    }
}

/// Returns the readable view of reasoning content holding Anthropic
/// thinking blocks, or `None` for other content: their thinking text, which
/// summarizes the model's reasoning, with blocks separated by blank lines,
/// or [`ReasoningView::Opaque`] when every block is redacted or empty.
pub(crate) fn reasoning_view(reasoning_content: &str) -> Option<ReasoningView<'static>> {
    let blocks = replayed_blocks(reasoning_content)?;
    let thinking: Vec<&str> = blocks
        .iter()
        .filter_map(|block| block.get("thinking").and_then(Value::as_str))
        .filter(|text| !text.trim().is_empty())
        .collect();
    if thinking.is_empty() {
        return Some(ReasoningView::Opaque);
    }

    Some(ReasoningView::Summary(thinking.join("\n\n")))
}

/// Returns the blocks of one replayed assistant turn: the recorded thinking
/// blocks in their place among `text` and the `calls`, or else the text
/// followed by the calls. Text that is empty or only whitespace, which the
/// API rejects, is left out, and a call the record does not place follows
/// the placed blocks.
fn replayed_turn(
    reasoning_content: Option<&str>,
    text: Option<&str>,
    calls: &[tool::ToolCall],
) -> Vec<Value> {
    let mut text = text.and_then(text_block);
    let mut calls: Vec<Option<&tool::ToolCall>> = calls.iter().map(Some).collect();
    let mut blocks = Vec::new();
    for marker in reasoning_content
        .and_then(replayed_blocks)
        .unwrap_or_default()
    {
        match marker.get("type").and_then(Value::as_str) {
            Some("text") => blocks.extend(text.take()),
            Some("tool_use") => {
                let id = marker.get("id").and_then(Value::as_str);
                blocks.extend(
                    calls
                        .iter_mut()
                        .find(|call| call.is_some_and(|call| Some(call.id()) == id))
                        .and_then(Option::take)
                        .map(tool_use_block),
                );
            }
            _ => blocks.push(marker),
        }
    }
    blocks.extend(text);
    blocks.extend(calls.into_iter().flatten().map(tool_use_block));

    blocks
}

/// Parses reasoning content that is a JSON array of replayed content blocks
/// holding at least one thinking block, the form a response's reasoning is
/// stored in.
fn replayed_blocks(content: &str) -> Option<Vec<Value>> {
    let blocks = serde_json::from_str::<Vec<Value>>(content).ok()?;
    let kind = |block: &Value| {
        block
            .get("type")
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let valid = blocks
        .iter()
        .all(|block| kind(block).is_some_and(|kind| REPLAYED_BLOCK_TYPES.contains(&kind.as_str())))
        && blocks.iter().any(|block| {
            kind(block).is_some_and(|kind| THINKING_BLOCK_TYPES.contains(&kind.as_str()))
        });

    valid.then_some(blocks)
}

/// Marks the stable prefix of a request with `cache_control` breakpoints:
/// the last tool definition, the last `system` block, and the last block of
/// the last message that is not signed thinking, which the API does not let
/// carry one. A later request whose prefix repeats up to a breakpoint reads
/// it from the cache. The entry written at the previous request's last
/// message carries no marker on the next request: the next request's last
/// breakpoint finds it by the API's lookback of up to 20 blocks, so a turn
/// that appends more blocks than that rewrites the conversation's cache.
/// The tools and `system` breakpoints are read however long a turn grows.
fn mark_cache_breakpoints(
    tools: &mut [ToolPayload<'_>],
    system: &mut [Value],
    messages: &mut [MessagePayload],
) {
    if let Some(tool) = tools.last_mut() {
        tool.cache_control = Some(ephemeral_cache_control());
    }
    let last_message_block = messages.last_mut().and_then(|message| {
        message
            .content
            .iter_mut()
            .rev()
            .find(|block| !is_thinking_block(block))
    });
    let marked = system
        .last_mut()
        .into_iter()
        .chain(last_message_block)
        .filter_map(Value::as_object_mut);
    for block in marked {
        block.insert("cache_control".to_string(), ephemeral_cache_control());
    }
}

/// Returns the `cache_control` of a breakpoint with the default lifetime.
fn ephemeral_cache_control() -> Value {
    json!({"type": "ephemeral"})
}

/// Returns whether `block` is signed thinking, replayed verbatim.
fn is_thinking_block(block: &Value) -> bool {
    block
        .get("type")
        .and_then(Value::as_str)
        .is_some_and(|kind| THINKING_BLOCK_TYPES.contains(&kind))
}

/// Returns a text block, or `None` for text the API rejects as blank.
fn text_block(text: &str) -> Option<Value> {
    (!text.trim().is_empty()).then(|| json!({"type": "text", "text": text}))
}

/// Returns the `tool_use` block replaying `call`; malformed arguments are
/// wrapped in an object, since the API accepts only object input.
fn tool_use_block(call: &tool::ToolCall) -> Value {
    let input = serde_json::from_str::<Value>(&call.wire_arguments()).unwrap_or_default();

    json!({
        "type": "tool_use",
        "id": call.id(),
        "name": call.name(),
        "input": input,
    })
}

/// Returns ordered text and base64 image blocks of user input.
fn user_input_blocks(input: &input::TurnInput) -> Vec<Value> {
    input
        .blocks()
        .iter()
        .filter_map(|block| match block {
            input::InputBlock::Image(image) => Some(json!({
                "type": "image",
                "source": {
                    "type": "base64",
                    "media_type": image.mime_type(),
                    "data": image.to_base64(),
                },
            })),
            input::InputBlock::Text(text) => text_block(text),
        })
        .collect()
}

/// Appends `blocks` to the last message when it has `role`, or else as a
/// new message; no blocks add nothing, since the API rejects empty content.
fn push_blocks(messages: &mut Vec<MessagePayload>, role: &'static str, blocks: Vec<Value>) {
    if blocks.is_empty() {
        return;
    }
    match messages.last_mut() {
        Some(last) if last.role == role => last.content.extend(blocks),
        _ => messages.push(MessagePayload {
            content: blocks,
            role,
        }),
    }
}

/// Content of one response, sorted into its answer, calls, and replay
/// record.
#[derive(Default)]
struct DecodedContent {
    calls: Vec<Value>,
    has_thinking: bool,
    output: Option<String>,
    record: Vec<Value>,
    text: String,
    unsupported_call: Option<String>,
}

impl DecodedContent {
    /// Sorts `blocks`, reading a call to [`OUTPUT_TOOL_NAME`] as the answer
    /// when `output_tool` holds.
    fn read<'a>(blocks: impl Iterator<Item = &'a Value>, output_tool: bool) -> Self {
        let mut content = Self::default();
        for block in blocks {
            let kind = block
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default();
            match kind {
                "redacted_thinking" | "thinking" => {
                    content.has_thinking = true;
                    content.record.push(block.clone());
                }
                "text" => {
                    content.text.push_str(
                        block
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    );
                    content.record.push(json!({"type": "text"}));
                }
                "tool_use"
                    if output_tool
                        && block.get("name").and_then(Value::as_str) == Some(OUTPUT_TOOL_NAME) =>
                {
                    content.output =
                        Some(block.get("input").cloned().unwrap_or_default().to_string());
                    content.record.push(json!({"type": "text"}));
                }
                "tool_use" => {
                    content.calls.push(block.clone());
                    content.record.push(json!({
                        "type": "tool_use",
                        "id": block.get("id").cloned().unwrap_or_default(),
                    }));
                }
                kind if kind.ends_with("tool_use") => {
                    content
                        .unsupported_call
                        .get_or_insert_with(|| schema_contract::bounded_diagnostic(kind));
                }
                _ => {}
            }
        }

        content
    }

    /// Converts the sorted content of a response that stopped normally: an
    /// unsupported call fails, calls beside a forced answer fail, calls win
    /// over text, and an answer must not be blank.
    fn into_response(self, response: &MessagesResponse) -> GeneratedResponse {
        let reasoning_content = self
            .has_thinking
            .then(|| serde_json::to_string(&self.record).ok())
            .flatten();
        if let Some(kind) = self.unsupported_call {
            return GeneratedResponse::failed(
                model::ModelError::UnsupportedToolType { kind },
                response.metadata("tool_calls".to_string()),
            );
        }
        if !self.calls.is_empty() {
            let metadata = response.metadata("tool_calls".to_string());
            if self.output.is_some() {
                return GeneratedResponse::failed(
                    model::ModelError::TerminalResponseWithToolCalls,
                    metadata,
                );
            }

            return match decode_calls(&self.calls, &self.text, reasoning_content) {
                Ok(calls) => GeneratedResponse::tool_calls(calls, metadata),
                Err(error) => GeneratedResponse::failed(error, metadata),
            };
        }
        let output = self.output.unwrap_or(self.text);
        if output.trim().is_empty() {
            return GeneratedResponse::failed(
                model::ModelError::InvalidResponse,
                response.metadata("stop".to_string()),
            );
        }

        GeneratedResponse::Output {
            metadata: response.metadata("stop".to_string()),
            output,
            reasoning_content,
        }
    }
}

/// Decodes `tool_use` blocks; the first carries the response's reasoning
/// and text, like a Chat Completions assistant message.
fn decode_calls(
    calls: &[Value],
    text: &str,
    reasoning_content: Option<String>,
) -> Result<Vec<tool::ToolCall>, model::ModelError> {
    schema_contract::ensure_content_size(text).map_err(model::ModelError::from)?;
    let mut reasoning_content = reasoning_content;
    let mut assistant_text = Some(text.to_string());
    let mut decoded = Vec::with_capacity(calls.len());
    for call in calls {
        let call = serde_json::from_value::<ToolUse>(call.clone()).map_err(|error| {
            model::ModelError::InvalidToolArguments {
                reason: schema_contract::bounded_diagnostic(error),
            }
        })?;
        decoded.push(
            tool::ToolCall::from_json(
                call.id,
                &call.name,
                &call.input.to_string(),
                reasoning_content.take(),
            )?
            .with_assistant_text(assistant_text.take())?,
        );
    }
    model::ensure_unique_tool_call_ids(&decoded)?;

    Ok(decoded)
}

/// Incremental reader of a Messages event stream that accumulates the
/// message an unstreamed response carries and, when `observed`, keeps the
/// content deltas not yet reported. The accumulated content stays within the
/// success body limit, so memory is bounded however long the stream runs.
#[derive(Default)]
struct MessageStream {
    /// Content blocks in the order they started.
    blocks: Vec<StreamedBlock>,
    /// Calls started so far, the position of the next one.
    calls: usize,
    /// Bytes of accumulated content: started blocks and their deltas.
    content_bytes: usize,
    /// Deltas read but not yet reported.
    deltas: Vec<ResponseDelta>,
    events: SseDecoder,
    observed: bool,
    /// Read a call to [`OUTPUT_TOOL_NAME`] as the answer.
    output_tool: bool,
    /// Every field but the content.
    response: MessagesResponse,
    /// The `message_stop` event arrived.
    stopped: bool,
}

impl MessageStream {
    /// Creates a reader that keeps content deltas when `observed`, reading
    /// a call to [`OUTPUT_TOOL_NAME`] as the answer when `output_tool`
    /// holds.
    fn new(observed: bool, output_tool: bool) -> Self {
        Self {
            observed,
            output_tool,
            ..Self::default()
        }
    }

    /// Returns the accumulated message, failing when the stream closed
    /// before `message_stop` or a call's streamed input is not JSON.
    fn finish(mut self) -> Result<MessagesResponse, TransportError> {
        if !self.stopped {
            return Err(TransportError::transport(StreamClosed));
        }
        self.blocks.sort_by_key(|block| block.index);
        let content = self
            .blocks
            .into_iter()
            .map(StreamedBlock::into_block)
            .collect::<Result<Vec<_>, _>>()?;
        self.response.content = Some(content);

        Ok(self.response)
    }

    /// Starts the block at wire `index`, numbering calls by the order they
    /// start; a call to [`OUTPUT_TOOL_NAME`] under forced-tool output is the
    /// answer rather than a call.
    fn start(&mut self, index: u64, block: Value) {
        let kind = block.get("type").and_then(Value::as_str);
        let answer = self.output_tool
            && kind == Some("tool_use")
            && block.get("name").and_then(Value::as_str) == Some(OUTPUT_TOOL_NAME);
        let call = (kind == Some("tool_use") && !answer).then(|| {
            self.calls += 1;

            self.calls - 1
        });
        self.blocks.push(StreamedBlock {
            answer,
            block,
            call,
            index,
            ..StreamedBlock::default()
        });
    }

    /// Accumulates one delta into the block at wire `index`, reporting its
    /// content; a delta for a block that never started is skipped.
    fn read_delta(&mut self, index: u64, delta: EventDelta) -> Result<(), TransportError> {
        let reasoned = self.blocks.iter().any(|block| block.reasoned);
        let Some(block) = self.blocks.iter_mut().rfind(|block| block.index == index) else {
            return Ok(());
        };
        let (accumulated, fragment) = match delta.kind.as_str() {
            "input_json_delta" => (&mut block.input, delta.partial_json),
            "signature_delta" => (&mut block.signature, delta.signature),
            "text_delta" => (&mut block.text, delta.text),
            "thinking_delta" => (&mut block.thinking, delta.thinking),
            _ => return Ok(()),
        };
        let fragment = fragment.unwrap_or_default();
        accumulated.push_str(&fragment);
        let bytes = fragment.len();
        let first_thought = delta.kind == "thinking_delta"
            && !fragment.is_empty()
            && !std::mem::replace(&mut block.reasoned, true);
        let reported = match delta.kind.as_str() {
            "input_json_delta" if block.answer => Some(ResponseDelta::Text(fragment)),
            "input_json_delta" => block.call.map(|index| ResponseDelta::ToolArguments {
                arguments: fragment,
                index,
            }),
            "text_delta" => Some(ResponseDelta::Text(fragment)),
            "thinking_delta" => Some(ResponseDelta::Reasoning(fragment)),
            _ => None,
        };
        self.grow(bytes)?;
        // Thinking blocks read as one text with blank lines between them,
        // as the reasoning view of the response joins them.
        if first_thought && reasoned {
            self.report(ResponseDelta::Reasoning("\n\n".to_string()));
        }
        if let Some(delta) = reported {
            self.report(delta);
        }

        Ok(())
    }

    /// Counts `bytes` more accumulated content, failing past the success
    /// body limit.
    fn grow(&mut self, bytes: usize) -> Result<(), TransportError> {
        self.content_bytes = self.content_bytes.saturating_add(bytes);
        if self.content_bytes > SUCCESS_BODY_LIMIT_BYTES {
            return Err(TransportError::ResponseBodyTooLarge);
        }

        Ok(())
    }

    /// Keeps `delta` for the observer, when there is one and the delta
    /// carries content.
    fn report(&mut self, delta: ResponseDelta) {
        let empty = match &delta {
            ResponseDelta::Reasoning(text) | ResponseDelta::Text(text) => text.is_empty(),
            ResponseDelta::ToolArguments { arguments, .. } => arguments.is_empty(),
        };
        if self.observed && !empty {
            self.deltas.push(delta);
        }
    }
}

impl EventReader for MessageStream {
    fn decoder(&mut self) -> &mut SseDecoder {
        &mut self.events
    }

    /// Reads one event, returning whether it is `message_stop`. Unknown
    /// events, such as `ping`, are skipped.
    fn dispatch(&mut self, data: &str) -> Result<bool, TransportError> {
        let event =
            serde_json::from_str::<StreamEvent>(data).map_err(TransportError::InvalidResponse)?;
        match event.kind.as_str() {
            "message_start" => {
                let message = event.message.unwrap_or_default();
                self.response.id = message.id;
                self.response.model = message.model;
                self.response.usage = message.usage;
            }
            "content_block_start" => {
                let block = event.content_block.unwrap_or_default();
                self.grow(block.to_string().len())?;
                self.start(event.index, block);
            }
            "content_block_delta" => {
                let delta = event.delta.unwrap_or_default();
                self.read_delta(event.index, delta)?;
            }
            "message_delta" => {
                let delta = event.delta.unwrap_or_default();
                self.response.stop_reason = delta.stop_reason.or(self.response.stop_reason.take());
                self.response.stop_details =
                    delta.stop_details.or(self.response.stop_details.take());
                let mut usage = self.response.usage.take().unwrap_or_default();
                usage.update(event.usage.unwrap_or_default());
                self.response.usage = Some(usage);
            }
            "message_stop" => {
                self.stopped = true;

                return Ok(true);
            }
            "error" => {
                let error = event.error.unwrap_or_default();
                let status = error.kind.as_deref().and_then(|kind| {
                    ERROR_STATUSES
                        .iter()
                        .find(|(name, _)| *name == kind)
                        .and_then(|(_, status)| StatusCode::from_u16(*status).ok())
                });

                return Err(TransportError::StreamFailed {
                    code: error.kind,
                    message: error.message,
                    status,
                });
            }
            _ => {}
        }

        Ok(false)
    }
}

/// One content block accumulated from its start and its deltas.
#[derive(Default)]
struct StreamedBlock {
    /// A call to [`OUTPUT_TOOL_NAME`] read as the answer.
    answer: bool,
    /// The block as it started.
    block: Value,
    /// Position of a call among the response's calls.
    call: Option<usize>,
    /// Wire index of the block in the message's content.
    index: u64,
    /// Partial JSON of a call's input.
    input: String,
    /// The block has streamed thinking text.
    reasoned: bool,
    signature: String,
    text: String,
    thinking: String,
}

impl StreamedBlock {
    /// Returns the block as an unstreamed response carries it: each
    /// streamed text appended to the field it started with, and a call's
    /// streamed input parsed in place of its empty start.
    fn into_block(self) -> Result<Value, TransportError> {
        let mut block = self.block;
        if let Some(fields) = block.as_object_mut() {
            for (field, fragment) in [
                ("signature", self.signature),
                ("text", self.text),
                ("thinking", self.thinking),
            ] {
                if !fragment.is_empty() {
                    let start = fields
                        .get(field)
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    let whole = format!("{start}{fragment}");
                    fields.insert(field.to_string(), Value::String(whole));
                }
            }
            if !self.input.is_empty() {
                let input = serde_json::from_str::<Value>(&self.input)
                    .map_err(TransportError::InvalidResponse)?;
                fields.insert("input".to_string(), input);
            }
        }

        Ok(block)
    }
}

/// One server-sent event of a Messages stream.
#[derive(Deserialize)]
struct StreamEvent {
    #[serde(default, deserialize_with = "deserialize_optional")]
    content_block: Option<Value>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    delta: Option<EventDelta>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    error: Option<StreamError>,
    #[serde(default)]
    index: u64,
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "deserialize_optional")]
    message: Option<MessagesResponse>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<MessagesUsage>,
}

/// The `delta` of a `content_block_delta` or `message_delta` event.
#[derive(Default, Deserialize)]
struct EventDelta {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default, deserialize_with = "deserialize_optional")]
    partial_json: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    signature: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    stop_details: Option<StopDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    stop_reason: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    text: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    thinking: Option<String>,
}

/// The failure an `error` event reports.
#[derive(Default, Deserialize)]
struct StreamError {
    #[serde(default, deserialize_with = "deserialize_optional", rename = "type")]
    kind: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    message: Option<String>,
}

/// A response the provider stopped before an answer, such as on an input
/// that filled the context window.
#[derive(Debug, thiserror::Error)]
#[error("{provider} stopped the response with `{reason}`")]
struct StoppedResponse {
    provider: String,
    reason: String,
}

#[derive(Serialize)]
struct MessagesPayload<'a> {
    max_tokens: u32,
    messages: Vec<MessagePayload>,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_config: Option<OutputConfig<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    system: Vec<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<ToolChoice>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ToolPayload<'a>>,
}

#[derive(Serialize)]
struct MessagePayload {
    content: Vec<Value>,
    role: &'static str,
}

#[derive(Serialize)]
struct OutputConfig<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    format: Option<OutputFormat<'a>>,
}

#[derive(Serialize)]
struct OutputFormat<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    schema: &'a Value,
}

#[derive(Serialize)]
struct Thinking {
    display: &'static str,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case", tag = "type")]
enum ToolChoice {
    Any,
    Auto,
    Tool { name: &'static str },
}

#[derive(Serialize)]
struct ToolPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_control: Option<Value>,
    description: &'a str,
    input_schema: &'a Value,
    name: &'a str,
}

impl<'a> From<&'a tool::ToolDefinition> for ToolPayload<'a> {
    fn from(definition: &'a tool::ToolDefinition) -> Self {
        Self {
            cache_control: None,
            description: definition.description(),
            input_schema: definition.parameters(),
            name: definition.name(),
        }
    }
}

#[derive(Default, Deserialize)]
struct MessagesResponse {
    #[serde(default, deserialize_with = "deserialize_optional")]
    content: Option<Vec<Value>>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    stop_details: Option<StopDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    stop_reason: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<MessagesUsage>,
}

impl MessagesResponse {
    fn metadata(&self, finish_reason: String) -> model::CompletionMetadata {
        model::CompletionMetadata::new(
            finish_reason,
            self.id.clone(),
            self.model.clone(),
            None,
            self.usage.as_ref().map(model::CompletionUsage::from),
        )
    }

    /// Names a refusal by its category and explanation, when reported.
    fn refusal_reason(&self) -> String {
        let details = self.stop_details.as_ref();
        [
            details.and_then(|details| details.category.as_deref()),
            details.and_then(|details| details.explanation.as_deref()),
        ]
        .into_iter()
        .flatten()
        .fold("refusal".to_string(), |reason, part| {
            format!("{reason}: {part}")
        })
    }
}

#[derive(Deserialize)]
struct StopDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    category: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    explanation: Option<String>,
}

/// Token counts of one response. The API's `input_tokens` excludes the
/// cache reads and writes, which [`model::CompletionUsage`] counts as input.
#[derive(Clone, Copy, Default, Deserialize)]
struct MessagesUsage {
    #[serde(
        default,
        deserialize_with = "deserialize_optional",
        rename = "cache_creation_input_tokens"
    )]
    cache_creation: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional",
        rename = "cache_read_input_tokens"
    )]
    cache_read: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional",
        rename = "input_tokens"
    )]
    input: Option<u64>,
    #[serde(
        default,
        deserialize_with = "deserialize_optional",
        rename = "output_tokens"
    )]
    output: Option<u64>,
}

impl MessagesUsage {
    /// Takes each count `later` reports, as the cumulative usage of a
    /// stream's `message_delta` updates the usage of its `message_start`.
    fn update(&mut self, later: Self) {
        self.cache_creation = later.cache_creation.or(self.cache_creation);
        self.cache_read = later.cache_read.or(self.cache_read);
        self.input = later.input.or(self.input);
        self.output = later.output.or(self.output);
    }
}

impl From<&MessagesUsage> for model::CompletionUsage {
    fn from(usage: &MessagesUsage) -> Self {
        let cache_miss = usage
            .input
            .map(|input| input.saturating_add(usage.cache_creation.unwrap_or(0)));
        let input = cache_miss.map(|input| input.saturating_add(usage.cache_read.unwrap_or(0)));
        let total = input
            .zip(usage.output)
            .map(|(input, output)| input.saturating_add(output));

        Self::new(
            usage.cache_read,
            cache_miss,
            input,
            usage.output,
            None,
            total,
        )
        .with_cache_write_tokens(usage.cache_creation)
    }
}

#[derive(Deserialize)]
struct ToolUse {
    id: String,
    #[serde(default)]
    input: Value,
    name: String,
}

#[cfg(test)]
#[path = "anthropic_test.rs"]
mod tests;

#[cfg(test)]
#[path = "anthropic_stream_test.rs"]
mod stream_tests;
