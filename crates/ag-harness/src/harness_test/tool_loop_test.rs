use std::io::Cursor;
use std::num::NonZeroU64;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};

use super::support::{model, object_schema, response_without_metadata, write_call};
use crate::context::{ContextBudget, ContextEstimator, HeuristicContextEstimator};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::file_system::MockFileSystem;
use crate::harness::Harness;
use crate::lifecycle::{LifecycleEvent, LifecycleEventKind, TurnErrorType};
use crate::model::{ModelError, ModelMessage, ModelRequest, ModelResponse};
use crate::model_profile::ModelProfile;
use crate::model_registry::{ModelCapabilities, ModelRegistry};
use crate::recovery::ExecutionIdentity;
use crate::repository::Repository;
use crate::session::SessionError;
use crate::store::MemoryStore;
use crate::tool::{ReadAction, Tool, ToolCall};
use crate::turn::{ToolActivity, TurnError, TurnOutcome};

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

/// A `read` call whose arguments are cut off mid-object.
const MALFORMED_READ: &str = r#"{"path src/": "notes.txt""#;

fn malformed(id: &str, name: &str, raw: &str) -> ToolCall {
    ToolCall::from_json(id.to_string(), name, raw, None).expect("malformed call")
}

fn malformed_read(id: &str) -> ModelResponse {
    ModelResponse::ToolCall(malformed(id, "read", MALFORMED_READ))
}

