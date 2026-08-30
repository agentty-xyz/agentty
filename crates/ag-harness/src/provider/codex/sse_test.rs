use std::fmt::Write as _;

use serde_json::{Value, json};

use super::super::error::CodexClientError;
use super::{
    ByteCounter, CodexSseDecoder, completed_response, completion_usage, parse_sse_response,
    with_response_timeout,
};

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
        "data: {\"type\":\"response.incomplete\",\"response\":{\"incomplete_details\":{\"reason\":\
         \"max_output_tokens\"}}}\n\n",
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
fn sse_parser_prefers_final_done_item_when_completion_has_only_metadata() {
    // Arrange
    let events = [
        json!({
            "type": "response.output_text.delta",
            "delta": "Working on it."
        }),
        json!({
            "type": "response.output_text.delta",
            "delta": r#"{"name":"Ada"}"#
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "reasoning-1",
                "encrypted_content": "opaque-1"
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "phase": "commentary",
                "content": [{ "type": "output_text", "text": "Working on it." }]
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "reasoning-2",
                "encrypted_content": "opaque-2"
            }
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "message",
                "role": "assistant",
                "phase": "final_answer",
                "content": [
                    { "type": "output_text", "text": r#"{"name":""# },
                    { "type": "output_text", "text": r#"Ada"}"# }
                ]
            }
        }),
        json!({
            "type": "response.completed",
            "response": { "id": "response-1" }
        }),
    ];
    let body = events.into_iter().fold(String::new(), |mut body, event| {
        write!(body, "data: {event}\n\n").expect("event should format");

        body
    });

    // Act
    let completion = parse_sse_response(&body).expect("response should parse");
    let replay: Vec<Value> = serde_json::from_str(
        completion
            .reasoning_content
            .as_deref()
            .expect("interleaved replay should be retained"),
    )
    .expect("replay should be JSON");

    // Assert
    assert_eq!(completion.output, r#"{"name":"Ada"}"#);
    assert_eq!(replay.len(), 4);
    assert_eq!(replay[0].get("type"), Some(&json!("reasoning")));
    assert_eq!(replay[1].get("phase"), Some(&json!("commentary")));
    assert_eq!(replay[2].get("type"), Some(&json!("reasoning")));
    assert_eq!(replay[3].get("phase"), Some(&json!("final_answer")));
}

#[test]
fn completed_event_rejects_backend_failure_state() {
    // Arrange
    let failed = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{",
        "\"status\":\"failed\",\"error\":{\"message\":\"backend failed\"},",
        "\"output\":[{\"type\":\"message\",\"content\":[{",
        "\"type\":\"output_text\",\"text\":\"{\\\"name\\\":\\\"Ada\\\"}\"}]}]}}\n\n"
    );
    let incomplete = concat!(
        "data: {\"type\":\"response.completed\",\"response\":{",
        "\"status\":\"incomplete\",\"incomplete_details\":{",
        "\"reason\":\"max_output_tokens\"}}}\n\n"
    );
    let failed_without_message =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"cancelled\"}}\n\n";
    let incomplete_without_reason =
        "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"incomplete\"}}\n\n";

    // Act
    let failed_error = parse_sse_response(failed).err();
    let incomplete_error = parse_sse_response(incomplete).err();
    let failed_without_message_error = parse_sse_response(failed_without_message).err();
    let incomplete_without_reason_error = parse_sse_response(incomplete_without_reason).err();

    // Assert
    assert!(matches!(
        failed_error,
        Some(CodexClientError::Provider { message }) if message == "backend failed"
    ));
    assert!(matches!(
        incomplete_error,
        Some(CodexClientError::Incomplete { reason }) if reason == "max_output_tokens"
    ));
    assert!(matches!(
        failed_without_message_error,
        Some(CodexClientError::Provider { message }) if message == "Codex response failed"
    ));
    assert!(matches!(
        incomplete_without_reason_error,
        Some(CodexClientError::Incomplete { reason })
            if reason == "Codex response was incomplete"
    ));
}

#[test]
fn sse_parser_retains_encrypted_reasoning_for_session_replay() {
    // Arrange
    let streamed_reasoning = json!({
        "type": "reasoning",
        "id": "reasoning-streamed",
        "encrypted_content": "streamed"
    });
    let completed_reasoning = json!({
        "type": "reasoning",
        "id": "reasoning-completed",
        "encrypted_content": "completed"
    });
    let events = [
        json!({
            "type": "response.output_item.done",
            "item": streamed_reasoning
        }),
        json!({
            "type": "response.completed",
            "response": {
                "output": [
                    completed_reasoning,
                    {
                        "type": "message",
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": r#"{"name":"Ada"}"#
                        }]
                    }
                ]
            }
        }),
    ];
    let body = events.into_iter().fold(String::new(), |mut body, event| {
        write!(body, "data: {event}\n\n").expect("event should format");

        body
    });
    let streamed_events = [
        json!({
            "type": "response.output_text.delta",
            "delta": r#"{"name":"Ada"}"#
        }),
        json!({
            "type": "response.output_item.done",
            "item": {
                "type": "reasoning",
                "id": "reasoning-streamed",
                "encrypted_content": "streamed"
            }
        }),
        json!({
            "type": "response.completed",
            "response": {}
        }),
    ];
    let streamed_body = streamed_events
        .into_iter()
        .fold(String::new(), |mut body, event| {
            write!(body, "data: {event}\n\n").expect("event should format");

            body
        });

    // Act
    let completion = parse_sse_response(&body).expect("response should parse");
    let replay: Vec<Value> = serde_json::from_str(
        completion
            .reasoning_content
            .as_deref()
            .expect("reasoning should be retained"),
    )
    .expect("reasoning replay should be JSON");
    let streamed_completion =
        parse_sse_response(&streamed_body).expect("streamed response should parse");
    let streamed_replay: Vec<Value> = serde_json::from_str(
        streamed_completion
            .reasoning_content
            .as_deref()
            .expect("streamed reasoning should be retained"),
    )
    .expect("streamed replay should be JSON");

    // Assert
    assert_eq!(replay[0], completed_reasoning);
    assert_eq!(replay[1].get("type"), Some(&json!("message")));
    assert_eq!(streamed_replay.len(), 2);
    assert_eq!(streamed_replay[0].get("type"), Some(&json!("reasoning")));
    assert_eq!(streamed_replay[1].get("type"), Some(&json!("message")));
}

