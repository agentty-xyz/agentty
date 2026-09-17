use std::future::Future;
use std::io::{self, Write};
use std::time::Duration;

use serde_json::Value;

use super::error::CodexClientError;
use crate::model::CompletionUsage;
use crate::schema_contract;

const JSON_STRING_MAX_EXPANSION: usize = 6;
const OPENAI_MODEL_HEADER: &str = "openai-model";
const RESPONSE_EVENT_ENVELOPE_LIMIT_BYTES: usize = 64 * 1024;
const RESPONSE_EVENT_LIMIT_BYTES: usize = schema_contract::RESPONSE_CONTENT_LIMIT_BYTES
    * JSON_STRING_MAX_EXPANSION
    + RESPONSE_EVENT_ENVELOPE_LIMIT_BYTES;
pub(super) const RESPONSE_WIRE_LIMIT_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone)]
pub(super) struct CodexResponsesCompletion {
    pub(super) account_fingerprint: Option<String>,
    pub(super) output: String,
    pub(super) reasoning_content: Option<String>,
    pub(super) response_id: Option<String>,
    pub(super) response_model: Option<String>,
    pub(super) status: String,
    pub(super) usage: Option<CompletionUsage>,
}

pub(super) async fn read_response_body(
    response: &mut reqwest::Response,
    limit: usize,
    stream_idle_timeout: Duration,
) -> Result<String, CodexClientError> {
    let mut bytes = Vec::new();
    while let Some(chunk) = with_stream_idle_timeout(response.chunk(), stream_idle_timeout).await? {
        if bytes.len().saturating_add(chunk.len()) > limit {
            return Err(CodexClientError::ResponseTooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }

    String::from_utf8(bytes).map_err(|error| CodexClientError::InvalidSse {
        reason: schema_contract::bounded_diagnostic(error),
    })
}

pub(super) async fn read_sse_response(
    response: &mut reqwest::Response,
    stream_idle_timeout: Duration,
) -> Result<CodexResponsesCompletion, CodexClientError> {
    let mut decoder = CodexSseDecoder::new(response_model_header(response.headers()));
    loop {
        let Some(chunk) = with_stream_idle_timeout(response.chunk(), stream_idle_timeout).await?
        else {
            break decoder.finish();
        };
        if let Some(completion) = decoder.push(&chunk)? {
            break Ok(completion);
        }
    }
}

pub(super) async fn with_stream_idle_timeout<Output>(
    operation: impl Future<Output = Result<Output, reqwest::Error>>,
    stream_idle_timeout: Duration,
) -> Result<Output, CodexClientError> {
    tokio::time::timeout(stream_idle_timeout, operation)
        .await
        .map_err(|_| CodexClientError::StreamIdleTimeout)?
        .map_err(CodexClientError::Transport)
}

pub(super) async fn with_response_timeout<Output>(
    operation: impl Future<Output = Result<Output, CodexClientError>>,
    response_timeout: Duration,
) -> Result<Output, CodexClientError> {
    tokio::time::timeout(response_timeout, operation)
        .await
        .map_err(|_| CodexClientError::ResponseTimeout)?
}

pub(super) struct CodexSseDecoder {
    bytes_received: usize,
    content_limit: usize,
    event_limit: usize,
    output: String,
    pending: Vec<u8>,
    replay_bytes: usize,
    replay_items: Vec<Value>,
    response_model: Option<String>,
    wire_limit: usize,
}

impl CodexSseDecoder {
    pub(super) fn new(response_model: Option<String>) -> Self {
        let mut decoder = Self::with_limits(
            RESPONSE_WIRE_LIMIT_BYTES,
            RESPONSE_EVENT_LIMIT_BYTES,
            schema_contract::RESPONSE_CONTENT_LIMIT_BYTES,
        );
        decoder.response_model = response_model;

        decoder
    }

    fn with_limits(wire_limit: usize, event_limit: usize, content_limit: usize) -> Self {
        Self {
            bytes_received: 0,
            content_limit,
            event_limit,
            output: String::new(),
            pending: Vec::new(),
            replay_bytes: 2,
            replay_items: Vec::new(),
            response_model: None,
            wire_limit,
        }
    }

    pub(super) fn push(
        &mut self,
        chunk: &[u8],
    ) -> Result<Option<CodexResponsesCompletion>, CodexClientError> {
        self.bytes_received = self.bytes_received.saturating_add(chunk.len());
        if self.bytes_received > self.wire_limit {
            return Err(CodexClientError::ResponseTooLarge);
        }
        self.pending.extend_from_slice(chunk);
        let mut consumed = 0;
        while let Some(relative_newline) = self.pending[consumed..]
            .iter()
            .position(|byte| *byte == b'\n')
        {
            let newline = consumed + relative_newline;
            let mut line = &self.pending[consumed..newline];
            if line.last() == Some(&b'\r') {
                line = &line[..line.len() - 1];
            }
            if line.len() > self.event_limit {
                return Err(CodexClientError::ResponseTooLarge);
            }
            let event = Self::parse_line(line)?;
            consumed = newline + 1;
            if let Some(event) = event
                && let Some(completion) = self.consume_event(&event)?
            {
                return Ok(Some(completion));
            }
        }
        if consumed > 0 {
            self.pending.copy_within(consumed.., 0);
            self.pending.truncate(self.pending.len() - consumed);
        }
        if self.pending.len() > self.event_limit {
            return Err(CodexClientError::ResponseTooLarge);
        }

        Ok(None)
    }

    pub(super) fn finish(mut self) -> Result<CodexResponsesCompletion, CodexClientError> {
        if !self.pending.is_empty() {
            let pending = std::mem::take(&mut self.pending);
            if let Some(event) = Self::parse_line(&pending)?
                && let Some(completion) = self.consume_event(&event)?
            {
                return Ok(completion);
            }
        }

        Err(CodexClientError::MissingResponseField(
            "response.completed event",
        ))
    }

    fn parse_line(line: &[u8]) -> Result<Option<Value>, CodexClientError> {
        let line = std::str::from_utf8(line).map_err(|error| CodexClientError::InvalidSse {
            reason: schema_contract::bounded_diagnostic(error),
        })?;
        let Some(data) = line.strip_prefix("data:").map(str::trim) else {
            return Ok(None);
        };
        if data == "[DONE]" {
            return Ok(None);
        }

        serde_json::from_str(data)
            .map(Some)
            .map_err(|error| CodexClientError::InvalidSse {
                reason: schema_contract::bounded_diagnostic(error),
            })
    }

    fn consume_event(
        &mut self,
        event: &Value,
    ) -> Result<Option<CodexResponsesCompletion>, CodexClientError> {
        if let Some(response_model) = event_response_model(event) {
            self.response_model = Some(response_model);
        }
        match event.get("type").and_then(Value::as_str) {
            Some("response.output_text.delta") => {
                let delta = event
                    .get("delta")
                    .and_then(Value::as_str)
                    .ok_or(CodexClientError::MissingResponseField("delta"))?;
                if delta.len() > self.content_limit.saturating_sub(self.output.len()) {
                    return Err(CodexClientError::ResponseContentTooLarge);
                }
                self.output.push_str(delta);
            }
            Some("response.output_item.done") => {
                if let Some(item) = event.get("item")
                    && is_replay_item(item)
                {
                    push_replay_item(
                        &mut self.replay_items,
                        &mut self.replay_bytes,
                        item,
                        self.content_limit,
                    )?;
                }
            }
            Some("response.completed") => {
                let response =
                    event
                        .get("response")
                        .ok_or(CodexClientError::MissingResponseField(
                            "response.completed event",
                        ))?;

                return completed_response(
                    std::mem::take(&mut self.output),
                    std::mem::take(&mut self.replay_items),
                    response,
                    self.content_limit,
                    self.response_model.take(),
                )
                .map(Some);
            }
            Some("response.failed" | "error") => {
                return Err(CodexClientError::Provider {
                    message: extract_event_error(event),
                });
            }
            Some("response.incomplete") => {
                return Err(CodexClientError::Incomplete {
                    reason: event
                        .pointer("/response/incomplete_details/reason")
                        .and_then(Value::as_str)
                        .map_or_else(
                            || "Codex response was incomplete".to_string(),
                            schema_contract::bounded_diagnostic,
                        ),
                });
            }
            _ => {}
        }

        Ok(None)
    }
}

fn completed_response(
    mut output: String,
    mut replay_items: Vec<Value>,
    response: &Value,
    content_limit: usize,
    response_model: Option<String>,
) -> Result<CodexResponsesCompletion, CodexClientError> {
    validate_completed_response(response)?;
    if let Some(completed_output) =
        extract_response_output(response).or_else(|| extract_output_from_items(&replay_items))
    {
        output = completed_output;
    } else if response.get("output").is_some() || output.trim().is_empty() {
        return Err(CodexClientError::MissingResponseField(
            "structured output text",
        ));
    }
    if output.len() > content_limit {
        return Err(CodexClientError::ResponseContentTooLarge);
    }
    if let Some(completed_replay) = extract_response_replay(response, content_limit)? {
        replay_items = completed_replay;
    }
    let has_reasoning = replay_items
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"));
    let reasoning_content = if has_reasoning {
        if !replay_items.iter().any(is_final_message) {
            let mut replay_bytes = replay_items_size(&replay_items);
            push_replay_item(
                &mut replay_items,
                &mut replay_bytes,
                &response_message(&output),
                content_limit,
            )?;
        }
        let reasoning_content = serialize_replay_items(&replay_items);

        Some(reasoning_content)
    } else {
        None
    };

    Ok(CodexResponsesCompletion {
        account_fingerprint: None,
        output,
        reasoning_content,
        response_id: string_field(response, "id"),
        response_model: response_model.or_else(|| bounded_string_field(response, "model")),
        status: string_field(response, "status").unwrap_or_else(|| "completed".to_string()),
        usage: response.get("usage").map(completion_usage),
    })
}

fn extract_response_replay(
    response: &Value,
    content_limit: usize,
) -> Result<Option<Vec<Value>>, CodexClientError> {
    let Some(output) = response.get("output").and_then(Value::as_array) else {
        return Ok(None);
    };
    if !output
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("reasoning"))
    {
        return Ok(None);
    }
    let mut replay_items = Vec::new();
    let mut replay_bytes = 2;
    for item in output.iter().filter(|item| is_replay_item(item)) {
        push_replay_item(&mut replay_items, &mut replay_bytes, item, content_limit)?;
    }

    Ok(Some(replay_items))
}

fn is_replay_item(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("reasoning")
        || item.get("type").and_then(Value::as_str) == Some("message")
            && item.get("role").and_then(Value::as_str) == Some("assistant")
}

fn is_final_message(item: &Value) -> bool {
    item.get("type").and_then(Value::as_str) == Some("message")
        && matches!(
            item.get("phase").and_then(Value::as_str),
            None | Some("final_answer")
        )
}

fn response_message(output: &str) -> Value {
    serde_json::json!({
        "type": "message",
        "role": "assistant",
        "content": [{ "type": "output_text", "text": output }]
    })
}

#[cfg(test)]
pub(super) fn parse_sse_response(body: &str) -> Result<CodexResponsesCompletion, CodexClientError> {
    let mut decoder = CodexSseDecoder::new(None);
    if let Some(completion) = decoder.push(body.as_bytes())? {
        return Ok(completion);
    }

    decoder.finish()
}

fn extract_event_error(event: &Value) -> String {
    event
        .pointer("/response/error/message")
        .or_else(|| event.pointer("/error/message"))
        .or_else(|| event.get("message"))
        .and_then(Value::as_str)
        .map_or_else(
            || "Codex response failed".to_string(),
            schema_contract::bounded_diagnostic,
        )
}

fn extract_response_output(response: &Value) -> Option<String> {
    extract_output_from_items(response.get("output")?.as_array()?)
}

fn extract_output_from_items(items: &[Value]) -> Option<String> {
    items
        .iter()
        .rev()
        .filter(|item| is_final_message(item))
        .find_map(|item| {
            let output = item
                .get("content")
                .and_then(Value::as_array)?
                .iter()
                .filter(|content| {
                    content.get("type").and_then(Value::as_str) == Some("output_text")
                })
                .filter_map(|content| content.get("text").and_then(Value::as_str))
                .collect::<String>();

            (!output.trim().is_empty()).then_some(output)
        })
}

fn validate_completed_response(response: &Value) -> Result<(), CodexClientError> {
    if response.get("error").is_some_and(|error| !error.is_null())
        || matches!(
            response.get("status").and_then(Value::as_str),
            Some("failed" | "cancelled")
        )
    {
        return Err(CodexClientError::Provider {
            message: response
                .pointer("/error/message")
                .and_then(Value::as_str)
                .map_or_else(
                    || "Codex response failed".to_string(),
                    schema_contract::bounded_diagnostic,
                ),
        });
    }
    if response.get("status").and_then(Value::as_str) == Some("incomplete") {
        return Err(CodexClientError::Incomplete {
            reason: response
                .pointer("/incomplete_details/reason")
                .and_then(Value::as_str)
                .map_or_else(
                    || "Codex response was incomplete".to_string(),
                    schema_contract::bounded_diagnostic,
                ),
        });
    }

    Ok(())
}

fn push_replay_item(
    replay_items: &mut Vec<Value>,
    replay_bytes: &mut usize,
    item: &Value,
    content_limit: usize,
) -> Result<(), CodexClientError> {
    let item_bytes = serialized_size(item);
    let separator = usize::from(!replay_items.is_empty());
    let next_size = replay_bytes
        .saturating_add(separator)
        .saturating_add(item_bytes);
    if next_size > content_limit {
        return Err(CodexClientError::ResponseContentTooLarge);
    }
    *replay_bytes = next_size;
    replay_items.push(item.clone());

    Ok(())
}

fn replay_items_size(items: &[Value]) -> usize {
    let mut size = 2_usize;
    for (index, item) in items.iter().enumerate() {
        size = size
            .saturating_add(usize::from(index > 0))
            .saturating_add(serialized_size(item));
    }

    size
}

#[allow(
    clippy::expect_used,
    reason = "serde_json::Value and the byte counter serialize infallibly"
)]
fn serialized_size(value: &Value) -> usize {
    let mut counter = ByteCounter::default();
    serde_json::to_writer(&mut counter, value).expect("JSON values should serialize");

    counter.0
}

