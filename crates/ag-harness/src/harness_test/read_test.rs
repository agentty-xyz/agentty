use std::io;
use std::io::Cursor;
use std::num::{NonZeroU64, NonZeroUsize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use mockall::Sequence;
use serde_json::{Value, json};
use tempfile::tempdir;

use super::support::{
    assert_read_tool_lifecycle, inspection_call, model, object_schema, read_call,
    read_call_with_path, read_harness, readable_file_system, readable_file_system_with,
    response_with_metadata, response_without_metadata,
};
use crate::context::{ContextBudget, ContextEstimator};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::engine::MAX_ELISION_EXTENSIONS;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, ModelResponseType, ToolErrorType};
use crate::model::{MockModel, ModelCompletion, ModelMessage, ModelResponse, ReasoningEffort};
use crate::model_registry::{ModelCapabilities, ModelRegistry};
use crate::read::ReadError;
use crate::recovery::ExecutionIdentity;
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
    let harness = Harness::new(model, unbounded_context_budget())
        .model_reasoning_effort(ReasoningEffort::Low);

    // Act
    let output = harness
        .run_once("reply quickly", object_schema())
        .await
        .expect("configured reasoning request should succeed");

    // Assert
    assert_eq!(output.output(), &json!({ "summary": "quick" }));
}

#[tokio::test]
async fn completes_read_tool_round_trip() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model.expect_complete().times(2).returning(move |request| {
        let call_index = call_count.fetch_add(1, Ordering::SeqCst);
        if call_index == 0 {
            assert_eq!(
                request.tools(),
                &[ToolDefinition::read_with_comparison_base(None)]
            );

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
    let harness = Harness::new(model, unbounded_context_budget())
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
    let harness = Harness::new(model, unbounded_context_budget())
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

/// Deterministic flat weights so in-turn budget arithmetic stays readable.
struct FlatEstimator;

impl ContextEstimator for FlatEstimator {
    fn message_weight(&self, _message: &ModelMessage) -> u64 {
        7
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        2
    }
}

#[tokio::test]
async fn tool_results_that_outgrow_the_budget_fail_typed_mid_turn() {
    // Arrange
    let mut model = model();
    model.expect_complete().times(1).returning(|_| {
        Ok(response_without_metadata(ModelResponse::ToolCall(
            read_call("call_read"),
        )))
    });
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("budgeted", "1").expect("identity"),
            model,
            ModelCapabilities {
                context_budget: ContextBudget::new(NonZeroU64::new(15).expect("nonzero budget")),
                image_input: false,
                tool_calls: true,
            },
        )
        .expect("register");
    let harness = Harness::from_registry(&registry, "budgeted")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(readable_file_system())
        .context_estimator(FlatEstimator);

    // Act
    let rejected = harness
        .run_once("inspect the manifest", object_schema())
        .await;

    // Assert
    // Admission passes with the 7-weight input message + tool definition 2
    // of 15, but the recorded call and result grow the request to three
    // 7-weight messages plus the 2-weight definition before the second
    // model call.
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded {
            budget: 15,
            required: 23,
        })
    ));
}

/// Weighs tool results by content bytes and every other item at one unit.
struct ToolResultBytesEstimator;

impl ContextEstimator for ToolResultBytesEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        match message {
            ModelMessage::ToolResult { content, .. } => {
                u64::try_from(content.len()).expect("content length fits u64")
            }
            _ => 1,
        }
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }
}

#[tokio::test]
async fn grown_requests_send_older_tool_results_elided_mid_turn() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    let third_request = Arc::new(Mutex::new(None));
    let captured_request = Arc::clone(&third_request);
    model.expect_complete().times(3).returning(move |request| {
        match call_count.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_one"),
            ))),
            1 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_two"),
            ))),
            _ => {
                *captured_request.lock().expect("request lock") = Some(request.messages().to_vec());

                Ok(response_without_metadata(ModelResponse::Output(
                    json!({ "summary": "workspace" }),
                )))
            }
        }
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
        .returning(|_, _| Ok(Box::new(Cursor::new("x".repeat(600).into_bytes()))));
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("budgeted", "1").expect("identity"),
            model,
            ModelCapabilities {
                context_budget: ContextBudget::new(NonZeroU64::new(1_000).expect("nonzero budget")),
                image_input: false,
                tool_calls: true,
            },
        )
        .expect("register");
    let harness = Harness::from_registry(&registry, "budgeted")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(file_system)
        .context_estimator(ToolResultBytesEstimator);

    // Act
    let outcome = harness
        .run_once("inspect the manifest twice", object_schema())
        .await
        .expect("elided request should fit");

    // Assert
    // Each read result carries the 600-byte line, so two results exceed
    // 1,000 while one result plus the elision notice fits.
    assert_eq!(outcome.output(), &json!({ "summary": "workspace" }));
    let messages = third_request
        .lock()
        .expect("request lock")
        .take()
        .expect("third request captured");
    assert!(matches!(
        &messages[2],
        ModelMessage::ToolResult { call_id, content, .. }
            if call_id == "call_one" && content.contains("\"elided\"")
    ));
    assert!(matches!(
        &messages[4],
        ModelMessage::ToolResult { call_id, content, .. }
            if call_id == "call_two" && content.contains(&"x".repeat(600))
    ));
}

