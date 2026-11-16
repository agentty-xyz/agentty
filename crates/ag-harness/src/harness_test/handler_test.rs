//! Tool handler unit tests: host handlers run through a harness turn beside
//! built-ins, and the built-in definitions sent to the model stay
//! byte-identical.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, oneshot};

use super::support::{model, object_schema, response_without_metadata};
use crate::bash::{BashConfig, Enforcement, UnsandboxedExecutor};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::error::ErrorKind;
use crate::harness::Harness;
use crate::model::{MockModel, ModelError, ModelMessage, ModelRequest, ModelResponse};
use crate::tool::{
    ToolCall, ToolContext, ToolDefinition, ToolEffects, ToolError, ToolHandler, ToolOutput, ToolSet,
};
use crate::turn::ToolActivity;
use crate::{Repository, RunRequest, Tool, ToolPolicy, TurnOptions};

/// SHA-256 of the built-in definitions advertised with every built-in
/// allowed, no comparison base, and an unsandboxed `bash`, serialized as
/// `[name, description, parameters]` triples in order. Recorded from the
/// closed-dispatch engine before built-ins became handlers.
const BUILTIN_DEFINITIONS_SHA256: &str =
    "240f8cf7dd7071a994fc68162e4d3b690484f0ce2c9ecf7334925abb8ad92d66";

/// How the [`Recorder`] handler answers a call.
#[derive(Clone, Copy)]
enum Answer {
    Echo,
    Reject,
    Fail,
    Oversized,
}

/// A host tool named `fetch` that answers as told and keeps every context
/// it was called with.
struct Recorder {
    answer: Answer,
    contexts: Mutex<Vec<ToolContext>>,
}

impl Recorder {
    fn new(answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            contexts: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl ToolHandler for Recorder {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            "fetch",
            "Fetch a URL.",
            json!({"type": "object", "properties": {"url": {"type": "string"}}}),
        )
        .expect("valid definition")
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::ReadOnly
    }

    async fn call(&self, arguments: Value, context: ToolContext) -> Result<ToolOutput, ToolError> {
        self.contexts.lock().expect("contexts lock").push(context);
        match self.answer {
            Answer::Echo => Ok(ToolOutput::new(json!({"fetched": arguments}).to_string())),
            Answer::Reject => Err(ToolError::rejected("url is required")),
            Answer::Fail => Err(ToolError::failed("connection refused")),
            Answer::Oversized => Ok(ToolOutput::new("x".repeat(64 * 1024 + 1))),
        }
    }
}

/// Scripts the model's responses in order and records every request.
fn scripted(responses: Vec<ModelResponse>) -> (MockModel, Arc<Mutex<Vec<ModelRequest>>>) {
    let requests = Arc::new(Mutex::new(Vec::new()));
    let recorded = Arc::clone(&requests);
    let responses = Mutex::new(responses.into_iter());
    let mut model = model();
    model.expect_profile().return_const(None);
    model.expect_complete().returning(move |request| {
        recorded.lock().expect("requests lock").push(request);
        let response = responses
            .lock()
            .expect("responses lock")
            .next()
            .expect("scripted response");

        Ok::<_, ModelError>(response_without_metadata(response))
    });

    (model, requests)
}

fn fetch(id: &str, raw: &str) -> ToolCall {
    ToolCall::from_json(id.to_string(), "fetch", raw, None).expect("fetch call")
}

fn output() -> ModelResponse {
    ModelResponse::Output(json!({"summary": "done"}))
}

fn harness(model: MockModel, tools: ToolSet) -> Harness {
    Harness::builder()
        .model(Arc::new(model))
        .context_budget(unbounded_context_budget())
        .tools(tools)
        .build()
        .expect("harness")
}

fn tool_set(handler: Arc<dyn ToolHandler>) -> ToolSet {
    ToolSet::builder()
        .handler(handler)
        .build()
        .expect("tool set")
}

/// Returns the content of the tool result answering `call_id`.
fn result_for(request: &ModelRequest, call_id: &str) -> String {
    request
        .messages()
        .iter()
        .find_map(|message| match message {
            ModelMessage::ToolResult {
                call_id: id,
                content,
                ..
            } if id == call_id => Some(content.clone()),
            _ => None,
        })
        .expect("tool result")
}

