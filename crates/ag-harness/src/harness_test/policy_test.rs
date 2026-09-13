use std::io;
use std::io::Cursor;
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::support::{
    model, object_schema, read_call, read_harness, readable_file_system, response_without_metadata,
    write_call, write_harness,
};
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, ToolErrorType, TurnErrorType};
use crate::model::{ModelError, ModelErrorType, ModelMessage, ModelResponse};
use crate::tool::ToolDefinition;
use crate::turn::TurnError;

#[tokio::test]
async fn rejects_schema_invalid_output_from_injected_model() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": 42
        }))))
    });
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed_events = Arc::clone(&events);
    let harness = Harness::new(model).with_lifecycle_observer(move |event| {
        observed_events
            .lock()
            .expect("event recorder should not be poisoned")
            .push(event);
    });

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("schema-invalid custom output should fail");

    // Assert
    assert!(matches!(
        error,
        TurnError::Model(ModelError::SchemaViolation { path, .. }) if path == "/summary"
    ));
    let events = events
        .lock()
        .expect("event recorder should not be poisoned");
    assert_eq!(events.len(), 4);
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        LifecycleEventKind::ModelRequestFailed {
            error_type: ModelErrorType::InvalidOutput,
            ..
        }
    )));
    assert!(events.iter().any(|event| matches!(
        event.kind(),
        LifecycleEventKind::TurnFailed {
            error_type: TurnErrorType::Model(ModelErrorType::InvalidOutput),
            ..
        }
    )));
}

#[tokio::test]
async fn completes_write_tool_round_trip() {
    // Arrange
    let patch = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n";
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(request.tools(), &[ToolDefinition::write()]);

            return Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call("call_write", patch),
            )));
        }
        assert!(matches!(
            &request.messages()[2],
            ModelMessage::ToolResult {
                call_id,
                content,
                name,
            }
                if call_id == "call_write"
                    && name == "write"
                    && serde_json::from_str::<Value>(content).is_ok_and(|value| {
                        value == json!({
                            "bytes_written": 4,
                            "path": "src/lib.rs",
                            "status": "applied"
                        })
                    })
        ));

        Ok(response_without_metadata(ModelResponse::Output(json!({
            "summary": "updated"
        }))))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system
        .expect_replace_beneath()
        .times(1)
        .withf(|_, _, expected, content| {
            expected.as_deref() == Some(b"old\n".as_slice()) && content == b"new\n"
        })
        .returning(|_, _, _, _| Ok(()));
    let harness = write_harness(model, file_system);

    // Act
    let output = harness
        .run_once("update the file", object_schema())
        .await
        .expect("write round trip should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "updated" }));
}

#[tokio::test]
async fn returns_correctable_write_rejection_to_model() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        if call_count.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(response_without_metadata(ModelResponse::ToolCall(
                write_call("call_write", "not a unified diff"),
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
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Ok(PathBuf::from("/repo")));
    file_system
        .expect_open_beneath()
        .times(1)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"old\n".to_vec()))));
    file_system.expect_replace_beneath().times(0);
    let harness = write_harness(model, file_system);

    // Act
    let output = harness
        .run_once("update", object_schema())
        .await
        .expect("model should recover from rejected patch");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "recovered" }));
}

#[tokio::test]
async fn returns_terminal_write_boundary_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            write_call(
                "call_write",
                "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
            ),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
    let harness = write_harness(model, file_system);

    // Act
    let error = harness
        .run_once("update", object_schema())
        .await
        .expect_err("write boundary failure should end turn");

    // Assert
    assert!(matches!(&error, TurnError::Write(_)));
    assert_eq!(error.error_type(), TurnErrorType::Tool);
}