/// Returns the JSON content of the tool result answering `call_id`.
fn result_for(request: &ModelRequest, call_id: &str) -> Value {
    request
        .messages()
        .iter()
        .find_map(|message| match message {
            ModelMessage::ToolResult {
                call_id: id,
                content,
                ..
            } if id == call_id => Some(serde_json::from_str(content).expect("JSON result")),
            _ => None,
        })
        .expect("tool result")
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
                image_input: false,
                profile: ModelProfile::new(1_350),
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
async fn malformed_calls_get_error_results_while_their_siblings_run() {
    // Arrange
    let (model, requests) = scripted(vec![
        Ok(ModelResponse::ToolCalls(vec![
            malformed("bad_read", "read", MALFORMED_READ),
            read_of("good_read", &json!({"path": "notes.txt"})),
        ])),
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
        .expect("the turn continues past the malformed call");

    // Assert
    assert_eq!(outcome.report().malformed_tool_calls(), 1);
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    let activities = outcome.report().tool_calls();
    assert!(matches!(
        &activities[0],
        ToolActivity::MalformedArguments { tool: Tool::Read, reason, .. }
            if reason.contains("unknown field `path src/`")
    ));
    assert_eq!((activities[0].name(), activities[0].path()), ("read", ""));
    assert!(
        activities[0]
            .to_string()
            .starts_with("read (malformed arguments: ")
    );
    assert!(matches!(activities[1], ToolActivity::Read { .. }));
    let requests = requests.lock().expect("requests lock");
    let ModelMessage::AssistantToolCalls(calls) = &requests[1].messages()[1] else {
        std::panic::resume_unwind(Box::new("batched calls expected"));
    };
    let bad = calls[0].malformed_arguments().expect("kept malformed");
    assert_eq!((bad.raw(), bad.tool()), (MALFORMED_READ, Tool::Read));
    assert_eq!(calls[0].arguments_json().expect("raw"), MALFORMED_READ);
    let result = result_for(&requests[1], "bad_read");
    assert_eq!(result["status"], json!("rejected"));
    assert!(
        result["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("failed to parse function arguments: "))
    );
    assert!(result["hint"].as_str().is_some_and(|hint| hint.contains(
        "Call `read` again with valid JSON arguments using only its documented fields: `action`, \
         `limit`, `offset`, `path`, `query`, `side`."
    )));
}

#[tokio::test]
async fn only_consecutive_all_malformed_responses_count_toward_the_limit() {
    // Arrange
    let (model, requests) = scripted(vec![
        Ok(malformed_read("bad_0")),
        Ok(malformed_read("bad_1")),
        Ok(ModelResponse::ToolCall(read_of(
            "read_0",
            &json!({"path": "notes.txt"}),
        ))),
        Ok(malformed_read("bad_2")),
        Ok(malformed_read("bad_3")),
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
        .expect("a decodable call resets the consecutive count");

    // Assert
    assert_eq!(outcome.report().malformed_tool_calls(), 4);
    assert_eq!(outcome.report().tool_calls().len(), 5);
    assert_eq!(requests.lock().expect("requests lock").len(), 6);
}

#[tokio::test]
async fn consecutive_all_malformed_responses_fail_the_turn_at_the_limit() {
    // Arrange
    let (model, requests) = scripted(vec![
        Ok(malformed_read("bad_0")),
        Ok(malformed_read("bad_1")),
        Ok(malformed_read("bad_2")),
    ]);
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(MockFileSystem::new());

    // Act
    let error = harness
        .run_once("read the notes", object_schema())
        .await
        .expect_err("the third malformed-only response fails the turn");

    // Assert
    assert!(matches!(
        error,
        crate::TurnError::Model(ModelError::InvalidToolArguments { ref reason })
            if reason.starts_with("`read`: unknown field `path src/`")
    ));
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 3);
    assert_eq!(
        result_for(&requests[2], "bad_1")["status"],
        json!("rejected")
    );
}

#[tokio::test]
async fn malformed_call_results_mark_required_fields() {
    // Arrange
    let (model, requests) = scripted(vec![
        Ok(ModelResponse::ToolCall(malformed(
            "bad_write",
            "write",
            r#"{"path": "notes.txt"}"#,
        ))),
        Ok(output()),
    ]);
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Write)
        .file_system(MockFileSystem::new());

    // Act
    harness
        .run_once("edit", object_schema())
        .await
        .expect("turn succeeds after the error result");

    // Assert
    let requests = requests.lock().expect("requests lock");
    let result = result_for(&requests[1], "bad_write");
    assert!(
        result["hint"]
            .as_str()
            .is_some_and(|hint| hint.contains("`patch` (required), `path` (required)"))
    );
}

#[tokio::test]
async fn malformed_calls_to_disallowed_tools_are_denied() {
    // Arrange
    let (model, _requests) = scripted(vec![Ok(ModelResponse::ToolCall(malformed(
        "bad_bash", "bash", "{",
    )))]);
    let harness = Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(MockFileSystem::new());

    // Act
    let error = harness
        .run_once("run something", object_schema())
        .await
        .expect_err("bash is not allowed");

    // Assert
    assert!(matches!(error, crate::TurnError::ToolDenied { ref name } if name == "bash"));
}

/// Responds with the same `read` call `times` times, each with its own call
/// identifier, then with the final output.
fn repeated_reads(times: usize) -> Vec<Result<ModelResponse, ModelError>> {
    (0..times)
        .map(|index| {
            Ok(ModelResponse::ToolCall(read_of(
                &format!("read_{index}"),
                &json!({"path": "notes.txt"}),
            )))
        })
        .chain([Ok(output())])
        .collect()
}

/// Returns the `[harness]` user messages a request carries after its input.
fn harness_feedback(request: &ModelRequest) -> Vec<String> {
    request
        .messages()
        .iter()
        .skip(request.input_index() + 1)
        .filter_map(|message| match message {
            ModelMessage::User(text) if text.starts_with("[harness]") => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// Returns the repetition events among `events` as `(repeats, stopped,
/// tool)`.
fn repetition_events(events: &Mutex<Vec<LifecycleEvent>>) -> Vec<(usize, bool, String)> {
    events
        .lock()
        .expect("events lock")
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::ToolCallsRepeated {
                repeats,
                stopped,
                tool_name,
                ..
            } => Some((*repeats, *stopped, tool_name.clone())),
            _ => None,
        })
        .collect()
}

fn repetition_harness(
    model: crate::model::MockModel,
    events: &Arc<Mutex<Vec<LifecycleEvent>>>,
) -> Harness {
    let recorded = Arc::clone(events);
    let opened = Arc::new(AtomicUsize::new(0));

    Harness::new(model, unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .allow(Tool::Write)
        .file_system(counting_file_system(
            b"line one of a longer manifest\n".repeat(20),
            &opened,
        ))
        .with_lifecycle_observer(move |event: LifecycleEvent| {
            recorded.lock().expect("events lock").push(event);
        })
}

#[tokio::test]
async fn a_third_identical_call_nudges_the_model_once() {
    // Arrange
    let (model, requests) = scripted(repeated_reads(4));
    let events = Arc::new(Mutex::new(Vec::new()));
    let harness = repetition_harness(model, &events);

    // Act
    let outcome = harness
        .run_once("read the notes", object_schema())
        .await
        .expect("turn succeeds");

    // Assert
    assert_eq!(outcome.report().repetition_nudges(), 1);
    let requests = requests.lock().expect("requests lock");
    assert_eq!(harness_feedback(&requests[2]), Vec::<String>::new());
    let nudges = harness_feedback(&requests[3]);
    assert_eq!(nudges.len(), 1);
    assert!(nudges[0].starts_with("[harness] You have called `read` 3 times"));
    assert_eq!(harness_feedback(&requests[4]), nudges);
    assert_eq!(repetition_events(&events), [(3, false, "read".to_string())]);
}

#[tokio::test]
async fn a_fifth_identical_call_fails_the_turn() {
    // Arrange
    let (model, requests) = scripted(repeated_reads(5));
    let events = Arc::new(Mutex::new(Vec::new()));
    let harness = repetition_harness(model, &events);

    // Act
    let error = harness
        .run_once("read the notes", object_schema())
        .await
        .expect_err("the fifth identical call fails the turn");

    // Assert
    assert!(matches!(
        &error,
        crate::TurnError::RepeatedToolCalls { repeats: 5, tool } if tool == "read"
    ));
    assert_eq!(error.error_type(), TurnErrorType::RepeatedToolCalls);
    assert_eq!(error.error_type().as_str(), "repeated_tool_calls");
    assert_eq!(requests.lock().expect("requests lock").len(), 5);
    assert_eq!(
        repetition_events(&events),
        [
            (3, false, "read".to_string()),
            (5, true, "read".to_string())
        ]
    );
}

#[tokio::test]
async fn a_differing_write_restarts_the_repetition_count() {
    // Arrange
    let read = || read_of("read", &json!({"path": "notes.txt"}));
    let (model, requests) = scripted(vec![
        Ok(ModelResponse::ToolCalls(vec![
            read_of("read_0", &json!({"path": "notes.txt"})),
            read_of("read_1", &json!({"path": "notes.txt"})),
        ])),
        Ok(ModelResponse::ToolCall(write_call("write_2", "not a diff"))),
        Ok(ModelResponse::ToolCall(
            ToolCall::from_json(
                "read_3".to_string(),
                "read",
                &json!({"path": "notes.txt"}).to_string(),
                None,
            )
            .expect("read call"),
        )),
        Ok(ModelResponse::ToolCall(write_call(
            "write_4",
            "still not a diff",
        ))),
        Ok(ModelResponse::ToolCall(read())),
        Ok(output()),
    ]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let harness = repetition_harness(model, &events);

    // Act
    let outcome = harness
        .run_once("read and edit the notes", object_schema())
        .await
        .expect("turn succeeds");

    // Assert
    assert_eq!(outcome.report().repetition_nudges(), 0);
    assert_eq!(repetition_events(&events), []);
    let requests = requests.lock().expect("requests lock");
    assert!(
        requests
            .iter()
            .all(|request| harness_feedback(request).is_empty())
    );
}

/// A provider rejection of a request whose input exceeds the context window.
fn overflow() -> ModelError {
    ModelError::context_overflow(std::io::Error::other("exceeded model token limit"))
}

/// Returns the heuristic weight of `request` without reserved output, as
/// the engine estimates a sent request.
fn sent_weight(request: &ModelRequest) -> u64 {
    let estimator = HeuristicContextEstimator::DEFAULT;
    let messages: u64 = request
        .messages()
        .iter()
        .map(|message| estimator.message_weight(message))
        .sum();
    let tools: u64 = request
        .tools()
        .iter()
        .map(|tool| estimator.tool_definition_weight(tool))
        .sum();

    messages + tools
}

/// Reads `notes.txt` with distinct windows so no read repeats another.
fn reads(count: usize) -> Vec<Result<ModelResponse, ModelError>> {
    (0..count)
        .map(|index| {
            Ok(ModelResponse::ToolCall(read_of(
                &format!("read_{index}"),
                &json!({"path": "notes.txt", "offset": index + 1, "limit": 70}),
            )))
        })
        .collect()
}

fn overflow_harness(model: crate::model::MockModel, budget: u64) -> Harness {
    Harness::new(
        model,
        ContextBudget::new(NonZeroU64::new(budget).expect("budget")),
    )
    .repository(Repository::fixture("repo"))
    .allow(Tool::Read)
    .file_system(counting_file_system(
        file_content(),
        &Arc::new(AtomicUsize::new(0)),
    ))
}

/// Runs seventy-two distinct small reads and a final output against a
/// provider whose real tokens are `density_percent` of the estimate and which
/// rejects any request above `window_tokens - max_output_tokens`, under a
/// profile whose margin tolerates a 16% under-count. Returns the outcome, the
/// simulated tokens of every accepted request, and the provider's limit.
async fn run_against_dense_provider(
    density_percent: u64,
) -> (Result<TurnOutcome, TurnError>, Vec<u64>, u64) {
    let window_tokens = 6_000;
    let max_output_tokens = 1_000;
    let input_limit = window_tokens - max_output_tokens;
    let mut responses = (0..72)
        .map(|index| {
            Ok(ModelResponse::ToolCall(read_of(
                &format!("read_{index}"),
                &json!({"path": "notes.txt", "offset": index + 1, "limit": 8}),
            )))
        })
        .collect::<Vec<_>>();
    responses.push(Ok(output()));
    let responses = Mutex::new(responses.into_iter());
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&accepted);
    let mut model = model();
    model.expect_complete().returning(move |request| {
        let tokens = sent_weight(&request) * density_percent / 100;
        if tokens > input_limit {
            return Err(overflow());
        }
        recorded.lock().expect("accepted lock").push(tokens);
        responses
            .lock()
            .expect("responses lock")
            .next()
            .expect("scripted response")
            .map(response_without_metadata)
    });
    let mut registry = ModelRegistry::new();
    registry
        .register(
            ExecutionIdentity::new("dense", "1").expect("identity"),
            model,
            ModelCapabilities {
                image_input: false,
                profile: ModelProfile::new(window_tokens)
                    .with_estimate_margin(max_output_tokens, 16),
                tool_calls: true,
            },
        )
        .expect("register");
    let harness = Harness::from_registry(&registry, "dense")
        .expect("harness")
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .file_system(counting_file_system(
            file_content(),
            &Arc::new(AtomicUsize::new(0)),
        ));

    let outcome = harness.run_once("read the notes", object_schema()).await;
    let accepted = accepted.lock().expect("accepted lock").clone();

    (outcome, accepted, input_limit)
}

#[tokio::test]
async fn content_within_the_declared_under_count_fits_the_window_without_recovery() {
    // Arrange
    let density_percent = 116;

    // Act
    let (outcome, accepted, input_limit) = run_against_dense_provider(density_percent).await;

    // Assert
    let outcome = outcome.expect("the margin keeps every request within the window");
    assert_eq!(outcome.report().context_overflow_recoveries(), 0);
    assert_eq!(accepted.len(), 73);
    assert!(accepted.iter().any(|tokens| *tokens > input_limit * 9 / 10));
}

#[tokio::test]
async fn content_denser_than_the_declared_under_count_fits_after_one_recovery() {
    // Arrange
    let density_percent = 146;

    // Act
    let (outcome, accepted, input_limit) = run_against_dense_provider(density_percent).await;

    // Assert
    let outcome = outcome.expect("one recovery fits content 46% over the estimate");
    assert_eq!(outcome.report().context_overflow_recoveries(), 1);
    assert_eq!(accepted.len(), 73);
    assert!(accepted.iter().all(|tokens| *tokens <= input_limit));
}

#[tokio::test]
async fn an_overflowed_request_is_resent_reduced_and_the_turn_finishes() {
    // Arrange
    let mut responses = reads(3);
    responses.extend([Err(overflow()), Ok(output())]);
    let (model, requests) = scripted(responses);
    let events = Arc::new(Mutex::new(Vec::new()));
    let observed = Arc::clone(&events);
    let harness =
        overflow_harness(model, 20_000).with_lifecycle_observer(move |event: LifecycleEvent| {
            observed.lock().expect("events lock").push(event);
        });

    // Act
    let outcome = harness
        .run_once("read the notes", object_schema())
        .await
        .expect("the reduced request fits the provider window");

    // Assert
    assert_eq!(outcome.report().context_overflow_recoveries(), 1);
    assert_eq!(outcome.report().model_requests().len(), 4);
    let requests = requests.lock().expect("requests lock");
    let rejected = sent_weight(&requests[3]);
    let retried = sent_weight(&requests[4]);
    assert!(retried <= rejected * 3 / 4, "{retried} of {rejected}");
    let overflow_retries = events
        .lock()
        .expect("events lock")
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::ContextOverflowRetried {
                attempt,
                rejected_weight,
                retry_weight,
                ..
            } => Some((*attempt, *rejected_weight, *retry_weight)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(overflow_retries, [(1, rejected, retried)]);
}

#[tokio::test]
async fn overflow_recoveries_are_bounded_per_turn() {
    // Arrange
    let mut responses = reads(6);
    responses.extend([Err(overflow()), Err(overflow()), Err(overflow())]);
    let (model, requests) = scripted(responses);
    let harness = overflow_harness(model, 40_000);

    // Act
    let error = harness
        .run_once("read the notes", object_schema())
        .await
        .expect_err("the third rejection exceeds the recoveries");

    // Assert
    assert!(matches!(error, TurnError::Model(ref error) if error.is_context_overflow()));
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests.len(), 9);
    assert!(sent_weight(&requests[8]) < sent_weight(&requests[7]));
    assert!(sent_weight(&requests[7]) < sent_weight(&requests[6]));
}

#[tokio::test]
async fn an_overflow_the_refit_cannot_shrink_fails_with_the_provider_rejection() {
    // Arrange
    let (model, requests) = scripted(vec![Err(overflow())]);
    let harness = overflow_harness(model, 20_000);

    // Act
    let error = harness
        .run_once("read the notes", object_schema())
        .await
        .expect_err("mandatory content cannot shrink below the ceiling");

    // Assert
    assert!(matches!(error, TurnError::Model(ref error) if error.is_context_overflow()));
    assert_eq!(requests.lock().expect("requests lock").len(), 1);
}

#[tokio::test]
async fn an_overflow_on_the_first_request_of_a_turn_drops_history_and_lowers_later_turns() {
    // Arrange
    let mut responses = reads(4);
    responses.extend([Ok(output()), Err(overflow()), Ok(output()), Ok(output())]);
    let (model, requests) = scripted(responses);
    let harness = overflow_harness(model, 20_000).store(Arc::new(MemoryStore::new()));
    let mut session = harness
        .session("overflowing", object_schema())
        .create()
        .await
        .expect("session");
    session.send("read the notes").await.expect("first turn");

    // Act
    let recovered = session
        .send("summarize them")
        .await
        .expect("history is reduced below the ceiling");
    let later = session.send("anything else?").await.expect("third turn");
    let oversized = session
        .send("x".repeat(40_000).as_str())
        .await
        .expect_err("mandatory content above the ceiling fails typed");

    // Assert
    assert_eq!(recovered.report().context_overflow_recoveries(), 1);
    let requests = requests.lock().expect("requests lock");
    let rejected = sent_weight(&requests[5]);
    assert!(sent_weight(&requests[6]) <= rejected * 3 / 4);
    assert!(later.report().model_requests()[0].estimated_weight() <= rejected * 3 / 4);
    assert!(matches!(
        oversized,
        SessionError::Turn(TurnError::ContextBudgetExceeded { budget, required })
            if budget == 20_000 && required > budget
    ));
}

#[tokio::test]
async fn a_failed_turn_reports_the_requests_and_tool_calls_before_its_failure() {
    // Arrange
    let (model, _requests) = scripted(vec![
        Ok(ModelResponse::ToolCall(read_of(
            "read_0",
            &json!({"path": "notes.txt"}),
        ))),
        Err(ModelError::InvalidResponse),
    ]);
    let events = Arc::new(Mutex::new(Vec::new()));
    let harness = repetition_harness(model, &events);

    // Act
    let error = harness
        .run_once("read the notes", object_schema())
        .await
        .expect_err("the second request fails");

    // Assert
    assert!(matches!(
        error,
        TurnError::Model(ModelError::InvalidResponse)
    ));
    let events = events.lock().expect("events lock");
    let report = events
        .iter()
        .find_map(|event| match event.kind() {
            LifecycleEventKind::TurnFailed { report, .. } => report.clone(),
            _ => None,
        })
        .expect("the failure carries a report");
    assert_eq!(report.model_requests().len(), 1);
    assert_eq!(report.tool_calls().len(), 1);
}
