use std::future::Future;
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
    pub(super) output: String,
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

pub(super) struct CodexSseDecoder {
    bytes_received: usize,
    content_limit: usize,
    event_limit: usize,
    output: String,
    pending: Vec<u8>,
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
            Some("response.completed") => {
                let response =
                    event
                        .get("response")
                        .ok_or(CodexClientError::MissingResponseField(
                            "response.completed event",
                        ))?;

                return completed_response(
                    std::mem::take(&mut self.output),
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
    response: &Value,
    content_limit: usize,
    response_model: Option<String>,
) -> Result<CodexResponsesCompletion, CodexClientError> {
    if let Some(completed_output) = extract_response_output(response) {
        output = completed_output;
    } else if response.get("output").is_some() || output.trim().is_empty() {
        return Err(CodexClientError::MissingResponseField(
            "structured output text",
        ));
    }
    if output.len() > content_limit {
        return Err(CodexClientError::ResponseContentTooLarge);
    }

    Ok(CodexResponsesCompletion {
        output,
        response_id: string_field(response, "id"),
        response_model: response_model.or_else(|| bounded_string_field(response, "model")),
        status: string_field(response, "status").unwrap_or_else(|| "completed".to_string()),
        usage: response.get("usage").map(completion_usage),
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
    response
        .get("output")?
        .as_array()?
        .iter()
        .rev()
        .filter(|item| {
            item.get("type").and_then(Value::as_str) == Some("message")
                && matches!(
                    item.get("phase").and_then(Value::as_str),
                    None | Some("final_answer")
                )
        })
        .flat_map(|item| {
            item.get("content")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
        })
        .find_map(|content| {
            (content.get("type").and_then(Value::as_str) == Some("output_text"))
                .then(|| content.get("text").and_then(Value::as_str))
                .flatten()
                .filter(|text| !text.trim().is_empty())
                .map(ToString::to_string)
        })
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
mod tests {
    use std::fmt::Write as _;

    use serde_json::json;

    use super::super::error::CodexClientError;
    use super::*;

    #[test]
    fn sse_parser_handles_fallback_output_and_typed_failures() {
        // Arrange
        let fallback = concat!(
            "data: {\"type\":\"response.created\"}\r\n\r\n",
            "data: {\"type\":\"response.completed\",\"response\":{",
            "\"output\":[{\"type\":\"message\",\"content\":[{",
            "\"type\":\"output_text\",\"text\":\"{\\\"name\\\":\\\"Ada\\\"}\"}]}]}}\n\n",
            "data: {not-json}\n\n"
        );
        let failures = [
            "data: {\"type\":\"error\",\"message\":\"failed\"}\n\n",
            "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"\
             reason\":\"max_output_tokens\"}}}\n\n",
            "data: {not-json}\n\n",
            "data: {\"type\":\"response.output_text.delta\"}\n\n",
            "data: {\"type\":\"response.completed\",\"response\":{}}\n\n",
            "data: {\"type\":\"response.incomplete\",\"response\":{}}\n\n",
            "data: {\"type\":\"error\"}\n\n",
            "data: {\"type\":\"response.created\"}",
            "data: [DONE]\n\n",
            "data: {\"type\":\"response.completed\"}\n\n",
        ];

        // Act
        let completion = parse_sse_response(fallback).expect("fallback output should parse");
        let errors = failures.map(parse_sse_response);

        // Assert
        assert_eq!(completion.output, r#"{"name":"Ada"}"#);
        assert!(matches!(errors[0], Err(CodexClientError::Provider { .. })));
        assert!(matches!(
            errors[1],
            Err(CodexClientError::Incomplete { .. })
        ));
        assert!(matches!(
            errors[2],
            Err(CodexClientError::InvalidSse { .. })
        ));
        assert!(matches!(
            errors[3],
            Err(CodexClientError::MissingResponseField("delta"))
        ));
        assert!(matches!(
            errors[4],
            Err(CodexClientError::MissingResponseField(
                "structured output text"
            ))
        ));
        assert!(matches!(
            &errors[5],
            Err(CodexClientError::Incomplete { reason })
                if reason == "Codex response was incomplete"
        ));
        assert!(matches!(
            &errors[6],
            Err(CodexClientError::Provider { message })
                if message == "Codex response failed"
        ));
        assert!(matches!(
            errors[7],
            Err(CodexClientError::MissingResponseField(
                "response.completed event"
            ))
        ));
        assert!(matches!(
            errors[8],
            Err(CodexClientError::MissingResponseField(
                "response.completed event"
            ))
        ));
        assert!(matches!(
            errors[9],
            Err(CodexClientError::MissingResponseField(
                "response.completed event"
            ))
        ));
    }

    #[test]
    fn sse_parser_prefers_final_answer_and_event_model_headers() {
        // Arrange
        let events = [
            json!({
                "type": "response.metadata",
                "headers": { "x-openai-model": "top-level-model" }
            }),
            json!({
                "type": "response.created",
                "response": {
                    "headers": { "OpenAI-Model": "routed-model" }
                }
            }),
            json!({
                "type": "response.output_text.delta",
                "delta": "Working on it."
            }),
            json!({
                "type": "response.output_text.delta",
                "delta": r#"{"name":"Ada"}"#
            }),
            json!({
                "type": "response.completed",
                "response": {
                    "model": "body-model",
                    "output": [
                        {
                            "type": "message",
                            "phase": "commentary",
                            "content": [{ "type": "output_text", "text": "Working on it." }]
                        },
                        {
                            "type": "message",
                            "phase": "final_answer",
                            "content": [{ "type": "output_text", "text": r#"{"name":"Ada"}"# }]
                        },
                        {
                            "type": "message",
                            "phase": "commentary",
                            "content": []
                        }
                    ]
                }
            }),
        ];
        let body = events.into_iter().fold(String::new(), |mut body, event| {
            write!(body, "data: {event}\n\n").expect("event should format");

            body
        });

        // Act
        let completion = parse_sse_response(&body).expect("response should parse");

        // Assert
        assert_eq!(completion.output, r#"{"name":"Ada"}"#);
        assert_eq!(completion.response_model.as_deref(), Some("routed-model"));
    }

    #[test]
    fn sse_decoder_bounds_and_finishes_pending_lines() {
        // Arrange
        let mut bounded = CodexSseDecoder::with_limits(1, 10, 10);
        let mut oversized_event = CodexSseDecoder::with_limits(10, 1, 10);
        let mut oversized_pending_event = CodexSseDecoder::with_limits(10, 1, 10);
        let mut oversized_content = CodexSseDecoder::with_limits(1_024, 1_024, 1);
        let mut invalid_utf8 = CodexSseDecoder::with_limits(2, 2, 2);
        let mut pending_invalid_utf8 = CodexSseDecoder::with_limits(1, 1, 1);
        let mut pending_completion = CodexSseDecoder::new(None);
        let pending_completion_event = concat!(
            "data: {\"type\":\"response.completed\",\"response\":{",
            "\"output\":[{\"type\":\"message\",\"content\":[{",
            "\"type\":\"output_text\",\"text\":\"{\\\"name\\\":\\\"Ada\\\"}\"}]}]}}"
        );
        let deltas = concat!(
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n",
            "data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n"
        );
        let fallback_response = json!({
            "output": [{
                "type": "message",
                "content": [{ "type": "output_text", "text": "xx" }]
            }]
        });

        // Act
        let oversized = bounded.push(b"xx").err();
        let oversized_event_error = oversized_event.push(b"xx\n").err();
        let oversized_pending_error = oversized_pending_event.push(b"xx").err();
        let oversized_content_error = oversized_content.push(deltas.as_bytes()).err();
        let oversized_fallback_error =
            completed_response(String::new(), &fallback_response, 1, None).err();
        let invalid = invalid_utf8.push(&[0xff, b'\n']).err();
        let pending_invalid = pending_invalid_utf8
            .push(&[0xff])
            .and_then(|_| pending_invalid_utf8.finish())
            .err();
        let pending = pending_completion
            .push(pending_completion_event.as_bytes())
            .and_then(|_| pending_completion.finish());

        // Assert
        assert!(matches!(
            oversized,
            Some(CodexClientError::ResponseTooLarge)
        ));
        assert!(matches!(
            oversized_event_error,
            Some(CodexClientError::ResponseTooLarge)
        ));
        assert!(matches!(
            oversized_pending_error,
            Some(CodexClientError::ResponseTooLarge)
        ));
        assert!(matches!(
            oversized_content_error,
            Some(CodexClientError::ResponseContentTooLarge)
        ));
        assert!(matches!(
            oversized_fallback_error,
            Some(CodexClientError::ResponseContentTooLarge)
        ));
        assert!(matches!(invalid, Some(CodexClientError::InvalidSse { .. })));
        assert!(matches!(
            pending_invalid,
            Some(CodexClientError::InvalidSse { .. })
        ));
        assert_eq!(
            pending.expect("pending completion should parse").output,
            r#"{"name":"Ada"}"#
        );
    }

    #[test]
    fn sse_decoder_processes_many_events_from_one_chunk() {
        // Arrange
        let delta = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"x\"}\n\n";
        let event_count = crate::transport::SUCCESS_BODY_LIMIT_BYTES / delta.len() + 1;
        let mut stream = delta.repeat(event_count);
        stream.push_str("data: {\"type\":\"response.completed\",\"response\":{}}\n\n");
        let mut decoder = CodexSseDecoder::new(None);

        // Act
        let completion = decoder
            .push(stream.as_bytes())
            .expect("event stream should parse")
            .expect("completion event should terminate the stream");

        // Assert
        assert_eq!(completion.output, "x".repeat(event_count));
        assert!(stream.len() > crate::transport::SUCCESS_BODY_LIMIT_BYTES);
    }

    #[test]
    fn usage_falls_back_to_reported_input_and_output_totals() {
        // Arrange
        let usage = json!({ "input_tokens": 10, "output_tokens": 4 });

        // Act
        let completion = completion_usage(&usage);

        // Assert
        assert_eq!(completion.input_tokens(), Some(10));
        assert_eq!(completion.output_tokens(), Some(4));
        assert_eq!(completion.total_tokens(), Some(14));
    }
}