/// Serves a single `line_bytes`-byte line to every manifest read.
fn file_system_reading_lines_of(line_bytes: usize) -> MockFileSystem {
    let mut file_system = MockFileSystem::new();
    file_system.expect_canonicalize().returning(|path| {
        if path == std::path::Path::new("repo") {
            Ok(PathBuf::from("/repo"))
        } else {
            Ok(PathBuf::from("/repo/Cargo.toml"))
        }
    });
    file_system
        .expect_open_beneath()
        .returning(move |_, _| Ok(Box::new(Cursor::new("x".repeat(line_bytes).into_bytes()))));

    file_system
}

/// Weighs fresh tool results by content bytes and every other item, including
/// elision notices, at one unit, so only the elision cap can stop a turn.
struct FreshResultBytesEstimator;

impl ContextEstimator for FreshResultBytesEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        match message {
            ModelMessage::ToolResult { content, .. } if !content.contains("\"elided\"") => {
                u64::try_from(content.len()).expect("content length fits u64")
            }
            _ => 1,
        }
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }
}

#[tokio::test]
async fn turns_that_keep_needing_elision_fail_typed_after_the_elision_cap() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    model
        .expect_complete()
        .times(MAX_ELISION_EXTENSIONS + 2)
        .returning(move |_| {
            let index = call_count.fetch_add(1, Ordering::SeqCst);

            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call(&format!("call_{index}")),
            )))
        });
    let file_system = file_system_reading_lines_of(600);
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("budgeted", "1").expect("identity"),
            model,
            ModelCapabilities {
                context_budget: ContextBudget::new(NonZeroU64::new(1_000).expect("nonzero budget")),
                image_input: false,
                tool_calls: true,
            },
        )
        .expect("register");
    let harness = Harness::from_registry(&registry, "budgeted")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(file_system)
        .context_estimator(FreshResultBytesEstimator);

    // Act
    let rejected = harness
        .run_once("keep re-reading the manifest", object_schema())
        .await;

    // Assert
    // Every request after the second read elides one more result and fits;
    // once the cap is spent, the next grown request only re-elides the prefix
    // and fails with its two latest results fresh, far below its full weight.
    assert!(matches!(
        rejected,
        Err(TurnError::ContextBudgetExceeded { budget: 1_000, required })
            if (1_000..2_400).contains(&required)
    ));
}

/// Call index whose fresh result [`HeavyResultsEstimator`] weighs at 700,
/// reached only after the elision cap's worth of re-elide-only requests.
const LATE_HEAVY_CALL: usize = MAX_ELISION_EXTENSIONS + 5;

/// Weighs the fresh results of `call_0` at 990 and of [`LATE_HEAVY_CALL`] at
/// 700 and every other item at one unit, so a turn outgrows a 1,000 budget
/// only through those two results.
struct HeavyResultsEstimator;

impl ContextEstimator for HeavyResultsEstimator {
    fn message_weight(&self, message: &ModelMessage) -> u64 {
        match message {
            ModelMessage::ToolResult {
                call_id, content, ..
            } if !content.contains("\"elided\"") => {
                if call_id == "call_0" {
                    990
                } else if *call_id == format!("call_{LATE_HEAVY_CALL}") {
                    700
                } else {
                    1
                }
            }
            _ => 1,
        }
    }

    fn tool_definition_weight(&self, _tool: &ToolDefinition) -> u64 {
        0
    }
}

