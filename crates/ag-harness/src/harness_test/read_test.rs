use std::io;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mockall::Sequence;
use serde_json::{Value, json};

use super::support::{
    assert_read_tool_lifecycle, inspection_call, model, object_schema, read_call,
    read_call_with_path, read_harness, readable_file_system, readable_file_system_with,
    response_with_metadata, response_without_metadata,
};
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, ModelResponseType, ToolErrorType};
use crate::model::{ModelMessage, ModelRequest, ModelResponse, ReasoningEffort};
use crate::read::ReadError;
use crate::repository::Repository;
use crate::tool::{ReadAction, Tool, ToolCall, ToolDefinition};
use crate::turn::{ToolActivity, TurnError};

#[tokio::test]
async fn applies_reasoning_effort_to_every_model_call() {
    // Arrange
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .withf(|request| request.model_reasoning_effort() == Some(ReasoningEffort::Low))
        .returning(|_| {
            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "quick"
            }))))
        });
    let harness = Harness::new(model).model_reasoning_effort(ReasoningEffort::Low);

    // Act
    let output = harness
        .run_once("reply quickly", object_schema())
        .await
        .expect("configured reasoning request should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "quick" }));
}

#[test]
fn preserves_request_reasoning_effort_over_harness_default() {
    // Arrange
    let harness = Harness::new(model()).model_reasoning_effort(ReasoningEffort::Low);
    let request = ModelRequest::new("reply", object_schema())
        .with_model_reasoning_effort(ReasoningEffort::High);

    // Act
    let (request, read_tool, write_tool) = harness
        .prepare_request(request, None)
        .expect("request preparation should succeed");

    // Assert
    assert_eq!(
        request.model_reasoning_effort(),
        Some(ReasoningEffort::High)
    );
    assert!(read_tool.is_none());
    assert!(write_tool.is_none());
}

#[tokio::test]
async fn completes_read_tool_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(request.tools(), &[ToolDefinition::read()]);

            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )));
        }
        assert_eq!(request.messages().len(), 3);
        assert!(matches!(
            &request.messages()[0],
            ModelMessage::User(prompt) if prompt == "inspect the manifest"
        ));
        assert!(matches!(
            &request.messages()[1],
            ModelMessage::AssistantToolCall(call) if call.id() == "call_read"
        ));
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            }
                if call_id == "call_read"
                    && name == "read"
                    && serde_json::from_str::<Value>(content)
                        .is_ok_and(|value| value["content"] == "[workspace]")
        ));

        Ok(response_without_metadata(ModelResponse::Output(
            json!({ "summary": "workspace" }),
        )))
    });
    let harness = read_harness(model, readable_file_system());

    // Act
    let output = harness
        .run_once("inspect the manifest", object_schema())
        .await
        .expect("tool round trip should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "workspace" }));
}

#[tokio::test]
async fn completes_repository_inspection_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                inspection_call(
                    "call_list",
                    json!({ "action": "list", "path": "Cargo.toml" }),
                ),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { content, .. }
                if serde_json::from_str::<Value>(content)
                    .is_ok_and(|value| value["result"] == json!(["Cargo.toml"]))
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "listed"
        }))))
    });
    let harness = Harness::new(model)
        .repository(Repository::fixture(env!("CARGO_MANIFEST_DIR")))
        .allow(Tool::Read);

    // Act
    let outcome = harness
        .run_once("list the manifest", object_schema())
        .await
        .expect("repository inspection should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "listed" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadInspection {
            action: ReadAction::List,
            summary,
            ..
        } if summary == "Cargo.toml"
    ));
}

#[tokio::test]
async fn returns_encoded_file_result_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )));
        }
        assert!(request.messages().iter().any(|message| {
            matches!(
                message,
                ModelMessage::ToolResult { content, .. }
                    if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                        value["status"] == "rejected"
                            && value["error"]
                                .as_str()
                                .is_some_and(|error| error.contains("exceeds the read size limit"))
                    })
            )
        }));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "recovered"
        }))))
    });
    let content = "\u{1}".repeat(16 * 1024).into_bytes();
    let harness = read_harness(model, readable_file_system_with(content));

    // Act
    let outcome = harness
        .run_once("read the manifest", object_schema())
        .await
        .expect("model should recover from an encoded-size rejection");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadRejected { path, .. } if path == "Cargo.toml"
    ));
}

