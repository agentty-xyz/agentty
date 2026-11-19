//! Shared structured-output backend for `OpenAI`-compatible Chat
//! Completions APIs.
//!
//! Requests are streamed: text, `reasoning_content`, and tool-argument
//! deltas reach an observer as they arrive, and the chunks accumulate into
//! the same completion an unstreamed response decodes to. A server that
//! ignores `stream` and answers with one JSON body is read as before.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::sync::Arc;

use reqwest::header::{CONTENT_TYPE, HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{
    self, HttpTransport, ProviderRequest, SUCCESS_BODY_LIMIT_BYTES, TransportError,
    deserialize_optional,
};
use crate::model::GeneratedResponse;
use crate::provider_spec::{
    InstructionPlacement, MaxTokensField, ProviderSpec, ReasoningMapping, StreamUsage,
    StructuredOutputMode, ThinkingOptions,
};
use crate::retry::RetryObserver;
use crate::stream::{DeltaObserver, ResponseDelta, SseDecoder, StreamClosed};
use crate::{input, model, schema as schema_contract, tool};

/// Path of the Chat Completions endpoint under a provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "chat/completions";
pub(crate) const STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Return only one JSON object. The object must validate against this JSON Schema. ",
    "Do not include Markdown fences or any other text.\n\nJSON Schema:\n",
);
/// Schema instruction for [`StructuredOutputMode::SchemaPrompt`]. Small local
/// models read an unconditional "return only one JSON object" as forbidding
/// tool calls: live, Qwen3 8B on Ollama answered 0 of 2 tool-requiring
/// prompts with a call under it and 4 of 4 under this wording.
pub(crate) const TOOL_AWARE_STRUCTURED_OUTPUT_INSTRUCTION: &str = concat!(
    "Call the offered tools whenever you need them. When you give your final answer, ",
    "return only one JSON object that validates against this JSON Schema, with no Markdown ",
    "fences or other text.\n\nJSON Schema:\n",
);

/// Heading of the block carrying [`model::ModelMessage::Instructions`],
/// which marks the application's instructions apart from the user's input
/// and from the harness's own `[harness]` notes. Chat Completions has no
/// host-only role that every provider template accepts mid-conversation, so
/// the block joins the user message it follows: as its own text part, or
/// after [`TURN_INSTRUCTIONS_SEPARATOR`] in plain text.
pub(crate) const TURN_INSTRUCTIONS_HEADING: &str = "[application instructions for this turn]\n";
/// Separator between a plain-text user message and the instructions block
/// appended to it.
pub(crate) const TURN_INSTRUCTIONS_SEPARATOR: &str = "\n\n";

/// Shared structured-output backend for OpenAI-compatible Chat Completions
/// APIs, applying the request policy of one [`ProviderSpec`].
pub(crate) struct ChatCompletionBackend<'spec> {
    deltas: Option<Arc<dyn DeltaObserver>>,
    headers: HeaderMap,
    http: Arc<HttpTransport>,
    model: String,
    observer: Option<Arc<dyn RetryObserver>>,
    policy: &'spec ProviderSpec,
}