#[tokio::test]
async fn rejects_disabled_write_call() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|request| {
        assert_eq!(request.tools(), []);

        Ok(response_without_metadata(ModelResponse::ToolCall(
            write_call(
                "call_denied",
                "--- /dev/null\n+++ b/src/lib.rs\n@@ -0,0 +1 @@\n+new\n",
            ),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    file_system.expect_replace_beneath().times(0);
    let harness = Harness::new(model);

    // Act
    let error = harness
        .run_once("update", object_schema())
        .await
        .expect_err("denied write should fail");

    // Assert
    assert!(matches!(
        error,
        TurnError::ToolDenied { name } if name == "write"
    ));
}

#[tokio::test]
async fn rejects_disabled_read_call() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|request| {
        assert_eq!(request.tools(), []);

        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("call_denied"),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    let harness = Harness::new(model).with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("denied tool should fail");

    // Assert
    assert!(matches!(
        &error,
        TurnError::ToolDenied { name } if name == "read"
    ));
    assert_eq!(error.error_type(), TurnErrorType::ToolDenied);
}

#[tokio::test]
async fn enforces_tool_call_limit() {
    for observed in [false, true] {
        // Arrange
        let mut model = model();
        model.expect_complete().times(2).returning(|_| {
            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_read"),
            )))
        });
        let events = Arc::new(Mutex::new(Vec::new()));
        let mut harness = read_harness(model, readable_file_system())
            .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));
        if observed {
            let events = Arc::clone(&events);
            harness = harness.with_lifecycle_observer(move |event: LifecycleEvent| {
                events.lock().expect("events lock").push(event);
            });
        }

        // Act
        let error = harness
            .run_once("inspect", object_schema())
            .await
            .expect_err("second tool call should exceed the limit");

        // Assert
        assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
        assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
        let events = events.lock().expect("events lock");
        assert_eq!(
            events.iter().any(|event| matches!(
                event.kind(),
                LifecycleEventKind::ToolFailed {
                    error_type: ToolErrorType::CallLimit,
                    ..
                }
            )),
            observed
        );
    }
}

#[tokio::test]
async fn enforces_tool_call_limit_within_one_model_response() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
            read_call("call_one"),
            read_call("call_two"),
        ])))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    let harness = read_harness(model, file_system)
        .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("second batched tool call should exceed the limit");

    // Assert
    assert!(matches!(error, TurnError::ToolCallLimit { limit: 1 }));
}

#[tokio::test]
async fn rejects_batched_writes_before_any_write_executes() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
            write_call(
                "call_one",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
            ),
            write_call(
                "call_two",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
            ),
        ])))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    file_system.expect_replace_beneath().times(0);
    let harness = write_harness(model, file_system)
        .max_tool_calls(NonZeroUsize::new(1).expect("limit should be non-zero"));

    // Act
    let error = harness
        .run_once("update twice", object_schema())
        .await
        .expect_err("oversized batch should fail before writing");

    // Assert
    assert!(matches!(&error, TurnError::ToolCallLimit { limit: 1 }));
    assert_eq!(error.error_type(), TurnErrorType::ToolCallLimit);
}

#[tokio::test]
async fn rejects_duplicate_batched_call_ids_before_any_write_executes() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(vec![
            write_call(
                "duplicate_call",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+first\n",
            ),
            write_call(
                "duplicate_call",
                "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+second\n",
            ),
        ])))
    });
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    file_system.expect_replace_beneath().times(0);
    let harness = write_harness(model, file_system);

    // Act
    let error = harness
        .run_once("update twice", object_schema())
        .await
        .expect_err("duplicate call identifiers should fail before writing");

    // Assert
    assert!(matches!(
        &error,
        TurnError::Model(ModelError::DuplicateToolCallId { id }) if id == "duplicate_call"
    ));
    assert_eq!(
        error.error_type(),
        TurnErrorType::Model(ModelErrorType::InvalidToolCall)
    );
}

#[tokio::test]
async fn rejects_empty_tool_call_batch() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCalls(
            Vec::new(),
        )))
    });
    let harness = Harness::new(model);

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("empty tool batch should fail immediately");

    // Assert
    assert!(matches!(
        &error,
        TurnError::Model(ModelError::MissingToolCall)
    ));
    assert_eq!(
        error.error_type(),
        TurnErrorType::Model(ModelErrorType::InvalidToolCall)
    );
}

#[tokio::test]
async fn returns_typed_read_failure() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("call_read"),
        )))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .times(1)
        .returning(|_| Err(io::Error::new(io::ErrorKind::NotFound, "missing root")));
    let harness = read_harness(model, file_system).with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("filesystem failure should end the turn");

    // Assert
    assert!(matches!(&error, TurnError::Read(_)));
    assert_eq!(error.error_type(), TurnErrorType::Tool);
}

#[tokio::test]
async fn returns_typed_model_failure() {
    // Arrange
    let mut model = model();
    model
        .expect_complete()
        .times(1)
        .returning(|_| Err(ModelError::request(io::Error::other("offline"))));
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().times(0);
    file_system.expect_open_beneath().times(0);
    let harness = Harness::new(model).with_lifecycle_observer(|_| {});

    // Act
    let error = harness
        .run_once("inspect", object_schema())
        .await
        .expect_err("model failure should end the turn");

    // Assert
    assert!(matches!(&error, TurnError::Model(ModelError::Request(_))));
    assert_eq!(
        error.error_type(),
        TurnErrorType::Model(ModelErrorType::Request)
    );
}
