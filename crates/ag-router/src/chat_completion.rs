//! Shared structured-output backend for `OpenAI`-compatible Chat
//! Completions APIs.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::http::{HttpTransport, ProviderRequest, TransportError, deserialize_optional};
use crate::model::GeneratedResponse;
use crate::provider_spec::{
    InstructionPlacement, MaxTokensField, ProviderSpec, ReasoningMapping, StructuredOutputMode,
    ThinkingOptions,
};
use crate::{input, model, schema as schema_contract, tool};

/// Path of the Chat Completions endpoint under a provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "chat/completions";
/// Bound for one Chat Completions request. Reasoning models can spend more
/// than a minute on a single completion, so a timed-out request is reported
/// without a retry instead of doubling the wait.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(3);
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
    client: Arc<dyn ChatCompletionClient>,
    headers: HeaderMap,
    model: String,
    policy: &'spec ProviderSpec,
}

impl<'spec> ChatCompletionBackend<'spec> {
    /// Creates a backend sending `headers` through an injected transport
    /// client.
    pub(crate) fn new(
        policy: &'spec ProviderSpec,
        model: String,
        headers: HeaderMap,
        client: Arc<dyn ChatCompletionClient>,
    ) -> Self {
        Self {
            client,
            headers,
            model,
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
            thinking: self.thinking(request),
            tools,
        };
        let payload = serde_json::to_value(payload).map_err(model::ModelError::request)?;
        let completion = self
            .client
            .complete(ProviderRequest::new(
                self.headers.clone(),
                self.policy.endpoint(ENDPOINT_PATH),
                payload,
            ))
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
                StructuredOutputMode::JsonObject | StructuredOutputMode::JsonSchema => {
                    STRUCTURED_OUTPUT_INSTRUCTION
                }
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
            StructuredOutputMode::JsonSchema | StructuredOutputMode::SchemaPrompt => {
                ResponseFormat {
                    json_schema: Some(JsonSchemaResponseFormat {
                        name: format.name(),
                        schema: format.schema().value(),
                    }),
                    kind: "json_schema",
                }
            }
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
pub(crate) struct ChatCompletion {
    content: Option<String>,
    metadata: model::CompletionMetadata,
    reasoning_content: Option<String>,
    tool_calls: Vec<ChatCompletionToolCall>,
}

impl ChatCompletion {
    /// Creates one normalized completion choice.
    pub(crate) fn new(finish_reason: String, content: Option<String>) -> Self {
        Self {
            content,
            metadata: model::CompletionMetadata::new(finish_reason, None, None, None, None),
            reasoning_content: None,
            tool_calls: Vec::new(),
        }
    }

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

/// Decoded client boundary between provider adapters and the Chat Completions
/// API.
#[async_trait]
pub(crate) trait ChatCompletionClient: Send + Sync {
    async fn complete(
        &self,
        request: ProviderRequest,
    ) -> Result<Option<ChatCompletion>, TransportError>;
}

/// Creates the production Chat Completions client implementation.
pub(crate) fn default_client() -> Arc<dyn ChatCompletionClient> {
    Arc::new(ReqwestChatCompletionClient {
        http: HttpTransport::new(REQUEST_TIMEOUT),
    })
}

struct ReqwestChatCompletionClient {
    http: HttpTransport,
}

#[async_trait]
impl ChatCompletionClient for ReqwestChatCompletionClient {
    async fn complete(
        &self,
        request: ProviderRequest,
    ) -> Result<Option<ChatCompletion>, TransportError> {
        let response = self
            .http
            .post_json::<ChatCompletionResponse>(&request)
            .await?;

        Ok(response.into_completion().map(|(choice, metadata)| {
            let mut completion = ChatCompletion::new(choice.finish_reason, choice.message.content);
            completion.metadata = metadata;
            completion.reasoning_content = choice.message.reasoning_content;
            completion.tool_calls = choice.message.tool_calls.unwrap_or_default();

            completion
        }))
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
    #[serde(skip_serializing_if = "Option::is_none")]
    thinking: Option<Thinking>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ChatCompletionTool<'a>>,
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
    fn into_completion(self) -> Option<(ChatCompletionChoice, model::CompletionMetadata)> {
        let Self {
            choices,
            id,
            model: response_model,
            system_fingerprint,
            usage,
        } = self;
        let choice = choices.into_iter().next()?;
        let metadata = model::CompletionMetadata::new(
            choice.finish_reason.clone(),
            id,
            response_model,
            system_fingerprint,
            usage.map(model::CompletionUsage::from),
        );

        Some((choice, metadata))
    }
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