impl<'spec> ChatCompletionBackend<'spec> {
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
    /// a response format, through the shared wire lifecycle.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        let tools: Vec<_> = request
            .tools()
            .iter()
            .map(ChatCompletionTool::from)
            .collect();
        let response_format = request
            .response_format
            .as_ref()
            .filter(|_| tools.is_empty() || self.policy.response_format_with_tools)
            .map(|format| self.response_format(format));
        let max_output_tokens = request.options.max_output_tokens.map(NonZeroU32::get);
        // The router rejects a bound the provider does not accept before any
        // request, so an unsupported field sends none.
        let (max_completion_tokens, max_tokens) = match self.policy.max_tokens_field {
            Some(MaxTokensField::MaxCompletionTokens) => (max_output_tokens, None),
            Some(MaxTokensField::MaxTokens) => (None, max_output_tokens),
            Some(MaxTokensField::MaxOutputTokens) | None => (None, None),
        };
        let payload = ChatCompletionPayload {
            enable_thinking: self.enable_thinking(request),
            max_completion_tokens,
            max_tokens,
            messages: self.messages(request)?,
            model: &self.model,
            prompt_cache_key: self
                .policy
                .prompt_cache_key
                .then_some(request.options.prompt_cache_key.as_deref())
                .flatten(),
            reasoning_effort: self.reasoning_effort(request),
            response_format,
            stream: true,
            stream_options: (self.policy.stream_usage == StreamUsage::Requested).then_some(
                StreamOptions {
                    include_usage: true,
                },
            ),
            thinking: self.thinking(request),
            tools,
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let request_body = ProviderRequest::new(
            self.headers.clone(),
            self.policy.endpoint(ENDPOINT_PATH),
            payload,
        )
        .with_retry(self.policy.retry, self.observer.clone());
        let completion = self
            .create(&request_body)
            .await
            .map_err(|error| self.map_completion_error(error))?
            .ok_or(model::ModelError::InvalidResponse)?;
        let (metadata, content, reasoning_content, tool_calls) = completion.into_parts();
        match metadata.finish_reason() {
            "stop" if !tool_calls.is_empty() => Ok(GeneratedResponse::failed(
                model::ModelError::TerminalResponseWithToolCalls,
                metadata,
            )),
            "stop" => Ok(match content {
                Some(output) => GeneratedResponse::Output {
                    metadata,
                    output,
                    reasoning_content: self
                        .policy
                        .replay
                        .reasoning_text
                        .then_some(reasoning_content)
                        .flatten(),
                },
                None => GeneratedResponse::failed(model::ModelError::InvalidResponse, metadata),
            }),
            "tool_calls" => Ok(Self::decode_tool_call(
                content.as_deref(),
                self.policy
                    .replay
                    .reasoning_text
                    .then_some(reasoning_content.as_deref())
                    .flatten(),
                tool_calls,
                metadata,
            )),
            _ => {
                let reason = schema_contract::bounded_diagnostic(metadata.finish_reason());

                Ok(GeneratedResponse::failed(
                    model::ModelError::IncompleteResponse { reason },
                    metadata,
                ))
            }
        }
    }

    fn messages(
        &self,
        request: &model::ModelRequest,
    ) -> Result<Vec<ChatCompletionMessagePayload>, model::ModelError> {
        let mut messages = Vec::with_capacity(request.messages().len() + 1);
        let prompt_schema = request
            .options
            .prefix_schema
            .as_ref()
            .or_else(|| request.schema());
        if self.policy.keeps_prefix_across_schemas()
            && let Some(schema) = prompt_schema
        {
            let instruction = match self.policy.structured_output {
                StructuredOutputMode::SchemaPrompt => TOOL_AWARE_STRUCTURED_OUTPUT_INSTRUCTION,
                StructuredOutputMode::JsonObject
                | StructuredOutputMode::JsonSchema
                | StructuredOutputMode::ForcedTool => STRUCTURED_OUTPUT_INSTRUCTION,
            };
            messages.push(ChatCompletionMessagePayload::Text {
                content: format!("{instruction}{}", schema.value()),
                role: "system",
            });
        }
        for message in request.messages() {
            match message {
                model::ModelMessage::Assistant(content) => {
                    messages.push(ChatCompletionMessagePayload::Text {
                        content: content.clone(),
                        role: "assistant",
                    });
                }
                model::ModelMessage::AssistantReasoning {
                    content,
                    reasoning_content,
                } => {
                    messages.push(ChatCompletionMessagePayload::AssistantReasoning {
                        content: content.clone(),
                        reasoning_content: reasoning_content.clone(),
                        role: "assistant",
                    });
                }
                model::ModelMessage::Instructions(content) => {
                    Self::push_instructions(
                        &mut messages,
                        content,
                        self.policy.instruction_placement,
                    );
                }
                model::ModelMessage::System(content) => {
                    messages.push(ChatCompletionMessagePayload::Text {
                        content: content.clone(),
                        role: "system",
                    });
                }
                model::ModelMessage::User(content) => {
                    messages.push(ChatCompletionMessagePayload::Text {
                        content: content.clone(),
                        role: "user",
                    });
                }
                model::ModelMessage::UserInput(input) => {
                    messages.push(self.user_input_message(input)?);
                }
                model::ModelMessage::AssistantToolCall(call) => {
                    messages.push(self.assistant_tool_call_message(std::slice::from_ref(call)));
                }
                model::ModelMessage::AssistantToolCalls(calls) => {
                    messages.push(self.assistant_tool_call_message(calls));
                }
                model::ModelMessage::ToolResult {
                    call_id,
                    content,
                    name,
                } => {
                    messages.push(ChatCompletionMessagePayload::ToolResult {
                        content: content.clone(),
                        name: self.policy.replay.tool_result_name.then(|| name.clone()),
                        role: "tool",
                        tool_call_id: call_id.clone(),
                    });
                }
            }
        }

        Ok(messages)
    }