#[allow(
    clippy::expect_used,
    reason = "SSE replay contains only serde_json::Value"
)]
fn serialize_replay_items(items: &[Value]) -> String {
    serde_json::to_string(items).expect("SSE replay values should serialize")
}

#[derive(Default)]
struct ByteCounter(usize);

impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self.0.saturating_add(bytes.len());

        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn response_model_header(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get(OPENAI_MODEL_HEADER)
        .and_then(|value| value.to_str().ok())
        .and_then(bounded_model)
}

fn event_response_model(event: &Value) -> Option<String> {
    event
        .pointer("/response/headers")
        .and_then(json_response_model_header)
        .or_else(|| event.get("headers").and_then(json_response_model_header))
}

fn json_response_model_header(headers: &Value) -> Option<String> {
    headers.as_object()?.iter().find_map(|(name, value)| {
        (name.eq_ignore_ascii_case(OPENAI_MODEL_HEADER)
            || name.eq_ignore_ascii_case("x-openai-model"))
        .then(|| value.as_str().and_then(bounded_model))
        .flatten()
    })
}

fn bounded_string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .and_then(bounded_model)
}

fn bounded_model(model: &str) -> Option<String> {
    let model = model.trim();

    (!model.is_empty()).then(|| schema_contract::bounded_diagnostic(model))
}

fn string_field(value: &Value, field: &str) -> Option<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

pub(super) fn completion_usage(usage: &Value) -> CompletionUsage {
    let input = usage.get("input_tokens").and_then(Value::as_u64);
    let output = usage.get("output_tokens").and_then(Value::as_u64);
    let cache_hit = usage
        .pointer("/input_tokens_details/cached_tokens")
        .and_then(Value::as_u64);
    let reasoning = usage
        .pointer("/output_tokens_details/reasoning_tokens")
        .and_then(Value::as_u64);
    let total = usage
        .get("total_tokens")
        .and_then(Value::as_u64)
        .or_else(|| {
            input
                .zip(output)
                .map(|(input, output)| input.saturating_add(output))
        });

    CompletionUsage::new(
        cache_hit,
        input
            .zip(cache_hit)
            .map(|(input, cache_hit)| input.saturating_sub(cache_hit)),
        input,
        output,
        reasoning,
        total,
    )
}

#[cfg(test)]
#[path = "sse_test.rs"]
mod tests;