#[tokio::test]
async fn returns_schema_valid_read_argument_rejections_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model
        .expect_complete()
        .times(2)
        .returning(move |request| {
            if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
                return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                    inspection_call("call_file", json!({})),
                    inspection_call("call_search", json!({ "action": "search" })),
                ])));
            }
            assert!(request.messages().iter().any(|message| {
                matches!(
                    message,
                    ModelMessage::ToolResult { content, .. }
                        if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                            value["status"] == "rejected"
                                && value["error"] == "file requires a path and accepts only offset and limit"
                        })
                )
            }));
            assert!(request.messages().iter().any(|message| {
                matches!(
                    message,
                    ModelMessage::ToolResult { content, .. }
                        if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                            value["status"] == "rejected"
                                && value["error"] == "search requires a query and accepts only an optional path and limit"
                        })
                )
            }));

            Ok(response_without_metadata(ModelResponse::Output(json!({
                "summary": "recovered"
            }))))
        });
    let harness = read_harness(model, MockFileSystem::new());

    // Act
    let outcome = harness
        .run_once("read and search the repository", object_schema())
        .await
        .expect("model should recover from schema-portable argument rejection");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadRejected { path, .. } if path == "read"
    ));
    assert!(matches!(
        &outcome.report().tool_calls()[1],
        ToolActivity::ReadInspectionRejected {
            action: ReadAction::Search,
            summary,
            ..
        } if summary == "read"
    ));
}

#[tokio::test]
async fn returns_correctable_repository_inspection_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                inspection_call(
                    "call_show",
                    json!({
                        "action": "show",
                        "path": "definitely-missing-review-file",
                        "side": "head"
                    }),
                ),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { content, .. }
                if serde_json::from_str::<Value>(content)
                    .is_ok_and(|value| value["status"] == "rejected")
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "recovered"
        }))))
    });
    let harness = Harness::new(model)
        .repository(Repository::fixture(env!("CARGO_MANIFEST_DIR")))
        .allow(Tool::Read);

    // Act
    let outcome = harness
        .run_once("show the missing file", object_schema())
        .await
        .expect("model should recover from a rejected inspection");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert!(matches!(
        &outcome.report().tool_calls()[0],
        ToolActivity::ReadInspectionRejected {
            action: ReadAction::Show,
            summary,
            ..
        } if summary == "definitely-missing-review-file"
    ));
}

#[tokio::test]
async fn returns_repository_inspection_boundary_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            inspection_call("call_list", json!({ "action": "list" })),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::other("repository unavailable")));
    file_system.expect_open_beneath().times(0);
    let harness = read_harness(model, file_system);

    // Act
    let error = harness
        .run_once("list files", object_schema())
        .await
        .expect_err("repository boundary failure should end the turn");

    // Assert
    assert!(matches!(
        error,
        TurnError::Read(ReadError::RepositoryRoot { .. })
    ));
}

