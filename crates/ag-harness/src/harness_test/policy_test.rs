use std::io;
use std::io::Cursor;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};

use super::support::{
    model, object_schema, read_call, read_harness, response_without_metadata, write_call,
    write_harness,
};
use crate::bash::{BashConfig, BashError};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEventKind, TurnErrorType};
use crate::model::{ModelError, ModelErrorType, ModelMessage, ModelResponse};
use crate::tool::ToolDefinition;
use crate::turn::TurnError;
use crate::{Repository, Tool, ToolPolicy, TurnOptions};

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
    let harness =
        Harness::new(model, unbounded_context_budget()).with_lifecycle_observer(move |event| {
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
    let harness = Harness::new(model, unbounded_context_budget());

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
    let harness = Harness::new(model, unbounded_context_budget()).with_lifecycle_observer(|_| {});

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
async fn runs_sequential_and_batched_tool_calls_without_a_per_turn_limit() {
    // Arrange
    let requests = Arc::new(AtomicUsize::new(0));
    let observed_requests = Arc::clone(&requests);
    let mut model = model();
    model.expect_complete().times(14).returning(move |_| {
        let index = observed_requests.fetch_add(1, Ordering::SeqCst);
        let response = match index {
            0..12 => ModelResponse::ToolCall(read_call(&format!("call_{index}"))),
            12 => ModelResponse::ToolCalls(
                (0..10)
                    .map(|call| read_call(&format!("batch_{call}")))
                    .collect(),
            ),
            _ => ModelResponse::Output(json!({"summary": "done"})),
        };

        Ok(response_without_metadata(response))
    });
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .returning(|path| Ok(path.to_path_buf()));
    file_system
        .expect_open_beneath()
        .times(22)
        .returning(|_, _| Ok(Box::new(Cursor::new(b"[workspace]"))));
    let harness = read_harness(model, file_system);

    // Act
    let outcome = harness
        .run_once("inspect", object_schema())
        .await
        .expect("turn without a tool-call limit");

    // Assert
    assert_eq!(outcome.output(), &json!({"summary": "done"}));
    assert_eq!(outcome.report().tool_calls().len(), 22);
    assert_eq!(requests.load(Ordering::SeqCst), 14);
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
    let harness = Harness::new(model, unbounded_context_budget());

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
    let harness = Harness::new(model, unbounded_context_budget()).with_lifecycle_observer(|_| {});

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

#[tokio::test]
async fn bash_permission_requires_host_configuration_and_host_information_grant() {
    // Arrange
    let mut model = model();
    model.expect_complete().never();
    let harness =
        Harness::new(model, unbounded_context_budget()).repository(Repository::fixture("repo"));
    let options = TurnOptions::new(object_schema(), ToolPolicy::default().allow(Tool::Bash));
    let configuration = BashConfig::new(
        "/trusted/launcher".into(),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(1),
        1024,
    )
    .expect("config");

    // Act
    let missing = harness.turn("run", options.clone()).await;
    let denied = harness.turn("run", options.with_bash(configuration)).await;

    // Assert
    assert!(matches!(
        missing,
        Err(TurnError::Bash(BashError::InvalidPolicy))
    ));
    assert!(matches!(
        denied,
        Err(TurnError::Bash(BashError::Unavailable))
    ));
}
