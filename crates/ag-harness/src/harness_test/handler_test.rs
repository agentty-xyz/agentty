//! Tool handler unit tests: host handlers run through ephemeral and durable
//! turns beside built-ins, and the built-in definitions sent to the model
//! stay byte-identical.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::{Notify, oneshot};

use super::support::{model, object_schema, response_without_metadata};
use crate::bash::{
    BashConfig, BashExecutor, BashProcess, Enforcement, ExecutionError, UnsandboxedExecutor,
};
use crate::command_journal::CommandCleanupScope;
use crate::context_budget_fixture::unbounded_context_budget;
use crate::error::ErrorKind;
use crate::event::ToolCallStatus;
use crate::harness::Harness;
use crate::memory_store::MemoryStore;
use crate::model::{MockModel, ModelError, ModelMessage, ModelRequest, ModelResponse};
use crate::recovery::ExecutionIdentity;
use crate::session::SessionError;
use crate::tool::{
    Builtin, ToolCall, ToolContext, ToolDefinition, ToolEffects, ToolError, ToolHandler,
    ToolOutput, ToolSet,
};
use crate::tool_handler::BuiltinPermissions;
use crate::turn::{HostToolOutcome, ToolActivity, TurnError};
use crate::{ComparisonBase, Repository, RunRequest, TurnOptions};

/// SHA-256 of the built-in definitions advertised with every built-in
/// allowed, no comparison base, and an unsandboxed `bash`, serialized as
/// `[name, description, parameters]` triples in order. Recorded from the
/// closed-dispatch engine before built-ins became handlers; it covers the
/// `read`, `write`, and `bash_none` entries of [`BUILTIN_DEFINITIONS`].
const BUILTIN_DEFINITIONS_SHA256: &str =
    "240f8cf7dd7071a994fc68162e4d3b690484f0ce2c9ecf7334925abb8ad92d66";

/// Every distinct built-in definition, by key, as `[name, description,
/// parameters]`.
const BUILTIN_DEFINITIONS: &str = include_str!("builtin_definitions.json");

/// How the [`Recorder`] handler answers a call.
#[derive(Clone, Copy)]
enum Answer {
    Echo,
    Reject,
    Fail,
    Oversized,
    Panic,
}

/// A host tool that answers as told and keeps every context it was called
/// with.
struct Recorder {
    answer: Answer,
    contexts: Mutex<Vec<ToolContext>>,
    name: &'static str,
}

impl Recorder {
    /// Returns a `fetch` tool that answers with `answer`.
    fn new(answer: Answer) -> Arc<Self> {
        Self::named("fetch", answer)
    }

    fn named(name: &'static str, answer: Answer) -> Arc<Self> {
        Arc::new(Self {
            answer,
            contexts: Mutex::new(Vec::new()),
            name,
        })
    }

    fn contexts(&self) -> Vec<ToolContext> {
        self.contexts.lock().expect("contexts lock").clone()
    }
}