    /// Pushes turn `instructions` as their own user message or, under
    /// [`InstructionPlacement::JoinPreviousUser`], appends them to the user
    /// message they follow, so templates that require alternating roles
    /// never see two user messages in a row; without a preceding user
    /// message they always form their own.
    fn push_instructions(
        messages: &mut Vec<ChatCompletionMessagePayload>,
        instructions: &str,
        placement: InstructionPlacement,
    ) {
        let block = format!("{TURN_INSTRUCTIONS_HEADING}{instructions}");
        let last = match placement {
            InstructionPlacement::JoinPreviousUser => messages.last_mut(),
            InstructionPlacement::Separate => None,
        };
        match last {
            Some(ChatCompletionMessagePayload::Text {
                content,
                role: "user",
            }) => {
                content.push_str(TURN_INSTRUCTIONS_SEPARATOR);
                content.push_str(&block);
            }
            Some(ChatCompletionMessagePayload::Parts {
                content,
                role: "user",
            }) => content.push(ChatCompletionContentPart::Text {
                kind: "text",
                text: block,
            }),
            _ => messages.push(ChatCompletionMessagePayload::Text {
                content: block,
                role: "user",
            }),
        }
    }

    fn user_input_message(
        &self,
        input: &input::TurnInput,
    ) -> Result<ChatCompletionMessagePayload, model::ModelError> {
        self.policy.validate_input(&self.model, input)?;
        let content = input
            .blocks()
            .iter()
            .map(|block| match block {
                input::InputBlock::Image(image) => ChatCompletionContentPart::ImageUrl {
                    image_url: ChatCompletionImageUrl {
                        url: image.to_data_url(),
                    },
                    kind: "image_url",
                },
                input::InputBlock::Text(text) => ChatCompletionContentPart::Text {
                    kind: "text",
                    text: text.clone(),
                },
            })
            .collect();

        Ok(ChatCompletionMessagePayload::Parts {
            content,
            role: "user",
        })
    }

    fn assistant_tool_call_message(
        &self,
        calls: &[tool::ToolCall],
    ) -> ChatCompletionMessagePayload {
        let reasoning_content = self
            .policy
            .replay
            .reasoning_text
            .then(|| {
                calls
                    .first()
                    .and_then(tool::ToolCall::reasoning_content)
                    .map(str::to_string)
            })
            .flatten();
        let tool_calls = calls
            .iter()
            .map(|call| ChatCompletionOutgoingToolCall {
                function: ChatCompletionOutgoingFunctionCall {
                    arguments: call.wire_arguments(),
                    name: call.name().to_string(),
                },
                id: call.id().to_string(),
                kind: "function",
            })
            .collect();

        ChatCompletionMessagePayload::AssistantToolCall {
            content: calls
                .first()
                .and_then(tool::ToolCall::assistant_text)
                .map(str::to_string),
            reasoning_content,
            role: "assistant",
            tool_calls,
        }
    }