#[tokio::test]
async fn requests_that_only_re_elide_the_prefix_do_not_spend_the_elision_cap() {
    // Arrange
    let model_calls = MAX_ELISION_EXTENSIONS + 40;
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    let last_request = Arc::new(Mutex::new(None));
    let captured_request = Arc::clone(&last_request);
    model
        .expect_complete()
        .times(model_calls)
        .returning(move |request| {
            let index = call_count.fetch_add(1, Ordering::SeqCst);
            if index + 1 == model_calls {
                *captured_request.lock().expect("request lock") = Some(request);

                return Ok(response_without_metadata(ModelResponse::Output(json!({
                    "summary": "done"
                }))));
            }

            Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call(&format!("call_{index}")),
            )))
        });
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("budgeted", "1").expect("identity"),
            model,
            ModelCapabilities {
                context_budget: ContextBudget::new(NonZeroU64::new(1_000).expect("nonzero budget")),
                image_input: false,
                tool_calls: true,
            },
        )
        .expect("register");
    let harness = Harness::from_registry(&registry, "budgeted")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(file_system_reading_lines_of(1))
        .context_estimator(HeavyResultsEstimator);

    // Act
    let outcome = harness
        .run_once("keep inspecting small files", object_schema())
        .await;

    // Assert
    // The sixth request extends elision over `call_0`, and more than
    // `MAX_ELISION_EXTENSIONS` later requests fit by re-eliding that prefix.
    // Once the replayed late heavy result outgrows the budget, a second
    // extension is still allowed because re-elision spent none of the cap.
    let outcome = outcome.expect("prefix re-elision keeps the turn within budget");
    assert_eq!(outcome.output(), &json!({ "summary": "done" }));
    let last_request = last_request
        .lock()
        .expect("request lock")
        .take()
        .expect("final request captured");
    for heavy_call_id in ["call_0".to_string(), format!("call_{LATE_HEAVY_CALL}")] {
        let heavy_result = last_request
            .messages()
            .iter()
            .find_map(|message| match message {
                ModelMessage::ToolResult {
                    call_id, content, ..
                } if *call_id == heavy_call_id => Some(content.clone()),
                _ => None,
            })
            .expect("heavy result replayed");
        assert!(heavy_result.contains("\"elided\""), "{heavy_call_id}");
    }
}

#[tokio::test]
async fn sessions_replay_extend_and_compact_a_turn_completed_with_elision() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    let projected_request = Arc::new(Mutex::new(None));
    let grown_request = Arc::new(Mutex::new(None));
    let generation_prompt = Arc::new(Mutex::new(None));
    let captured_projection = Arc::clone(&projected_request);
    let captured_growth = Arc::clone(&grown_request);
    let captured_generation = Arc::clone(&generation_prompt);
    model.expect_complete().times(6).returning(move |request| {
        match call_count.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_one"),
            ))),
            1 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_two"),
            ))),
            2 => Ok(response_without_metadata(ModelResponse::Output(
                json!({ "summary": "first answer" }),
            ))),
            3 => {
                *captured_projection.lock().expect("request lock") =
                    Some(request.messages().to_vec());

                Ok(response_without_metadata(ModelResponse::ToolCall(
                    read_call("call_three"),
                )))
            }
            4 => {
                *captured_growth.lock().expect("request lock") = Some(request.messages().to_vec());

                Ok(response_without_metadata(ModelResponse::Output(
                    json!({ "summary": "second answer" }),
                )))
            }
            _ => {
                *captured_generation.lock().expect("prompt lock") =
                    Some(request.prompt().to_string());

                Ok(compacted_summary())
            }
        }
    });
    let directory = tempdir().expect("temporary directory should be created");
    let mut session = elision_harness(model)
        .database(directory.path().join("harness.db"))
        .session("elided", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    // Each read result weighs about 10,000 of 16,000, so the completed first
    // turn outweighs every later request and its source for compaction.
    session
        .send("inspect the manifest twice")
        .await
        .expect("elided turn should complete");
    let follow_up = session
        .send("follow up")
        .await
        .expect("follow-up turn should complete after its own read");
    session
        .compact()
        .await
        .expect("compaction should succeed")
        .expect("checkpoint should be published");

    // Assert
    assert_eq!(follow_up.report().history().replayed_turns(), 1);
    let projected = projected_request
        .lock()
        .expect("request lock")
        .take()
        .expect("follow-up request captured");
    assert!(projected.contains(&ModelMessage::User(
        "inspect the manifest twice".to_string()
    )));
    assert!(projected.iter().any(|message| matches!(
        message,
        ModelMessage::Assistant(content) if content.contains("first answer")
    )));
    assert!(has_elided_result(&projected, "call_one"));
    assert!(has_full_result(&projected, "call_two"));
    // The follow-up's own read grows the request past the budget, so the
    // replayed history result is elided ahead of the new one.
    let grown = grown_request
        .lock()
        .expect("request lock")
        .take()
        .expect("grown request captured");
    assert!(has_elided_result(&grown, "call_two"));
    assert!(has_full_result(&grown, "call_three"));
    let prompt = generation_prompt
        .lock()
        .expect("prompt lock")
        .take()
        .expect("generation prompt captured");
    assert!(prompt.contains("inspect the manifest twice"));
    assert!(prompt.contains("first answer"));
    assert!(prompt.contains("follow up"));
    assert!(prompt.contains("second answer"));
    // Only the follow-up's read result fits the compaction source; the first
    // turn is rendered as its input and final output.
    assert_eq!(prompt.matches(&"x".repeat(40_000)).count(), 1);
}

