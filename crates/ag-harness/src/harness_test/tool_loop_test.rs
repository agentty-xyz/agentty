use std::io::Cursor;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::support::{model, object_schema, response_without_metadata, write_call};
use crate::context::ContextBudget;
use crate::context_budget_fixture::unbounded_context_budget;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::model::{ModelError, ModelMessage, ModelRequest, ModelResponse};
use crate::model_registry::{ModelCapabilities, ModelRegistry};
use crate::recovery::ExecutionIdentity;
use crate::repository::Repository;
use crate::tool::{ReadAction, Tool, ToolCall};
use crate::turn::ToolActivity;

/// Scripts the model's responses in order and records every request it
/// received.
fn scripted(
    responses: Vec<Result<ModelResponse, ModelError>>,
) -> (crate::model::MockModel, Arc<Mutex<Vec<ModelRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let responses = Mutex::new(responses.into_iter());
    let mut model = model();
    model.expect_complete().returning(move |request| {
        recorded.lock().expect("requests lock").push(request);
        responses
            .lock()
            .expect("responses lock")
            .next()
            .expect("scripted response")
            .map(response_without_metadata)
    });

    (model, requests)
}

/// Serves `content` for every file and counts how often one was opened.
fn counting_file_system(content: Vec<u8>, opened: &Arc<AtomicUsize>) -> MockFileSystem {
    let opened = Arc::clone(opened);
    let mut file_system = MockFileSystem::new();
    file_system
        .expect_canonicalize()
        .returning(|path| Ok(path.to_path_buf()));
    file_system.expect_open_beneath().returning(move |_, _| {
        opened.fetch_add(1, Ordering::SeqCst);

        Ok(Box::new(Cursor::new(content.clone())))
    });

    file_system
}

fn file_content() -> Vec<u8> {
    (0..80)
        .flat_map(|line| format!("line {line} of the notes\n").into_bytes())
        .collect()
}

fn read_of(id: &str, arguments: &Value) -> ToolCall {
    ToolCall::from_json(id.to_string(), "read", &arguments.to_string(), None).expect("read call")
}

fn output() -> ModelResponse {
    ModelResponse::Output(json!({"summary": "done"}))
}

fn invalid_arguments() -> Result<ModelResponse, ModelError> {
    Err(ModelError::InvalidToolArguments {
        reason: "`read`: unknown field `path src/`".to_string(),
    })
}

#[tokio::test]
async fn identical_reads_are_answered_by_pointing_to_the_earlier_result() {
    // Arrange
    let (model, requests) = scripted(vec![
        Ok(ModelResponse::ToolCall(read_of(
            "read_0",
            &json!({"path": "notes.txt"}),
        ))),
        Ok(ModelResponse::ToolCall(read_of(
            "read_1",
            &json!({"path": "notes.txt"}),
        ))),
        Ok(ModelResponse::ToolCalls(vec![
            read_of("read_2", &json!({"path": "notes.txt"})),
            read_of("read_3", &json!({"path": "other.txt"})),
        ])),
        Ok(output()),
    ]);
    let opened = Arc::new(AtomicUsize::new(0));
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(counting_file_system(
            b"line one of a longer manifest\n".repeat(20),
            &opened,
        ));

    // Act
    let outcome = harness
        .run_once("read the notes", object_schema())
        .await
        .expect("turn succeeds");

    // Assert
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    let activities = outcome.report().tool_calls();
    assert!(matches!(activities[0], ToolActivity::Read { .. }));
    assert!(matches!(
        &activities[1],
        ToolActivity::ReadRepeated { action: ReadAction::File, summary, .. }
            if summary == "notes.txt"
    ));
    assert!(matches!(activities[2], ToolActivity::ReadRepeated { .. }));
    assert!(matches!(activities[3], ToolActivity::Read { .. }));
    let requests = requests.lock().expect("requests lock");
    let ModelMessage::ToolResult { content, .. } = requests[2].messages().last().expect("result")
    else {
        std::panic::resume_unwind(Box::new("tool result expected"));
    };
    let repeated: Value = serde_json::from_str(content).expect("JSON result");
    assert_eq!(repeated["unchanged"], json!(true));
    assert_eq!(repeated["same_as_call"], json!("read_0"));
}