#[tokio::test]
async fn a_host_handler_answers_its_calls_without_a_repository() {
    // Arrange
    let recorder = Recorder::new(Answer::Echo);
    let (model, requests) = scripted(vec![
        ModelResponse::ToolCall(fetch("fetch_0", r#"{"url":"https://example.com"}"#)),
        output(),
    ]);
    let harness = harness(model, tool_set(recorder.clone()));

    // Act
    let (mut events, handle) = harness
        .run(RunRequest::new("fetch it").output(object_schema()))
        .into_parts();
    let outcome = handle.outcome().await;
    let first_event = std::future::poll_fn(|poll_context| {
        std::pin::Pin::new(&mut events).poll_next(poll_context)
    })
    .await
    .expect("run event");

    // Assert
    outcome.result.as_ref().expect("run output");
    let requests = requests.lock().expect("requests lock");
    assert_eq!(requests[0].tools(), [recorder.definition()]);
    assert_eq!(
        result_for(&requests[1], "fetch_0"),
        r#"{"fetched":{"url":"https://example.com"}}"#
    );
    assert!(matches!(
        outcome.report.tool_calls(),
        [ToolActivity::Custom { rejected: false, tool, .. }] if tool == "fetch"
    ));
    let contexts = recorder.contexts.lock().expect("contexts lock");
    assert_eq!(contexts.len(), 1, "{contexts:?}");
    let context = &contexts[0];
    assert_eq!(context.call_id(), "fetch_0");
    assert!(context.session_id().is_some());
    assert_eq!(context.workspace(), None);
    assert_eq!(context.run_id(), Some(&first_event.run));
}

#[tokio::test]
async fn rejections_unparsable_arguments_and_oversized_results_are_answered_with_errors() {
    // Arrange
    let calls = vec![
        fetch("fetch_0", r#"{"url":"https://example.com"}"#),
        fetch("fetch_1", r#"{"url""#),
    ];
    let (model, requests) = scripted(vec![ModelResponse::ToolCalls(calls), output()]);
    let rejecting = harness(model, tool_set(Recorder::new(Answer::Reject)));
    let (oversized_model, oversized_requests) = scripted(vec![
        ModelResponse::ToolCall(fetch("fetch_2", "{}")),
        output(),
    ]);
    let oversized = harness(oversized_model, tool_set(Recorder::new(Answer::Oversized)));

    // Act
    let rejected = rejecting
        .run(RunRequest::new("fetch it").output(object_schema()))
        .await;
    let too_large = oversized
        .run(RunRequest::new("fetch it").output(object_schema()))
        .await;

    // Assert
    rejected.result.as_ref().expect("run output");
    too_large.result.as_ref().expect("run output");
    let requests = requests.lock().expect("requests lock");
    assert_eq!(
        result_for(&requests[1], "fetch_0"),
        r#"{"error":"url is required","status":"rejected"}"#
    );
    let unparsable: Value =
        serde_json::from_str(&result_for(&requests[1], "fetch_1")).expect("JSON result");
    assert_eq!(unparsable["status"], "rejected");
    assert!(
        unparsable["error"]
            .as_str()
            .is_some_and(|error| error.starts_with("failed to parse function arguments: ")),
        "{unparsable}"
    );
    let oversized_requests = oversized_requests.lock().expect("requests lock");
    assert_eq!(
        result_for(&oversized_requests[1], "fetch_2"),
        json!({
            "error": "the tool returned 65537 bytes, over the 65536-byte result limit",
            "status": "rejected",
        })
        .to_string()
    );
    for report in [&rejected.report, &too_large.report] {
        assert!(
            report
                .tool_calls()
                .iter()
                .all(|activity| matches!(activity, ToolActivity::Custom { rejected: true, .. })),
            "{:?}",
            report.tool_calls()
        );
    }
}

#[tokio::test]
async fn a_failed_handler_fails_the_run_as_a_tool_failure() {
    // Arrange
    let (model, _requests) = scripted(vec![ModelResponse::ToolCall(fetch("fetch_0", "{}"))]);
    let harness = harness(model, tool_set(Recorder::new(Answer::Fail)));

    // Act
    let outcome = harness
        .run(RunRequest::new("fetch it").output(object_schema()))
        .await;

    // Assert
    let error = outcome.result.expect_err("failed tool");
    assert_eq!(error.kind(), ErrorKind::ToolFailed);
    assert_eq!(error.to_string(), "tool `fetch` failed: connection refused");
}

/// A host tool that waits forever after handing its context's cancellation
/// to a task, which reports when the turn is cancelled.
struct Waiter {
    cancelled: Mutex<Option<oneshot::Sender<()>>>,
    started: Arc<Notify>,
}

#[async_trait]
impl ToolHandler for Waiter {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new("wait", "Wait.", json!({"type": "object"})).expect("definition")
    }

    fn effects(&self) -> ToolEffects {
        ToolEffects::Executes
    }

    async fn call(&self, _arguments: Value, context: ToolContext) -> Result<ToolOutput, ToolError> {
        let cancelled = self.cancelled.lock().expect("sender lock").take();
        tokio::spawn(async move {
            context.cancelled().await;
            if let Some(cancelled) = cancelled {
                let _ = cancelled.send(());
            }
        });
        self.started.notify_one();

        std::future::pending().await
    }
}

#[tokio::test]
async fn a_handler_learns_its_turn_was_cancelled() {
    // Arrange
    let (cancelled, cancelled_signal) = oneshot::channel();
    let started = Arc::new(Notify::new());
    let waiter = Arc::new(Waiter {
        cancelled: Mutex::new(Some(cancelled)),
        started: Arc::clone(&started),
    });
    let call = ToolCall::from_json("wait_0".to_string(), "wait", "{}", None).expect("call");
    let (model, _requests) = scripted(vec![ModelResponse::ToolCall(call)]);
    let harness = harness(model, tool_set(waiter));
    let handle = harness
        .run(RunRequest::new("wait").output(object_schema()))
        .into_handle();

    let cancel = handle.cancel_token();
    let outcome = tokio::spawn(handle.outcome());

    // Act
    started.notified().await;
    cancel.cancel();
    let outcome = outcome.await.expect("run task");

    // Assert
    assert_eq!(
        outcome.result.expect_err("cancelled").kind(),
        ErrorKind::Cancelled
    );
    tokio::time::timeout(Duration::from_secs(5), cancelled_signal)
        .await
        .expect("cancellation reached the handler")
        .expect("signal");
}

#[tokio::test]
async fn a_run_tool_set_replaces_the_default_and_orders_host_and_built_in_tools() {
    // Arrange
    let recorder = Recorder::new(Answer::Echo);
    let tools = ToolSet::builder()
        .handler(recorder.clone())
        .builtin(Tool::Read)
        .builtin(Tool::Write)
        .build()
        .expect("tool set");
    let write = ToolCall::from_json(
        "write_0".to_string(),
        "write",
        r#"{"path":"a.txt","patch":"+x"}"#,
        None,
    )
    .expect("write call");
    let (model, requests) = scripted(vec![ModelResponse::ToolCall(write), output()]);
    let harness = Harness::builder()
        .model(Arc::new(model))
        .context_budget(unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .allow(Tool::Read)
        .build()
        .expect("harness");

    // Act
    let outcome = harness
        .run(
            RunRequest::new("inspect")
                .output(object_schema())
                .tools(tools),
        )
        .await;

    // Assert
    outcome.result.as_ref().expect("run output");
    let requests = requests.lock().expect("requests lock");
    let names = requests[0]
        .tools()
        .iter()
        .map(ToolDefinition::name)
        .collect::<Vec<_>>();
    assert_eq!(
        names,
        ["fetch", "read"],
        "write is in the set but not allowed"
    );
    let unavailable: Value =
        serde_json::from_str(&result_for(&requests[1], "write_0")).expect("JSON result");
    assert_eq!(
        unavailable["hint"],
        "This call did not run. Call only the available tools (`fetch`, `read`) with their \
         documented arguments, or give your final response."
    );
}

#[test]
fn a_built_in_left_out_of_the_default_set_needs_no_repository() {
    // Arrange
    let (model, _requests) = scripted(Vec::new());

    // Act
    let built = Harness::builder()
        .model(Arc::new(model))
        .context_budget(unbounded_context_budget())
        .allow(Tool::Read)
        .tools(tool_set(Recorder::new(Answer::Echo)))
        .build();

    // Assert
    assert!(built.is_ok());
}

#[tokio::test]
async fn built_in_definitions_sent_to_the_model_are_unchanged() {
    // Arrange
    let (model, requests) = scripted(vec![output()]);
    let harness = harness(model, ToolSet::builtins());
    let policy = ToolPolicy::default()
        .allow(Tool::Read)
        .allow(Tool::Write)
        .allow(Tool::Bash);
    let bash = BashConfig::for_executor(
        Arc::new(UnsandboxedExecutor::without_isolation()),
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("bash configuration")
    .with_host_information();
    let harness = harness.repository(Repository::fixture("repo"));

    // Act
    harness
        .turn(
            "inspect",
            TurnOptions::new(object_schema(), policy).with_bash(bash),
        )
        .await
        .expect("turn");

    // Assert
    let requests = requests.lock().expect("requests lock");
    let sent = requests[0].tools();
    assert_eq!(
        sent,
        [
            ToolDefinition::read_with_comparison_base(None),
            ToolDefinition::write(),
            ToolDefinition::bash(Enforcement::None),
        ]
    );
    let triples = sent
        .iter()
        .map(|tool| json!([tool.name(), tool.description(), tool.parameters()]))
        .collect::<Vec<_>>();
    let digest = Sha256::digest(Value::Array(triples).to_string());
    assert_eq!(hex::encode(digest), BUILTIN_DEFINITIONS_SHA256);
}