#[tokio::test]
async fn sessions_replay_a_turn_beyond_the_history_byte_limit_as_its_input_and_output() {
    // Arrange
    let mut model = model();
    let call_count = Arc::new(AtomicUsize::new(0));
    let follow_up_request = Arc::new(Mutex::new(None));
    let generation_prompt = Arc::new(Mutex::new(None));
    let captured_follow_up = Arc::clone(&follow_up_request);
    let captured_generation = Arc::clone(&generation_prompt);
    model.expect_complete().times(6).returning(move |request| {
        match call_count.fetch_add(1, Ordering::SeqCst) {
            0 => Ok(response_without_metadata(ModelResponse::Output(
                json!({ "summary": "greeting" }),
            ))),
            1 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_one"),
            ))),
            2 => Ok(response_without_metadata(ModelResponse::ToolCall(
                read_call("call_two"),
            ))),
            3 => Ok(response_without_metadata(ModelResponse::Output(
                json!({ "summary": "first answer" }),
            ))),
            4 => {
                *captured_follow_up.lock().expect("request lock") =
                    Some(request.messages().to_vec());

                Ok(response_without_metadata(ModelResponse::Output(
                    json!({ "summary": "second answer" }),
                )))
            }
            _ => {
                *captured_generation.lock().expect("prompt lock") =
                    Some(request.prompt().to_string());

                Ok(compacted_summary())
            }
        }
    });
    let directory = tempdir().expect("temporary directory should be created");
    // The elided turn retains about 80,000 bytes, beyond the 60,000-byte
    // history window, while its input and final output fit.
    let mut session = elision_harness(model)
        .database(directory.path().join("harness.db"))
        .max_history_bytes(NonZeroUsize::new(60_000).expect("nonzero history limit"))
        .session("elided", object_schema())
        .create()
        .await
        .expect("session should be created");

    // Act
    session
        .send("hello")
        .await
        .expect("first turn should complete");
    session
        .send("inspect the manifest twice")
        .await
        .expect("elided turn should complete");
    let follow_up = session
        .send("follow up")
        .await
        .expect("follow-up turn should complete");
    session
        .compact()
        .await
        .expect("compaction should succeed")
        .expect("checkpoint should be published");

    // Assert
    assert_eq!(follow_up.report().history().replayed_turns(), 2);
    let messages = follow_up_request
        .lock()
        .expect("request lock")
        .take()
        .expect("follow-up request captured");
    assert_eq!(
        messages,
        [
            ModelMessage::User("hello".to_string()),
            ModelMessage::Assistant(r#"{"summary":"greeting"}"#.to_string()),
            ModelMessage::User("inspect the manifest twice".to_string()),
            ModelMessage::Assistant(r#"{"summary":"first answer"}"#.to_string()),
            ModelMessage::User("follow up".to_string()),
        ]
    );
    let prompt = generation_prompt
        .lock()
        .expect("prompt lock")
        .take()
        .expect("generation prompt captured");
    for content in [
        "hello",
        "inspect the manifest twice",
        "first answer",
        "follow up",
    ] {
        assert!(
            prompt.contains(content),
            "compaction source omits {content}"
        );
    }
    assert!(!prompt.contains(&"x".repeat(40_000)));
}

/// Builds a read-enabled harness with a 16,000 budget whose manifest reads
/// each return one 40,000-byte line.
fn elision_harness(model: MockModel) -> Harness {
    Harness::new(
        model,
        ContextBudget::new(NonZeroU64::new(16_000).expect("nonzero budget")),
    )
    .repository(Repository::fixture("repo"))
    .allow(Tool::Read)
    .file_system(file_system_reading_lines_of(40_000))
}

fn compacted_summary() -> ModelCompletion {
    response_without_metadata(ModelResponse::Output(json!({
        "context": "compacted",
        "decisions": [],
        "state": "done"
    })))
}

fn has_elided_result(messages: &[ModelMessage], call: &str) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::ToolResult { call_id, content, .. }
                if call_id == call && content.contains("\"elided\"")
        )
    })
}

fn has_full_result(messages: &[ModelMessage], call: &str) -> bool {
    messages.iter().any(|message| {
        matches!(
            message,
            ModelMessage::ToolResult { call_id, content, .. }
                if call_id == call && content.contains(&"x".repeat(40_000))
        )
    })
}