#[tokio::test]
async fn a_write_between_identical_reads_runs_the_read_again() {
    // Arrange
    let (model, _requests) = scripted(vec![
        Ok(ModelResponse::ToolCall(read_of(
            "read_0",
            &json!({"path": "src/lib.rs"}),
        ))),
        Ok(ModelResponse::ToolCall(write_call(
            "write_1",
            "not a unified diff",
        ))),
        Ok(ModelResponse::ToolCall(read_of(
            "read_2",
            &json!({"path": "src/lib.rs"}),
        ))),
        Ok(output()),
    ]);
    let opened = Arc::new(AtomicUsize::new(0));
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .allow(Tool::Write)
        .file_system(counting_file_system(
            b"line one of a longer manifest\n".repeat(20),
            &opened,
        ));

    // Act
    let outcome = harness
        .run_once("read, edit, and read again", object_schema())
        .await
        .expect("turn succeeds");

    // Assert
    let activities = outcome.report().tool_calls();
    assert!(matches!(activities[1], ToolActivity::WriteRejected { .. }));
    assert!(matches!(activities[2], ToolActivity::Read { .. }));
    assert_eq!(opened.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn superseded_reads_are_cleared_first_and_reported() {
    // Arrange
    let (model, requests) = scripted(vec![
        Ok(ModelResponse::ToolCall(read_of(
            "first_lines",
            &json!({"path": "notes.txt", "limit": 70}),
        ))),
        Ok(ModelResponse::ToolCall(read_of(
            "other",
            &json!({"path": "other.txt", "limit": 5}),
        ))),
        Ok(ModelResponse::ToolCall(read_of(
            "all_lines",
            &json!({"path": "notes.txt"}),
        ))),
        Ok(ModelResponse::ToolCall(read_of(
            "latest",
            &json!({"path": "latest.txt", "limit": 5}),
        ))),
        Ok(output()),
    ]);
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("budgeted", "1").expect("identity"),
            model,
            ModelCapabilities {
                context_budget: ContextBudget::new(NonZeroU64::new(1_350).expect("budget")),
                image_input: false,
                tool_calls: true,
            },
        )
        .expect("register");
    let opened = Arc::new(AtomicUsize::new(0));
    let harness = Harness::from_registry(&registry, "budgeted")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(counting_file_system(file_content(), &opened));

    // Act
    let outcome = harness
        .run_once("read the notes", object_schema())
        .await
        .expect("turn fits by clearing the superseded read");

    // Assert
    let last = outcome
        .report()
        .model_requests()
        .last()
        .expect("final request")
        .context();
    assert_eq!(last.superseded_results(), 1);
    let requests = requests.lock().expect("requests lock");
    let ModelMessage::ToolResult { content, .. } = &requests[4].messages()[2] else {
        std::panic::resume_unwind(Box::new("first read result expected"));
    };
    let notice: Value = serde_json::from_str(content).expect("notice");
    assert_eq!(notice["superseded_by"], json!("all_lines"));
}

#[tokio::test]
async fn invalid_tool_arguments_are_corrected_and_the_limit_counts_consecutive_responses() {
    // Arrange
    let (model, requests) = scripted(vec![
        invalid_arguments(),
        invalid_arguments(),
        Ok(ModelResponse::ToolCall(read_of(
            "read_0",
            &json!({"path": "notes.txt"}),
        ))),
        invalid_arguments(),
        invalid_arguments(),
        Ok(output()),
    ]);
    let opened = Arc::new(AtomicUsize::new(0));
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(counting_file_system(b"notes\n".to_vec(), &opened));

    // Act
    let outcome = harness
        .run_once("read the notes", object_schema())
        .await
        .expect("each correction stays below the consecutive limit");

    // Assert
    assert_eq!(outcome.report().tool_argument_corrections(), 4);
    assert_eq!(outcome.report().tool_calls().len(), 1);
    assert_eq!(outcome.report().model_requests().len(), 2);
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 6);
    let ModelMessage::User(feedback) = requests[1].messages().last().expect("feedback") else {
        std::panic::resume_unwind(Box::new("feedback expected"));
    };
    assert!(feedback.contains("`read`: unknown field `path src/`"));
    assert!(feedback.contains("`read` takes `action`, `limit`, `offset`, `path`, `query`, `side`"));
    assert_eq!(requests[2].messages().len(), 3);
}

#[tokio::test]
async fn invalid_tool_argument_feedback_marks_required_fields() {
    // Arrange
    let (model, requests) = scripted(vec![invalid_arguments(), Ok(output())]);
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Write)
        .file_system(MockFileSystem::new());

    // Act
    harness
        .run_once("edit", object_schema())
        .await
        .expect("turn succeeds after the correction");

    // Assert
    let requests = requests.lock().expect("requests lock");
    let ModelMessage::User(feedback) = requests[1].messages().last().expect("feedback") else {
        std::panic::resume_unwind(Box::new("feedback expected"));
    };
    assert!(feedback.contains("`write` takes `patch` (required), `path` (required)"));
}