    fn response_format<'a>(&self, format: &'a model::JsonSchemaFormat) -> ResponseFormat<'a> {
        match self.policy.structured_output {
            StructuredOutputMode::JsonObject => ResponseFormat {
                json_schema: None,
                kind: "json_object",
            },
            // The router rejects a forced tool for this protocol.
            StructuredOutputMode::JsonSchema
            | StructuredOutputMode::SchemaPrompt
            | StructuredOutputMode::ForcedTool => ResponseFormat {
                json_schema: Some(JsonSchemaResponseFormat {
                    name: format.name(),
                    schema: format.schema().value(),
                }),
                kind: "json_schema",
            },
        }
    }

    fn enable_thinking(&self, request: &model::ModelRequest) -> Option<bool> {
        match self.policy.reasoning {
            ReasoningMapping::EnableThinking => {
                request.model_reasoning_effort().map(reasoning_enabled)
            }
            ReasoningMapping::Effort(_)
            | ReasoningMapping::None
            | ReasoningMapping::Thinking(_) => None,
        }
    }

    fn reasoning_effort(&self, request: &model::ModelRequest) -> Option<&'spec str> {
        match &self.policy.reasoning {
            ReasoningMapping::Effort(names) => request
                .model_reasoning_effort()
                .map(|effort| names.name(effort)),
            ReasoningMapping::EnableThinking
            | ReasoningMapping::None
            | ReasoningMapping::Thinking(_) => None,
        }
    }

    fn thinking(&self, request: &model::ModelRequest) -> Option<Thinking> {
        match self.policy.reasoning {
            ReasoningMapping::Thinking(ThinkingOptions {
                disable_supported,
                preserve_reasoning,
            }) => {
                let enabled = request
                    .model_reasoning_effort()
                    .map_or(preserve_reasoning, |reasoning_effort| {
                        !disable_supported || reasoning_enabled(reasoning_effort)
                    });

                (request.model_reasoning_effort().is_some() || preserve_reasoning).then(|| {
                    Thinking {
                        keep: (enabled && preserve_reasoning).then_some("all"),
                        kind: if enabled { "enabled" } else { "disabled" },
                    }
                })
            }
            ReasoningMapping::Effort(_)
            | ReasoningMapping::EnableThinking
            | ReasoningMapping::None => None,
        }
    }

    /// Sends `request` and reads its streamed completion, reporting deltas
    /// as they arrive, or the one JSON completion of a server that does not
    /// stream; `None` for a response without choices.
    async fn create(
        &self,
        request: &ProviderRequest,
    ) -> Result<Option<ChatCompletion>, TransportError> {
        let mut response = self.http.send(request).await?;
        let streamed = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim_start().starts_with("text/event-stream"));
        if !streamed {
            let response =
                http::decode_success_body::<ChatCompletionResponse>(&mut response).await?;

            return Ok(response.into_completion());
        }
        let mut chunks = ChunkStream::new(self.deltas.is_some());
        while let Some(chunk) = response.chunk().await.map_err(TransportError::reqwest)? {
            let done = chunks.push(&chunk)?;
            if let Some(observer) = &self.deltas {
                for delta in std::mem::take(&mut chunks.deltas) {
                    observer.delta(delta).await;
                }
            }
            if done {
                break;
            }
        }

        chunks.finish().map(Some)
    }

    fn map_completion_error(&self, error: TransportError) -> model::ModelError {
        error.into_model_error(&self.policy.display_name, &self.policy.overflow_markers)
    }

    fn decode_tool_call(
        content: Option<&str>,
        reasoning_content: Option<&str>,
        calls: Vec<ChatCompletionToolCall>,
        metadata: model::CompletionMetadata,
    ) -> GeneratedResponse {
        match Self::decode_tool_call_parts(content, reasoning_content, calls) {
            Ok(calls) => GeneratedResponse::tool_calls(calls, metadata),
            Err(error) => GeneratedResponse::failed(error, metadata),
        }
    }

    fn decode_tool_call_parts(
        content: Option<&str>,
        reasoning_content: Option<&str>,
        calls: Vec<ChatCompletionToolCall>,
    ) -> Result<Vec<tool::ToolCall>, model::ModelError> {
        if let Some(content) = content {
            schema_contract::ensure_content_size(content).map_err(model::ModelError::from)?;
        }
        if calls.is_empty() {
            return Err(model::ModelError::MissingToolCall);
        }
        let mut decoded = Vec::with_capacity(calls.len());
        for (index, call) in calls.into_iter().enumerate() {
            let first = index == 0;
            decoded.push(
                Self::decode_one_tool_call(call, first.then_some(reasoning_content).flatten())?
                    .with_assistant_text(first.then_some(content).flatten().map(str::to_string))?,
            );
        }
        model::ensure_unique_tool_call_ids(&decoded)?;

        Ok(decoded)
    }

    fn decode_one_tool_call(
        call: ChatCompletionToolCall,
        reasoning_content: Option<&str>,
    ) -> Result<tool::ToolCall, model::ModelError> {
        if call.kind != "function" {
            return Err(model::ModelError::UnsupportedToolType {
                kind: schema_contract::bounded_diagnostic(call.kind),
            });
        }
        let function = serde_json::from_value::<ChatCompletionFunctionCall>(call.function)
            .map_err(|error| model::ModelError::InvalidToolArguments {
                reason: schema_contract::bounded_diagnostic(error),
            })?;
        tool::ToolCall::from_json(
            call.id,
            &function.name,
            &function.arguments,
            reasoning_content.map(str::to_string),
        )
    }
}