#[async_trait]
impl ToolHandler for Recorder {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            self.name,
            "Fetch a URL.",
            json!({"type": "object", "properties": {"url": {"type": "string"}}}),
        )
        .expect("valid definition")
    }

    fn effects(&self) -> ToolEffects {
        match self.answer {
            Answer::Oversized => ToolEffects::Executes,
            _ => ToolEffects::ReadOnly,
        }
    }

    async fn call(&self, arguments: Value, context: ToolContext) -> Result<ToolOutput, ToolError> {
        self.contexts.lock().expect("contexts lock").push(context);
        match self.answer {
            Answer::Echo => Ok(ToolOutput::new(json!({"fetched": arguments}).to_string())),
            Answer::Reject => Err(ToolError::rejected("url is required")),
            Answer::Fail => Err(ToolError::failed("connection refused")),
            Answer::Oversized => Ok(ToolOutput::new("x".repeat(64 * 1024 + 1))),
            Answer::Panic => std::panic::resume_unwind(Box::new("handler bug")),
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

fn call(id: &str, name: &str, raw: &str) -> ToolCall {
    ToolCall::from_json(id.to_string(), name, raw, None).expect("tool call")
}

fn fetch(id: &str, raw: &str) -> ToolCall {
    call(id, "fetch", raw)
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

fn request() -> RunRequest {
    RunRequest::new("fetch it").output(object_schema())
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

/// Returns the outcome of each host tool call in `activities`, or `None`
/// for any other activity.
fn host_outcomes(activities: &[ToolActivity]) -> Vec<Option<HostToolOutcome>> {
    activities
        .iter()
        .map(|activity| match activity {
            ToolActivity::Host { outcome, .. } => Some(*outcome),
            _ => None,
        })
        .collect()
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
    let (mut events, handle) = harness.run(request()).into_parts();
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
        [ToolActivity::Host { outcome: HostToolOutcome::Completed, tool, .. }] if tool == "fetch"
    ));
    let contexts = recorder.contexts();
    assert_eq!(contexts.len(), 1, "{contexts:?}");
    let context = &contexts[0];
    assert_eq!(context.call_id(), "fetch_0");
    assert_eq!(context.session_id(), None, "a stateless run has no session");
    assert_eq!(context.workspace(), None);
    assert_eq!(context.run_id(), Some(&first_event.run));
}

#[tokio::test]
async fn rejections_and_arguments_that_are_not_an_object_are_answered_with_errors() {
    // Arrange
    let recorder = Recorder::new(Answer::Reject);
    let calls = vec![
        fetch("fetch_0", r#"{"url":"https://example.com"}"#),
        fetch("fetch_1", r#"{"url""#),
        fetch("fetch_2", "[1]"),
        fetch("fetch_3", ""),
    ];
    let (model, requests) = scripted(vec![ModelResponse::ToolCalls(calls), output()]);
    let harness = harness(model, tool_set(recorder.clone()));

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    outcome.result.as_ref().expect("run output");
    let requests = requests.lock().expect("requests lock");
    let rejected = r#"{"error":"url is required","status":"rejected"}"#;
    assert_eq!(result_for(&requests[1], "fetch_0"), rejected);
    assert_eq!(
        result_for(&requests[1], "fetch_3"),
        rejected,
        "blank arguments reach the handler as an empty object"
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
    assert_eq!(
        result_for(&requests[1], "fetch_2"),
        r#"{"error":"function arguments must be a JSON object","status":"rejected"}"#
    );
    assert_eq!(
        host_outcomes(outcome.report.tool_calls()),
        [
            Some(HostToolOutcome::Rejected),
            Some(HostToolOutcome::InvalidArguments),
            Some(HostToolOutcome::InvalidArguments),
            Some(HostToolOutcome::Rejected),
        ]
    );
    let contexts = recorder.contexts();
    assert_eq!(
        contexts.len(),
        2,
        "invalid arguments never reach the handler"
    );
    assert!(
        contexts.iter().all(|context| context.run_id().is_some()),
        "an awaited run still names itself"
    );
}

#[tokio::test]
async fn an_oversized_result_is_dropped_and_reported_as_a_completed_call() {
    // Arrange
    let recorder = Recorder::new(Answer::Oversized);
    let (model, requests) = scripted(vec![
        ModelResponse::ToolCall(fetch("fetch_0", "{}")),
        output(),
    ]);
    let harness = harness(model, tool_set(recorder.clone()));

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    outcome.result.as_ref().expect("run output");
    assert_eq!(recorder.effects(), ToolEffects::Executes);
    let requests = requests.lock().expect("requests lock");
    assert_eq!(
        result_for(&requests[1], "fetch_0"),
        json!({
            "error": "the tool ran, but its 65537-byte result is over the 65536-byte limit and \
                      was dropped",
            "hint": "The call's effects already happened; do not repeat it to see the result. \
                     Ask for less output if the tool supports it.",
            "status": "output_too_large",
        })
        .to_string()
    );
    let calls = outcome.report.tool_calls();
    assert_eq!(
        host_outcomes(calls),
        [Some(HostToolOutcome::ResultTooLarge)]
    );
    assert_eq!(ToolCallStatus::from(&calls[0]), ToolCallStatus::Completed);
}

#[tokio::test]
async fn host_calls_whose_arguments_are_never_an_object_end_the_turn() {
    // Arrange
    let recorder = Recorder::new(Answer::Echo);
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCall(fetch("fetch_0", "[1]")),
        ModelResponse::ToolCall(fetch("fetch_1", "null")),
        ModelResponse::ToolCall(fetch("fetch_2", "true")),
    ]);
    let harness = harness(model, tool_set(recorder.clone()));

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    let error = outcome.result.expect_err("malformed responses");
    assert_eq!(
        error.to_string(),
        TurnError::from(ModelError::InvalidToolArguments {
            reason: "`fetch`: function arguments must be a JSON object".to_string(),
        })
        .to_string()
    );
    assert!(recorder.contexts().is_empty());
}

#[tokio::test]
async fn a_name_rewritten_for_replay_never_runs_a_host_handler() {
    // Arrange
    let recorder = Recorder::named("lookup_issue", Answer::Echo);
    let (model, requests) = scripted(vec![
        ModelResponse::ToolCall(call("call_0", "lookup.issue", "{}")),
        output(),
    ]);
    let harness = harness(model, tool_set(recorder.clone()));

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    outcome.result.as_ref().expect("run output");
    assert!(recorder.contexts().is_empty());
    assert!(matches!(
        outcome.report.tool_calls(),
        [ToolActivity::Unavailable { tool, .. }] if tool == "lookup_issue"
    ));
    let unavailable: Value =
        serde_json::from_str(&result_for(&requests.lock().expect("lock")[1], "call_0"))
            .expect("JSON result");
    assert_eq!(unavailable["status"], "unavailable");
}

#[tokio::test]
async fn identical_host_calls_are_nudged_and_then_stop_the_turn() {
    // Arrange
    let recorder = Recorder::new(Answer::Echo);
    let responses = (0..5)
        .map(|index| ModelResponse::ToolCall(fetch(&format!("fetch_{index}"), "{}")))
        .collect();
    let (model, requests) = scripted(responses);
    let harness = harness(model, tool_set(recorder.clone()));

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    let error = outcome.result.expect_err("repeated calls");
    assert_eq!(error.kind(), ErrorKind::Repetition);
    assert_eq!(
        error.to_string(),
        TurnError::RepeatedToolCalls {
            repeats: 5,
            tool: "fetch".to_string(),
        }
        .to_string()
    );
    assert_eq!(recorder.contexts().len(), 5);
    let requests = requests.lock().expect("requests lock");
    assert!(
        requests[3]
            .messages()
            .iter()
            .any(|message| format!("{message:?}").contains("called `fetch` 3 times")),
        "the third identical call is nudged"
    );
}

#[tokio::test]
async fn a_failed_handler_fails_the_run_as_a_tool_failure() {
    // Arrange
    let (model, _requests) = scripted(vec![ModelResponse::ToolCall(fetch("fetch_0", "{}"))]);
    let harness = harness(model, tool_set(Recorder::new(Answer::Fail)));

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    let error = outcome.result.expect_err("failed tool");
    assert_eq!(error.kind(), ErrorKind::ToolFailed);
    assert_eq!(error.to_string(), "tool `fetch` failed: connection refused");
}

#[tokio::test]
async fn a_panicking_handler_fails_its_turn_and_the_session_runs_again() {
    // Arrange
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCall(fetch("fetch_0", "{}")),
        output(),
    ]);
    let harness =
        harness(model, tool_set(Recorder::new(Answer::Panic))).store(Arc::new(MemoryStore::new()));
    let session = harness
        .session("panics", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let panicked = session.run(RunRequest::new("fetch it")).await;
    let next = session.run(RunRequest::new("answer")).await;

    // Assert
    let error = panicked.result.expect_err("panicked tool");
    assert_eq!(error.kind(), ErrorKind::ToolFailed);
    assert_eq!(
        error.to_string(),
        "tool `fetch` failed: the handler panicked"
    );
    next.result.as_ref().expect("the session runs another turn");
}

#[tokio::test]
async fn a_durable_turn_names_its_session_and_replays_host_calls() {
    // Arrange
    let recorder = Recorder::new(Answer::Echo);
    let (model, requests) = scripted(vec![
        ModelResponse::ToolCall(fetch("fetch_0", r#"{"url":"https://example.com"}"#)),
        output(),
        output(),
    ]);
    let harness = harness(model, tool_set(recorder.clone())).store(Arc::new(MemoryStore::new()));
    let session = harness
        .session("durable", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let first = session.run(RunRequest::new("fetch it")).await;
    let second = session.run(RunRequest::new("again")).await;

    // Assert
    first.result.as_ref().expect("first run");
    second.result.as_ref().expect("second run");
    let contexts = recorder.contexts();
    assert_eq!(
        contexts[0]
            .session_id()
            .map(crate::event::SessionId::as_str),
        Some(session.id())
    );
    let requests = requests.lock().expect("requests lock");
    let replayed = &requests[2];
    assert!(replayed.messages().iter().any(|message| matches!(
        message,
        ModelMessage::AssistantToolCall(call) if call.id() == "fetch_0" && call.name() == "fetch"
    )));
    assert_eq!(
        result_for(replayed, "fetch_0"),
        r#"{"fetched":{"url":"https://example.com"}}"#
    );
}

#[tokio::test]
async fn a_host_id_reused_with_another_tool_set_conflicts() {
    // Arrange
    let (model, requests) = scripted(vec![output()]);
    let harness = harness(model, ToolSet::empty())
        .store(Arc::new(MemoryStore::new()))
        .execution_identity(ExecutionIdentity::new("scripted", "1").expect("identity"));
    let mut session = harness
        .session("host", object_schema())
        .create()
        .await
        .expect("session");
    let options = || TurnOptions::from_output(object_schema());
    let host_only = tool_set(Recorder::new(Answer::Echo));

    // Act
    let first = session
        .turn("ask")
        .options(options())
        .host_id("job")
        .await
        .expect("first turn");
    let retried = session
        .turn("ask")
        .options(options().with_tools(ToolSet::empty()))
        .host_id("job")
        .await
        .expect("same request");
    let changed = session
        .turn("ask")
        .options(options().with_tools(host_only))
        .host_id("job")
        .await;

    // Assert
    assert_eq!(retried.output(), first.output());
    assert!(matches!(changed, Err(SessionError::HostTurnConflict)));
    assert_eq!(
        requests.lock().expect("requests lock").len(),
        1,
        "the equivalent request replays the recorded turn"
    );
}

#[tokio::test]
async fn a_host_request_recorded_with_built_in_permissions_replays_with_its_tool_set() {
    // Arrange
    let cases: [&[Builtin]; 3] = [
        &[],
        &[Builtin::Read],
        &[Builtin::Read, Builtin::Write, Builtin::Bash],
    ];
    let (model, requests) = scripted(cases.iter().map(|_| output()).collect());
    let harness = harness(model, ToolSet::empty())
        .repository(Repository::fixture("repo"))
        .store(Arc::new(MemoryStore::new()))
        .execution_identity(ExecutionIdentity::new("scripted", "1").expect("identity"));
    let mut session = harness
        .session("legacy", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let mut replays = Vec::new();
    for (index, allowed) in cases.into_iter().enumerate() {
        let host_id = format!("job_{index}");
        let recorded = session
            .turn("ask")
            .options(
                TurnOptions::from_output(object_schema())
                    .with_tools(permitted_builtins(allowed))
                    .with_bash(bash_config(Enforcement::None)),
            )
            .host_id(host_id.clone())
            .await
            .expect("recorded turn");
        let replayed = session
            .turn("ask")
            .options(
                TurnOptions::from_output(object_schema())
                    .with_tools(selected_builtins(allowed))
                    .with_bash(bash_config(Enforcement::None)),
            )
            .host_id(host_id)
            .await
            .expect("replayed turn");
        replays.push((recorded.output().clone(), replayed.output().clone()));
    }

    // Assert
    assert_eq!(
        requests.lock().expect("requests lock").len(),
        cases.len(),
        "every replay returns the recorded turn without a model call"
    );
    for (recorded, replayed) in replays {
        assert_eq!(replayed, recorded);
    }
}

/// A host tool that hands its context's cancellation to a task, which
/// reports when the turn is cancelled, then waits forever holding a guard
/// that reports when its call future is dropped.
struct Waiter {
    cancelled: Mutex<Option<oneshot::Sender<()>>>,
    dropped: Mutex<Option<oneshot::Sender<()>>>,
    started: Arc<Notify>,
}

/// Sends on its channel when dropped.
struct DropGuard(Option<oneshot::Sender<()>>);

impl Drop for DropGuard {
    fn drop(&mut self) {
        if let Some(dropped) = self.0.take() {
            let _ = dropped.send(());
        }
    }
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
        let _guard = DropGuard(self.dropped.lock().expect("sender lock").take());
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
async fn a_cancelled_turn_signals_its_handler_and_drops_the_call() {
    // Arrange
    let (cancelled, cancelled_signal) = oneshot::channel();
    let (dropped, dropped_signal) = oneshot::channel();
    let started = Arc::new(Notify::new());
    let waiter = Arc::new(Waiter {
        cancelled: Mutex::new(Some(cancelled)),
        dropped: Mutex::new(Some(dropped)),
        started: Arc::clone(&started),
    });
    let (model, _requests) = scripted(vec![ModelResponse::ToolCall(call("wait_0", "wait", "{}"))]);
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
    tokio::time::timeout(Duration::from_secs(5), dropped_signal)
        .await
        .expect("the call's future was dropped")
        .expect("signal");
}

#[tokio::test]
async fn a_run_tool_set_replaces_the_default_and_orders_host_and_built_in_tools() {
    // Arrange
    let recorder = Recorder::new(Answer::Echo);
    let tools = ToolSet::builder()
        .handler(recorder.clone())
        .read()
        .write()
        .build()
        .expect("tool set");
    let bash = call("bash_0", "bash", r#"{"command":"ls"}"#);
    let (model, requests) = scripted(vec![ModelResponse::ToolCall(bash), output()]);
    let harness = Harness::builder()
        .model(Arc::new(model))
        .context_budget(unbounded_context_budget())
        .repository(Repository::fixture("repo"))
        .tools(ToolSet::empty().with_read())
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
    assert_eq!(names, ["fetch", "read", "write"]);
    let unavailable: Value =
        serde_json::from_str(&result_for(&requests[1], "bash_0")).expect("JSON result");
    assert_eq!(
        unavailable["hint"],
        "This call did not run. Call only the available tools (`fetch`, `read`, `write`) with \
         their documented arguments, or give your final response."
    );
}

#[tokio::test]
async fn a_run_set_replaces_the_harness_default_set() {
    // Arrange
    let default_set = tool_set(Recorder::new(Answer::Echo))
        .to_builder()
        .read()
        .write()
        .build()
        .expect("tool set");
    let requests = [
        request().tools(ToolSet::empty().with_read()),
        request().tools(ToolSet::empty()),
    ];

    // Act
    let mut advertised = Vec::new();
    for run in requests {
        let (model, sent) = scripted(vec![output()]);
        let harness = Harness::builder()
            .model(Arc::new(model))
            .context_budget(unbounded_context_budget())
            .repository(Repository::fixture("repo"))
            .tools(default_set.clone())
            .build()
            .expect("harness");
        harness.run(run).await.result.as_ref().expect("run output");
        let names = sent.lock().expect("requests lock")[0]
            .tools()
            .iter()
            .map(|tool| tool.name().to_string())
            .collect::<Vec<_>>();
        advertised.push(names);
    }

    // Assert
    // A run's own set replaces the harness's default set.
    assert_eq!(advertised, [vec!["read".to_string()], vec![]]);
}

#[tokio::test]
async fn built_ins_need_a_repository_only_in_turns_that_offer_them() {
    // Arrange: the builder's set offers only `fetch`, so `read` needs no
    // repository until a turn's own set offers it.
    let build = |model: MockModel| {
        Harness::builder()
            .model(Arc::new(model))
            .context_budget(unbounded_context_budget())
            .tools(tool_set(Recorder::new(Answer::Echo)))
            .build()
            .expect("harness")
    };
    let (default_model, default_requests) = scripted(vec![output()]);
    let (options_model, options_requests) = scripted(Vec::new());
    let (run_model, run_requests) = scripted(Vec::new());

    // Act
    let default_run = build(default_model).run(request()).await;
    let explicit_options = build(options_model)
        .turn(
            "ask",
            TurnOptions::from_output(object_schema()).with_tools(ToolSet::empty().with_read()),
        )
        .await;
    let builtins_run = build(run_model)
        .run(request().tools(ToolSet::builtins()))
        .await;

    // Assert
    default_run.result.as_ref().expect("default run");
    let advertised = default_requests.lock().expect("requests lock")[0]
        .tools()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect::<Vec<_>>();
    assert_eq!(advertised, ["fetch"]);
    assert!(
        matches!(explicit_options, Err(TurnError::RepositoryRequired)),
        "explicit options replace the builder's set with the built-ins"
    );
    assert_eq!(
        builtins_run
            .result
            .expect_err("built-ins without a repository")
            .to_string(),
        TurnError::RepositoryRequired.to_string()
    );
    assert!(options_requests.lock().expect("lock").is_empty());
    assert!(run_requests.lock().expect("lock").is_empty());
}

/// A `bash` executor that claims `enforcement` and never runs a command.
struct Claiming(Enforcement);

impl BashExecutor for Claiming {
    fn identity(&self) -> &'static str {
        "claiming"
    }

    fn enforcement(&self) -> Enforcement {
        self.0
    }

    fn cleanup_scope(&self) -> CommandCleanupScope {
        CommandCleanupScope::ProcessGroupBestEffort
    }

    fn bind(&self) -> Result<Box<dyn BashProcess>, ExecutionError> {
        Err(ExecutionError::Unsupported)
    }
}

/// Returns a `bash` configuration whose executor claims `enforcement`.
fn bash_config(enforcement: Enforcement) -> BashConfig {
    let executor: Arc<dyn BashExecutor> = match enforcement {
        Enforcement::None => Arc::new(UnsandboxedExecutor::without_isolation()),
        _ => Arc::new(Claiming(enforcement)),
    };

    BashConfig::for_executor(
        executor,
        "/bin/bash".into(),
        "revision".into(),
        Duration::from_secs(5),
        1024,
    )
    .expect("bash configuration")
    .with_host_information()
}

/// Returns the built-ins in `allowed` as a set, in their advertised order.
fn selected_builtins(allowed: &[Builtin]) -> ToolSet {
    let mut tools = ToolSet::empty();
    if allowed.contains(&Builtin::Read) {
        tools = tools.with_read();
    }
    if allowed.contains(&Builtin::Write) {
        tools = tools.with_write();
    }
    if allowed.contains(&Builtin::Bash) {
        tools = tools.with_bash();
    }

    tools
}

/// Returns the built-ins `allowed` permits, selected the way durable records
/// describe a set of built-ins: by permissions alone.
fn permitted_builtins(allowed: &[Builtin]) -> ToolSet {
    let permissions = allowed
        .iter()
        .fold(BuiltinPermissions::default(), |permissions, builtin| {
            permissions.allow(*builtin)
        });

    ToolSet::builtins().restricted_to(permissions)
}

/// Returns the definitions one turn sends to the model when it offers the
/// built-ins in `allowed`, with a comparison base when `compared`, and
/// `bash` claiming `enforcement`. The built-ins are selected one by one, or
/// by their permissions when `by_permissions` is set.
async fn sent_definitions(
    compared: bool,
    enforcement: Enforcement,
    allowed: &[Builtin],
    by_permissions: bool,
) -> Vec<ToolDefinition> {
    let (model, requests) = scripted(vec![output()]);
    let harness = harness(model, ToolSet::empty()).repository(Repository::fixture("repo"));
    let bash = bash_config(enforcement);
    let tools = if by_permissions {
        permitted_builtins(allowed)
    } else {
        selected_builtins(allowed)
    };
    let options = TurnOptions::from_output(object_schema()).with_tools(tools);
    let mut options = options.with_bash(bash);
    if compared {
        options = options.with_comparison_base(ComparisonBase::fixture("repo"));
    }
    harness.turn("inspect", options).await.expect("turn");

    let requests = requests.lock().expect("requests lock");

    requests[0].tools().to_vec()
}

fn triple(definition: &ToolDefinition) -> Value {
    json!([
        definition.name(),
        definition.description(),
        definition.parameters()
    ])
}

#[tokio::test]
async fn built_in_definitions_sent_to_the_model_are_unchanged() {
    // Arrange
    let golden: Value = serde_json::from_str(BUILTIN_DEFINITIONS).expect("golden JSON");
    let cases: [(bool, Enforcement, &[Builtin], &[&str]); 5] = [
        (
            false,
            Enforcement::None,
            &[Builtin::Read, Builtin::Write, Builtin::Bash],
            &["read", "write", "bash_none"],
        ),
        (
            true,
            Enforcement::None,
            &[Builtin::Bash, Builtin::Read, Builtin::Write],
            &["read_with_base", "write", "bash_none"],
        ),
        (
            false,
            Enforcement::FileSystem,
            &[Builtin::Bash, Builtin::Write],
            &["write", "bash_file_system"],
        ),
        (
            true,
            Enforcement::FileSystemAndNetwork,
            &[Builtin::Bash, Builtin::Read],
            &["read_with_base", "bash_file_system_and_network"],
        ),
        (
            false,
            Enforcement::FileSystemAndNetwork,
            &[Builtin::Write],
            &["write"],
        ),
    ];

    // Act
    let mut sent = Vec::new();
    let mut sent_with_policy = Vec::new();
    for (compared, enforcement, allowed, _keys) in cases {
        for (legacy, sent) in [(false, &mut sent), (true, &mut sent_with_policy)] {
            let definitions = sent_definitions(compared, enforcement, allowed, legacy).await;
            sent.push(json!({
                "case": format!("compared: {compared}, enforcement: {enforcement:?}, allowed: {allowed:?}"),
                "definitions": definitions.iter().map(triple).collect::<Vec<_>>(),
            }));
        }
    }

    // Assert
    let expected = cases
        .iter()
        .map(|(compared, enforcement, allowed, keys)| {
            json!({
                "case": format!("compared: {compared}, enforcement: {enforcement:?}, allowed: {allowed:?}"),
                "definitions": keys.iter().map(|key| golden[*key].clone()).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(sent, expected);
    assert_eq!(sent_with_policy, expected);
    let legacy = ["read", "write", "bash_none"]
        .map(|key| golden[key].clone())
        .to_vec();
    let digest = Sha256::digest(Value::Array(legacy).to_string());
    assert_eq!(hex::encode(digest), BUILTIN_DEFINITIONS_SHA256);
}