#[test]
fn sse_decoder_bounds_and_finishes_pending_lines() {
    // Arrange
    let mut bounded = CodexSseDecoder::with_limits(1, 10, 10);
    let mut oversized_event = CodexSseDecoder::with_limits(10, 1, 10);
    let mut oversized_pending_event = CodexSseDecoder::with_limits(10, 1, 10);
    let mut oversized_content = CodexSseDecoder::with_limits(1_024, 1_024, 1);
    let mut oversized_replay = CodexSseDecoder::with_limits(1_024, 1_024, 20);
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
    let oversized_reasoning_response = json!({
        "output": [
            {
                "type": "reasoning",
                "encrypted_content": "xx"
            },
            {
                "type": "message",
                "content": [{ "type": "output_text", "text": "x" }]
            }
        ]
    });

    // Act
    let oversized = bounded.push(b"xx").err();
    let oversized_event_error = oversized_event.push(b"xx\n").err();
    let oversized_pending_error = oversized_pending_event.push(b"xx").err();
    let oversized_content_error = oversized_content.push(deltas.as_bytes()).err();
    let oversized_replay_error = oversized_replay
        .push(
            b"data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"reasoning\",\"encrypted_content\":\"opaque\"}}\n",
        )
        .err();
    let oversized_fallback_error =
        completed_response(String::new(), Vec::new(), &fallback_response, 1, None).err();
    let oversized_reasoning_error = completed_response(
        String::new(),
        Vec::new(),
        &oversized_reasoning_response,
        1,
        None,
    )
    .err();
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
        oversized_replay_error,
        Some(CodexClientError::ResponseContentTooLarge)
    ));
    assert!(matches!(
        oversized_fallback_error,
        Some(CodexClientError::ResponseContentTooLarge)
    ));
    assert!(matches!(
        oversized_reasoning_error,
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
fn sse_decoder_ignores_non_replay_items() {
    // Arrange
    let mut decoder = CodexSseDecoder::new(None);

    // Act
    let result = decoder.push(
        b"data: {\"type\":\"response.output_item.done\",\"item\":{\"type\":\"function_call\"}}\n\n",
    );
    let flushed = std::io::Write::flush(&mut ByteCounter::default());

    // Assert
    assert!(matches!(result, Ok(None)));
    assert!(flushed.is_ok());
}

#[tokio::test]
async fn response_timeout_bounds_an_active_stream() {
    // Act
    let error = with_response_timeout(
        std::future::pending::<Result<(), CodexClientError>>(),
        std::time::Duration::from_millis(1),
    )
    .await
    .err();

    // Assert
    assert!(matches!(error, Some(CodexClientError::ResponseTimeout)));
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