fn reasoning_enabled(reasoning_effort: model::ReasoningEffort) -> bool {
    reasoning_effort != model::ReasoningEffort::Low
}

/// Provider-independent fields extracted from the first completion choice.
struct ChatCompletion {
    content: Option<String>,
    metadata: model::CompletionMetadata,
    reasoning_content: Option<String>,
    tool_calls: Vec<ChatCompletionToolCall>,
}

impl ChatCompletion {
    /// Consumes the choice into provider-interpreted completion fields.
    fn into_parts(
        self,
    ) -> (
        model::CompletionMetadata,
        Option<String>,
        Option<String>,
        Vec<ChatCompletionToolCall>,
    ) {
        (
            self.metadata,
            self.content,
            self.reasoning_content,
            self.tool_calls,
        )
    }
}

/// Incremental reader of a Chat Completions chunk stream that accumulates
/// the first choice into the completion an unstreamed response carries,
/// and, when `observed`, keeps the content deltas not yet reported. The
/// accumulated text stays within the success body limit, so memory is
/// bounded however long the stream runs.
#[derive(Default)]
struct ChunkStream {
    /// Position among the response's calls of each call, by its wire index.
    call_indexes: HashMap<u64, usize>,
    calls: Vec<StreamedToolCall>,
    content: Option<String>,
    /// Bytes of accumulated text: content, reasoning, and calls.
    content_bytes: usize,
    /// Deltas read but not yet reported.
    deltas: Vec<ResponseDelta>,
    events: SseDecoder,
    finish_reason: Option<String>,
    id: Option<String>,
    model: Option<String>,
    observed: bool,
    reasoning_content: Option<String>,
    system_fingerprint: Option<String>,
    usage: Option<ChatCompletionUsage>,
}

impl ChunkStream {
    /// Creates a reader that keeps content deltas when `observed`.
    fn new(observed: bool) -> Self {
        Self {
            observed,
            ..Self::default()
        }
    }

