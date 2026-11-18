//! Anthropic Messages API backend.
//!
//! Requests are unstreamed `POST {base_url}/messages` calls carrying the
//! whole history. Each system message joins the top-level `system` blocks,
//! consecutive messages of one role merge into one message, and turn
//! instructions become a user text block under the shared instructions
//! heading.
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
use std::time::Duration;

use reqwest::header::HeaderMap;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::chat_completion::TURN_INSTRUCTIONS_HEADING;
use crate::http::{HttpTransport, ProviderRequest, deserialize_optional};
use crate::model::{GeneratedResponse, ModelErrorType};
use crate::provider_spec::{
    DEFAULT_FALLBACK_MAX_TOKENS, ProviderSpec, ReasoningMapping, StructuredOutputMode,
};
use crate::reasoning::ReasoningView;
use crate::{input, model, schema as schema_contract, tool};

/// Path of the Messages endpoint under a provider base URL.
pub(crate) const ENDPOINT_PATH: &str = "messages";
/// Name of the tool whose input is the answer under
/// [`StructuredOutputMode::ForcedTool`].
pub(crate) const OUTPUT_TOOL_NAME: &str = "structured_output";
/// Description of the [`OUTPUT_TOOL_NAME`] tool.
const OUTPUT_TOOL_DESCRIPTION: &str =
    "Give your final answer as this tool's input once you need no other tool.";
/// Bound for one unstreamed response, matching the Anthropic SDKs' default
/// request timeout.
const REQUEST_TIMEOUT: Duration = Duration::from_mins(10);
/// Content block types a reasoning-content array may hold.
const REPLAYED_BLOCK_TYPES: [&str; 4] = ["redacted_thinking", "text", "thinking", "tool_use"];
/// Signed reasoning block types, replayed verbatim.
const THINKING_BLOCK_TYPES: [&str; 2] = ["redacted_thinking", "thinking"];

/// Structured-output backend for the Anthropic Messages API, applying the
/// request policy of one [`ProviderSpec`].
pub(crate) struct AnthropicBackend<'spec> {
    headers: HeaderMap,
    http: Arc<HttpTransport>,
    model: String,
    policy: &'spec ProviderSpec,
}

impl<'spec> AnthropicBackend<'spec> {
    /// Creates a backend sending `headers` through `http`.
    pub(crate) fn new(
        policy: &'spec ProviderSpec,
        model: String,
        headers: HeaderMap,
        http: Arc<HttpTransport>,
    ) -> Self {
        Self {
            headers,
            http,
            model,
            policy,
        }
    }

    /// Generates raw structured output, or plain text for a request without
    /// a response format, from one response.
    pub(crate) async fn generate(
        &self,
        request: &model::ModelRequest,
    ) -> Result<GeneratedResponse, model::ModelError> {
        for message in request.messages() {
            if let model::ModelMessage::UserInput(input) = message {
                self.policy.validate_input(&self.model, input)?;
            }
        }
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
                description: OUTPUT_TOOL_DESCRIPTION,
                input_schema: format.schema().value(),
                name: OUTPUT_TOOL_NAME,
            });

            choice
        });
        let (system, messages) = Self::messages(request);
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
        );
        let response = self
            .http
            .post_json::<MessagesResponse>(&request_body)
            .await
            .map_err(|error| {
                error.into_model_error(&self.policy.display_name, &self.policy.overflow_markers)
            })?;

        Ok(self.decode(&response, output_tool.is_some()))
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

/// Creates the production transport for Messages requests.
pub(crate) fn default_transport() -> Arc<HttpTransport> {
    Arc::new(HttpTransport::new(REQUEST_TIMEOUT))
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
    description: &'a str,
    input_schema: &'a Value,
    name: &'a str,
}

impl<'a> From<&'a tool::ToolDefinition> for ToolPayload<'a> {
    fn from(definition: &'a tool::ToolDefinition) -> Self {
        Self {
            description: definition.description(),
            input_schema: definition.parameters(),
            name: definition.name(),
        }
    }
}

#[derive(Deserialize)]
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
#[derive(Deserialize)]
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