#[tokio::test]
async fn completes_multiple_read_tools_from_one_model_response() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
                read_call("call_one"),
                read_call("call_two"),
            ])));
        }
        assert!(matches!(
            &request.messages()[1],
            ModelMessage::AssistantToolCalls(calls)
                if calls.iter().map(ToolCall::id).collect::<Vec<_>>()
                    == ["call_one", "call_two"]
        ));
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { call_id, .. } if call_id == "call_one"
        ));
        assert!(matches!(
            &request.messages()[3],
            ModelMessage::ToolResult { call_id, .. } if call_id == "call_two"
        ));

        Ok(response_without_metadata(ModelResponse::Output(
            json!({ "summary": "workspace" }),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(4)
        .returning(|path| {
            if path == std::path::Path::new("repo") {
                Ok(PathBuf::from("/repo"))
            } else {
                Ok(PathBuf::from("/repo/Cargo.toml"))
            }
        });
    file_system
        .expect_open_beneath()
        .times(2)
        .returning(|_, _| {
            Ok(Box::new(Cursor::new(
                b"[workspace]\nmember = true\n".to_vec(),
            )))
        });
    let harness = read_harness(model, file_system);

    // Act
    let outcome = harness
        .run_once("inspect two files", object_schema())
        .await
        .expect("parallel tool round trip should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "workspace" }));
    assert_eq!(outcome.report().tool_calls().len(), 2);
}

#[tokio::test]
async fn returns_correctable_read_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult { content, .. }
                if serde_json::from_str::<Value>(content).is_ok_and(|value| {
                    value["path"] == "Cargo.toml" && value["status"] == "rejected"
                })
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "recovered"
        }))))
    });
    let mut file_system = MockFileSystem::new();
    let mut sequence = Sequence::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_canonicalize()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing")));
    file_system.expect_open_beneath().times(0);
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = read_harness(model, file_system).with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });

    // Act
    let outcome = harness
        .run_once("inspect", object_schema())
        .await
        .expect("model should recover from a rejected read path");

    // Assert
    assert_eq!(outcome.output(), &json!({ "summary": "recovered" }));
    assert_eq!(outcome.report().tool_calls().len(), 1);
    let activity = &outcome.report().tool_calls()[0];
    assert_eq!(activity.name(), "read");
    assert_eq!(activity.path(), "Cargo.toml");
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert!(matches!(
        events[5].kind(),
        LifecycleEventKind::ToolFailed {
            error_type: ToolErrorType::Execution,
            ..
        }
    ));
    assert!(matches!(
        events[8].kind(),
        LifecycleEventKind::TurnCompleted { .. }
    ));
}

#[tokio::test]
async fn report_describes_model_requests_and_repository_reads() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |_| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call_with_path("call_read", "Cargo\n.toml\u{1b}"),
            )));
        }

        Ok(response_with_metadata(ModelResponse::Output(json!({
            "summary": "workspace"
        }))))
    });
    let harness = read_harness(model, readable_file_system());

    // Act
    let outcome = harness
        .run_once("inspect", object_schema())
        .await
        .expect("reported turn should succeed");

    // Assert
    assert_eq!(outcome.output(), &json!({"summary": "workspace"}));
    assert_eq!(outcome.report().model_requests().len(), 2);
    let final_request = &outcome.report().model_requests()[1];
    assert_eq!(final_request.response_type(), ModelResponseType::Output);
    let metadata = final_request
        .completion()
        .expect("metadata should be present");
    assert_eq!(metadata.finish_reason(), "stop\u{fffd}forged");
    assert_eq!(metadata.response_id(), Some("response\u{fffd}-1"));
    assert_eq!(metadata.response_model(), Some("reported\u{fffd}model"));
    assert_eq!(metadata.system_fingerprint(), Some("finger\u{fffd}print"));
    assert_eq!(
        metadata.usage().and_then(|usage| usage.total_tokens()),
        Some(16)
    );
    assert_eq!(outcome.report().tool_calls().len(), 1);
    let activity = &outcome.report().tool_calls()[0];
    assert_eq!(activity.name(), "read");
    assert_eq!(activity.path(), "Cargo\u{fffd}.toml\u{fffd}");
    assert!(activity.duration() <= outcome.report().duration());
}

#[tokio::test]
async fn emits_correlated_lifecycle_for_read_tool_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        assert!(request.lifecycle_observed());
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("provider-call-id"),
            )));
        }

        Ok(response_without_metadata(ModelResponse::Output(
            json!({ "summary": "workspace" }),
        )))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness =
        read_harness(model, readable_file_system()).with_lifecycle_observer(move |event| {
            observed_events
                .lock()
                .expect("event recorder should not be poisoned")
                .push(event);
        });

    // Act
    let output = harness
        .run_once("sensitive prompt", object_schema())
        .await
        .expect("tool round trip should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "workspace" }));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert_eq!(events.len(), 9);
    assert_eq!(
        events
            .iter()
            .map(LifecycleEvent::sequence)
            .collect::<Vec<_>>(),
        (0..9).collect::<Vec<_>>()
    );
    assert_read_tool_lifecycle(&events);
    let event_debug = format!("{events:?}");
    assert!(!event_debug.contains("sensitive prompt"));
    assert!(!event_debug.contains("[workspace]"));
}