    /// Reads `chunk`, returning whether the stream's `[DONE]` event
    /// arrived; later events are not read.
    fn push(&mut self, chunk: &[u8]) -> Result<bool, TransportError> {
        for data in self.events.push(chunk)? {
            if self.dispatch(&data)? {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Reads the event left when the stream closed and returns the
    /// completion, failing when no choice finished.
    fn finish(mut self) -> Result<ChatCompletion, TransportError> {
        if let Some(data) = std::mem::take(&mut self.events).finish()? {
            self.dispatch(&data)?;
        }
        let finish_reason = self
            .finish_reason
            .ok_or_else(|| TransportError::transport(StreamClosed))?;
        let metadata = model::CompletionMetadata::new(
            finish_reason,
            self.id,
            self.model,
            self.system_fingerprint,
            self.usage.map(model::CompletionUsage::from),
        );

        Ok(ChatCompletion {
            content: self.content,
            metadata,
            reasoning_content: self.reasoning_content,
            tool_calls: self
                .calls
                .into_iter()
                .map(StreamedToolCall::into_tool_call)
                .collect(),
        })
    }

    /// Reads one event, returning whether it is `[DONE]`.
    fn dispatch(&mut self, data: &str) -> Result<bool, TransportError> {
        if data == "[DONE]" {
            return Ok(true);
        }
        let chunk = serde_json::from_str::<ChatCompletionChunk>(data)
            .map_err(TransportError::InvalidResponse)?;
        if let Some(error) = chunk.error {
            return Err(TransportError::StreamFailed {
                code: error.code,
                message: error.message,
            });
        }
        self.id = self.id.take().or(chunk.id);
        self.model = self.model.take().or(chunk.model);
        self.system_fingerprint = self.system_fingerprint.take().or(chunk.system_fingerprint);
        if chunk.usage.is_some() {
            self.usage = chunk.usage;
        }
        let first = chunk
            .choices
            .into_iter()
            .flatten()
            .find(|choice| choice.index == 0);
        let Some(choice) = first else {
            return Ok(false);
        };
        // Some providers, such as Moonshot, report streamed usage on the
        // choice instead.
        if self.usage.is_none() {
            self.usage = choice.usage;
        }
        if choice.finish_reason.is_some() {
            self.finish_reason = choice.finish_reason;
        }
        let Some(delta) = choice.delta else {
            return Ok(false);
        };
        if let Some(text) = delta.content.filter(|text| !text.is_empty()) {
            self.grow(text.len())?;
            self.content.get_or_insert_default().push_str(&text);
            self.report(ResponseDelta::Text(text));
        }
        if let Some(text) = delta.reasoning_content.filter(|text| !text.is_empty()) {
            self.grow(text.len())?;
            self.reasoning_content
                .get_or_insert_default()
                .push_str(&text);
            self.report(ResponseDelta::Reasoning(text));
        }
        for call in delta.tool_calls.into_iter().flatten() {
            self.read_call(call)?;
        }

        Ok(false)
    }

    /// Accumulates one tool-call fragment into the call at its wire index,
    /// numbering calls by the order they start.
    fn read_call(&mut self, fragment: ChunkToolCall) -> Result<(), TransportError> {
        let next = self.call_indexes.len();
        let index = *self.call_indexes.entry(fragment.index).or_insert(next);
        if index == self.calls.len() {
            self.calls.push(StreamedToolCall::default());
        }
        let function = fragment.function.unwrap_or_default();
        let grown = [
            &fragment.id,
            &fragment.kind,
            &function.name,
            &function.arguments,
        ]
        .into_iter()
        .flatten()
        .map(String::len)
        .sum();
        self.grow(grown)?;
        let call = &mut self.calls[index];
        call.id = call.id.take().or(fragment.id);
        call.kind = call.kind.take().or(fragment.kind);
        call.name = call.name.take().or(function.name);
        if let Some(arguments) = function.arguments.filter(|arguments| !arguments.is_empty()) {
            call.arguments.push_str(&arguments);
            self.report(ResponseDelta::ToolArguments { arguments, index });
        }

        Ok(())
    }

    /// Counts `bytes` more accumulated text, failing past the success body
    /// limit.
    fn grow(&mut self, bytes: usize) -> Result<(), TransportError> {
        self.content_bytes = self.content_bytes.saturating_add(bytes);
        if self.content_bytes > SUCCESS_BODY_LIMIT_BYTES {
            return Err(TransportError::ResponseBodyTooLarge);
        }

        Ok(())
    }

    /// Keeps `delta` for the observer, when there is one.
    fn report(&mut self, delta: ResponseDelta) {
        if self.observed {
            self.deltas.push(delta);
        }
    }
}

/// One tool call accumulated from its streamed fragments.
#[derive(Default)]
struct StreamedToolCall {
    arguments: String,
    id: Option<String>,
    kind: Option<String>,
    name: Option<String>,
}

impl StreamedToolCall {
    /// Returns the call as an unstreamed response carries it; a call that
    /// never named its type reads as a function call.
    fn into_tool_call(self) -> ChatCompletionToolCall {
        let mut function = serde_json::Map::new();
        function.insert("arguments".to_string(), Value::String(self.arguments));
        if let Some(name) = self.name {
            function.insert("name".to_string(), Value::String(name));
        }

        ChatCompletionToolCall {
            function: Value::Object(function),
            id: self.id.unwrap_or_default(),
            kind: self.kind.unwrap_or_else(|| "function".to_string()),
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionPayload<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    enable_thinking: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_completion_tokens: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u32>,
    messages: Vec<ChatCompletionMessagePayload>,
    model: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompt_cache_key: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    response_format: Option<ResponseFormat<'a>>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatCompletionTool<'a>>,
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
}

#[derive(Serialize)]
struct Thinking {
    #[serde(skip_serializing_if = "Option::is_none")]
    keep: Option<&'static str>,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatCompletionMessagePayload {
    Text {
        content: String,
        role: &'static str,
    },
    Parts {
        content: Vec<ChatCompletionContentPart>,
        role: &'static str,
    },
    AssistantReasoning {
        content: String,
        reasoning_content: String,
        role: &'static str,
    },
    AssistantToolCall {
        content: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        reasoning_content: Option<String>,
        role: &'static str,
        tool_calls: Vec<ChatCompletionOutgoingToolCall>,
    },
    ToolResult {
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        name: Option<String>,
        role: &'static str,
        tool_call_id: String,
    },
}

#[derive(Serialize)]
#[serde(untagged)]
enum ChatCompletionContentPart {
    Text {
        #[serde(rename = "type")]
        kind: &'static str,
        text: String,
    },
    ImageUrl {
        #[serde(rename = "type")]
        kind: &'static str,
        image_url: ChatCompletionImageUrl,
    },
}

#[derive(Serialize)]
struct ChatCompletionImageUrl {
    url: String,
}

#[derive(Serialize)]
struct ChatCompletionOutgoingToolCall {
    function: ChatCompletionOutgoingFunctionCall,
    id: String,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct ChatCompletionOutgoingFunctionCall {
    arguments: String,
    name: String,
}

#[derive(Serialize)]
struct ResponseFormat<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    json_schema: Option<JsonSchemaResponseFormat<'a>>,
    #[serde(rename = "type")]
    kind: &'static str,
}

#[derive(Serialize)]
struct JsonSchemaResponseFormat<'a> {
    name: &'a str,
    schema: &'a Value,
}

#[derive(Serialize)]
struct ChatCompletionTool<'a> {
    function: ChatCompletionFunction<'a>,
    #[serde(rename = "type")]
    kind: &'static str,
}

impl<'a> From<&'a tool::ToolDefinition> for ChatCompletionTool<'a> {
    fn from(definition: &'a tool::ToolDefinition) -> Self {
        Self {
            function: ChatCompletionFunction {
                description: definition.description(),
                name: definition.name(),
                parameters: definition.parameters(),
            },
            kind: "function",
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionFunction<'a> {
    description: &'a str,
    name: &'a str,
    parameters: &'a Value,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<ChatCompletionChoice>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    system_fingerprint: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<ChatCompletionUsage>,
}

impl ChatCompletionResponse {
    fn into_completion(self) -> Option<ChatCompletion> {
        let Self {
            choices,
            id,
            model: response_model,
            system_fingerprint,
            usage,
        } = self;
        let choice = choices.into_iter().next()?;
        let metadata = model::CompletionMetadata::new(
            choice.finish_reason,
            id,
            response_model,
            system_fingerprint,
            usage.map(model::CompletionUsage::from),
        );

        Some(ChatCompletion {
            content: choice.message.content,
            metadata,
            reasoning_content: choice.message.reasoning_content,
            tool_calls: choice.message.tool_calls.unwrap_or_default(),
        })
    }
}

/// One chunk of a streamed completion.
#[derive(Deserialize)]
struct ChatCompletionChunk {
    #[serde(default, deserialize_with = "deserialize_optional")]
    choices: Option<Vec<ChunkChoice>>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    error: Option<ChunkError>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    id: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    model: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    system_fingerprint: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<ChatCompletionUsage>,
}

/// A failure a provider reported inside its stream.
#[derive(Deserialize)]
struct ChunkError {
    #[serde(default, deserialize_with = "deserialize_optional")]
    code: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    message: Option<String>,
}

#[derive(Deserialize)]
struct ChunkChoice {
    #[serde(default, deserialize_with = "deserialize_optional")]
    delta: Option<ChunkDelta>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    finish_reason: Option<String>,
    #[serde(default)]
    index: u64,
    #[serde(default, deserialize_with = "deserialize_optional")]
    usage: Option<ChatCompletionUsage>,
}

#[derive(Deserialize)]
struct ChunkDelta {
    #[serde(default, deserialize_with = "deserialize_optional")]
    content: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    reasoning_content: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    tool_calls: Option<Vec<ChunkToolCall>>,
}

#[derive(Deserialize)]
struct ChunkToolCall {
    #[serde(default, deserialize_with = "deserialize_optional")]
    function: Option<ChunkFunction>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    id: Option<String>,
    #[serde(default)]
    index: u64,
    #[serde(default, deserialize_with = "deserialize_optional", rename = "type")]
    kind: Option<String>,
}

#[derive(Default, Deserialize)]
struct ChunkFunction {
    #[serde(default, deserialize_with = "deserialize_optional")]
    arguments: Option<String>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    name: Option<String>,
}

#[derive(Deserialize)]
struct ChatCompletionChoice {
    finish_reason: String,
    message: ChatCompletionMessage,
}

#[derive(Deserialize)]
struct ChatCompletionMessage {
    content: Option<String>,
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    tool_calls: Option<Vec<ChatCompletionToolCall>>,
}

#[derive(Deserialize)]
struct ChatCompletionUsage {
    #[serde(default, deserialize_with = "deserialize_optional")]
    completion_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    completion_tokens_details: Option<CompletionTokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_cache_hit_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_cache_miss_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_tokens: Option<u64>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    prompt_tokens_details: Option<PromptTokenDetails>,
    #[serde(default, deserialize_with = "deserialize_optional")]
    total_tokens: Option<u64>,
}

impl From<ChatCompletionUsage> for model::CompletionUsage {
    fn from(usage: ChatCompletionUsage) -> Self {
        Self::new(
            usage.prompt_cache_hit_tokens.or_else(|| {
                usage
                    .prompt_tokens_details
                    .as_ref()
                    .and_then(|details| details.cached_tokens)
            }),
            usage.prompt_cache_miss_tokens,
            usage.prompt_tokens,
            usage.completion_tokens,
            usage
                .completion_tokens_details
                .and_then(|details| details.reasoning_tokens),
            usage.total_tokens,
        )
    }
}

#[derive(Deserialize)]
struct CompletionTokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    reasoning_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct PromptTokenDetails {
    #[serde(default, deserialize_with = "deserialize_optional")]
    cached_tokens: Option<u64>,
}

#[derive(Deserialize)]
struct ChatCompletionToolCall {
    #[serde(default)]
    function: Value,
    id: String,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Deserialize)]
struct ChatCompletionFunctionCall {
    arguments: String,
    name: String,
}

#[cfg(test)]
#[path = "chat_completion_test.rs"]
mod tests;
