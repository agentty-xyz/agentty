//! Approval unit tests: calls that change state wait for the harness's
//! approval handler, read-only calls never ask, and a decision that never
//! comes fails the run and releases its session instead of holding it.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_core::Stream;
use serde_json::{Value, json};
use tokio::sync::{Notify, oneshot};

use super::support::{
    LifecycleRecorder, model, object_schema, response_without_metadata, write_call,
};
use crate::approval::{ApprovalHandler, ApprovalRequest, Decision};
use crate::context_budget_fixture::unbounded_context_budget;
use crate::error::ErrorKind;
use crate::event::{EventStream, Item, ItemKind, RunEvent, RunEventKind, ToolCallStatus};
use crate::file_system::MockFileSystem;
use crate::harness::{Harness, HarnessBuilder};
use crate::lifecycle::LifecycleEventKind;
use crate::limit::{LimitKind, RunLimits};
use crate::memory_store::MemoryStore;
use crate::model::{MockModel, ModelError, ModelMessage, ModelRequest, ModelResponse};
use crate::reservation::TURN_LEASE_SECONDS;
use crate::tool::{
    ToolCall, ToolContext, ToolDefinition, ToolEffects, ToolError, ToolHandler, ToolOutput, ToolSet,
};
use crate::transcript::TurnRange;
use crate::turn::{HostToolOutcome, ToolActivity};
use crate::{Repository, RunRequest, TurnOptions};

/// Approval timeout of every harness in this suite.
const TIMEOUT: Duration = Duration::from_secs(30);

/// Arguments of a tool that never ran.
const NO_CALLS: [Value; 0] = [];

/// How the [`Approver`] answers.
enum Behavior {
    /// Returns this decision at once.
    Decide(Decision),
    /// Allows the call after this long.
    AllowAfter(Duration),
    /// Never decides; the pending decision reports its drop.
    Never(Mutex<Option<oneshot::Sender<()>>>),
}

/// An approval handler that answers as told and keeps every request.
struct Approver {
    asked: Notify,
    behavior: Behavior,
    requests: Mutex<Vec<ApprovalRequest>>,
}

impl Approver {
    fn new(behavior: Behavior) -> Arc<Self> {
        Arc::new(Self {
            asked: Notify::new(),
            behavior,
            requests: Mutex::new(Vec::new()),
        })
    }

    fn deciding(decision: Decision) -> Arc<Self> {
        Self::new(Behavior::Decide(decision))
    }

    /// Returns a handler that never decides, and the signal of its pending
    /// decision's drop.
    fn never() -> (Arc<Self>, oneshot::Receiver<()>) {
        let (dropped, dropped_signal) = oneshot::channel();

        (
            Self::new(Behavior::Never(Mutex::new(Some(dropped)))),
            dropped_signal,
        )
    }

    fn requests(&self) -> Vec<ApprovalRequest> {
        self.requests.lock().expect("requests lock").clone()
    }
}

#[async_trait]
impl ApprovalHandler for Approver {
    async fn decide(&self, request: &ApprovalRequest) -> Decision {
        self.requests
            .lock()
            .expect("requests lock")
            .push(request.clone());
        self.asked.notify_one();
        match &self.behavior {
            Behavior::Decide(decision) => decision.clone(),
            Behavior::AllowAfter(delay) => {
                tokio::time::sleep(*delay).await;

                Decision::Allow
            }
            Behavior::Never(dropped) => {
                let _dropped = dropped.lock().expect("dropped lock").take();

                std::future::pending().await
            }
        }
    }
}

/// A host tool with `effects` that echoes its arguments and keeps them.
struct Echo {
    arguments: Mutex<Vec<Value>>,
    effects: ToolEffects,
    name: &'static str,
}

impl Echo {
    fn new(name: &'static str, effects: ToolEffects) -> Arc<Self> {
        Arc::new(Self {
            arguments: Mutex::new(Vec::new()),
            effects,
            name,
        })
    }

    fn arguments(&self) -> Vec<Value> {
        self.arguments.lock().expect("arguments lock").clone()
    }
}

#[async_trait]
impl ToolHandler for Echo {
    fn definition(&self) -> ToolDefinition {
        ToolDefinition::new(
            self.name,
            "Echo a target.",
            json!({
                "type": "object",
                "properties": {"target": {"type": "string"}},
                "required": ["target"]
            }),
        )
        .expect("valid definition")
    }

    fn effects(&self) -> ToolEffects {
        self.effects
    }

    async fn call(&self, arguments: Value, _context: ToolContext) -> Result<ToolOutput, ToolError> {
        self.arguments
            .lock()
            .expect("arguments lock")
            .push(arguments.clone());

        Ok(ToolOutput::new(json!({"echo": arguments}).to_string()))
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

fn output() -> ModelResponse {
    ModelResponse::Output(json!({"summary": "done"}))
}

fn builder(model: MockModel, tools: ToolSet, approver: Arc<Approver>) -> HarnessBuilder {
    Harness::builder()
        .model(Arc::new(model))
        .context_budget(unbounded_context_budget())
        .tools(tools)
        .approval(approver)
        .approval_timeout(TIMEOUT)
}

fn harness(model: MockModel, tools: ToolSet, approver: Arc<Approver>) -> Harness {
    builder(model, tools, approver).build().expect("harness")
}

fn tool_set(handlers: Vec<Arc<Echo>>) -> ToolSet {
    handlers
        .into_iter()
        .fold(ToolSet::builder(), |builder, handler| {
            builder.handler(handler)
        })
        .build()
        .expect("tool set")
}

fn request() -> RunRequest {
    RunRequest::new("deploy it").output(object_schema())
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

/// Returns the telemetry label of every failed tool call `recorder` saw.
fn tool_failures(recorder: &LifecycleRecorder) -> Vec<&'static str> {
    recorder
        .events
        .lock()
        .expect("events lock")
        .iter()
        .filter_map(|event| match event.kind() {
            LifecycleEventKind::ToolFailed { error_type, .. } => Some(error_type.as_str()),
            _ => None,
        })
        .collect()
}

/// Reads every event of a finished run.
async fn events(mut stream: EventStream) -> Vec<RunEvent> {
    let mut events = Vec::new();
    while let Some(event) =
        std::future::poll_fn(|context| std::pin::Pin::new(&mut stream).poll_next(context)).await
    {
        events.push(event);
    }

    events
}

#[tokio::test]
async fn a_call_that_changes_state_waits_for_approval_and_a_read_only_call_never_asks() {
    // Arrange
    let lookup = Echo::new("lookup", ToolEffects::ReadOnly);
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let approver = Approver::deciding(Decision::Allow);
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCalls(vec![
            call("lookup_0", "lookup", r#"{"target":"docs"}"#),
            call("deploy_0", "deploy", r#"{"target":"prod"}"#),
        ]),
        output(),
    ]);
    let harness = harness(
        model,
        tool_set(vec![lookup.clone(), deploy.clone()]),
        approver.clone(),
    );

    // Act
    let (stream, handle) = harness.run(request()).into_parts();
    let (outcome, events) = tokio::join!(handle.outcome(), events(stream));

    // Assert
    outcome.result.as_ref().expect("run output");
    assert_eq!(lookup.arguments(), [json!({"target": "docs"})]);
    assert_eq!(deploy.arguments(), [json!({"target": "prod"})]);
    let requests = approver.requests();
    assert_eq!(requests.len(), 1, "only `deploy` asks: {requests:?}");
    let asked = &requests[0];
    assert_eq!(asked.tool, "deploy");
    assert_eq!(asked.call_id, "deploy_0");
    assert_eq!(asked.arguments, json!({"target": "prod"}));
    assert_eq!(asked.effects, ToolEffects::Executes);
    assert_eq!(asked.run.as_ref(), Some(&events[0].run));
    assert_eq!(asked.session, None, "a stateless run has no session");
    let kinds: Vec<&RunEventKind> = events.iter().map(|event| &event.kind).collect();
    let approval = kinds
        .iter()
        .position(|kind| matches!(kind, RunEventKind::ApprovalRequested { .. }))
        .expect("approval event");
    let RunEventKind::ApprovalRequested { id, request } = kinds[approval] else {
        unreachable!("found above");
    };
    assert_eq!(request, asked);
    assert!(matches!(
        kinds[approval - 2],
        RunEventKind::ItemStarted { id: started, item_kind: ItemKind::ToolCall } if started == id
    ));
    assert!(matches!(
        kinds[approval - 1],
        RunEventKind::ToolCallRequested { id: requested, call_id, .. }
            if requested == id && call_id == "deploy_0"
    ));
    assert!(matches!(
        kinds[approval + 1],
        RunEventKind::ItemCompleted { id: completed, item: Item::ToolCall(item) }
            if completed == id && item.status == ToolCallStatus::Completed
    ));
}

#[tokio::test]
async fn allowing_an_ordinary_call_unsandboxed_runs_it_as_allowed() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let approver = Approver::deciding(Decision::AllowUnsandboxed);
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCalls(vec![call("deploy_0", "deploy", r#"{"target":"prod"}"#)]),
        output(),
    ]);
    let harness = harness(model, tool_set(vec![deploy.clone()]), approver.clone());

    // Act
    let outcome = harness.run(request()).into_parts().1.outcome().await;

    // Assert
    outcome.result.as_ref().expect("run output");
    assert_eq!(deploy.arguments(), [json!({"target": "prod"})]);
    // Only calls marked to leave the sandbox show a command.
    assert_eq!(approver.requests()[0].command, None);
}

#[tokio::test]
async fn a_denied_call_returns_the_feedback_to_the_model_and_the_run_continues() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Writes);
    let approver = Approver::deciding(Decision::Deny {
        feedback: "deploys are frozen".to_string(),
    });
    let (model, requests) = scripted(vec![
        ModelResponse::ToolCall(call("deploy_0", "deploy", r#"{"target":"prod"}"#)),
        output(),
    ]);
    let recorder = LifecycleRecorder::default();
    let harness = builder(model, tool_set(vec![deploy.clone()]), approver)
        .lifecycle_observer(recorder.clone())
        .build()
        .expect("harness");

    // Act
    let (stream, handle) = harness.run(request()).into_parts();
    let (outcome, events) = tokio::join!(handle.outcome(), events(stream));

    // Assert
    outcome.result.as_ref().expect("the run continues");
    assert_eq!(deploy.arguments(), NO_CALLS, "a denied call never runs");
    assert_eq!(
        result_for(&requests.lock().expect("requests lock")[1], "deploy_0"),
        r#"{"error":"deploys are frozen","status":"denied"}"#
    );
    assert!(matches!(
        outcome.report.tool_calls(),
        [ToolActivity::Host { outcome: HostToolOutcome::Denied, tool, .. }] if tool == "deploy"
    ));
    assert!(
        outcome.report.tool_calls()[0]
            .to_string()
            .starts_with("deploy (denied; ")
    );
    assert_eq!(tool_failures(&recorder), ["approval_denied"]);
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        RunEventKind::ItemCompleted { item: Item::ToolCall(item), .. }
            if item.status == ToolCallStatus::Denied
    )));
}

#[tokio::test]
async fn a_denied_built_in_write_never_touches_the_repository() {
    // Arrange
    let approver = Approver::deciding(Decision::Deny {
        feedback: "read only today".to_string(),
    });
    let (model, requests) = scripted(vec![
        ModelResponse::ToolCall(write_call("write_0", "+fn main() {}")),
        output(),
    ]);
    let harness = builder(model, ToolSet::empty().with_write(), approver.clone())
        .repository(Repository::fixture("repo"))
        .file_system(MockFileSystem::new())
        .build()
        .expect("harness");

    // Act
    let outcome = harness.run(request()).await;

    // Assert
    outcome.result.as_ref().expect("the run continues");
    assert_eq!(approver.requests()[0].effects, ToolEffects::Writes);
    assert_eq!(
        result_for(&requests.lock().expect("requests lock")[1], "write_0"),
        r#"{"error":"read only today","status":"denied"}"#
    );
}

#[tokio::test]
async fn allow_with_runs_the_call_with_its_validated_replacement_arguments() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let replaced = Approver::deciding(Decision::AllowWith(json!({"target": "staging"})));
    let invalid = Approver::deciding(Decision::AllowWith(json!({"target": 5})));
    let responses = || {
        vec![
            ModelResponse::ToolCall(call("deploy_0", "deploy", r#"{"target":"prod"}"#)),
            output(),
        ]
    };
    let (model, requests) = scripted(responses());
    let (invalid_model, _) = scripted(responses());
    let allowed = harness(model, tool_set(vec![deploy.clone()]), replaced);
    let rejected = harness(invalid_model, tool_set(vec![deploy.clone()]), invalid);

    // Act
    let allowed = allowed.run(request()).await;
    let rejected = rejected.run(request()).await;

    // Assert
    allowed.result.as_ref().expect("run output");
    assert_eq!(deploy.arguments(), [json!({"target": "staging"})]);
    let requests = requests.lock().expect("requests lock");
    assert!(
        requests[1].messages().iter().any(|message| matches!(
            message,
            ModelMessage::AssistantToolCall(call)
                if call.arguments_json().ok().as_deref() == Some(r#"{"target":"prod"}"#)
        )),
        "the session keeps the model's call"
    );
    assert_eq!(
        result_for(&requests[1], "deploy_0"),
        r#"{"echo":{"target":"staging"},"approved_arguments":{"target":"staging"}}"#,
        "the model learns which arguments ran"
    );
    let error = rejected.result.expect_err("invalid replacement");
    assert_eq!(error.kind(), ErrorKind::ToolFailed);
    assert!(
        error
            .to_string()
            .starts_with("tool `deploy` failed: approved arguments do not match"),
        "{error}"
    );
    assert_eq!(
        deploy.arguments().len(),
        1,
        "the invalid replacement never ran"
    );
}

#[tokio::test(start_paused = true)]
async fn a_decision_that_never_comes_times_out_and_releases_the_session() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let (approver, dropped) = Approver::never();
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCall(call("deploy_0", "deploy", r#"{"target":"prod"}"#)),
        output(),
    ]);
    let recorder = LifecycleRecorder::default();
    let harness = builder(model, tool_set(vec![deploy.clone()]), approver)
        .store(Arc::new(MemoryStore::new()))
        .lifecycle_observer(recorder.clone())
        .build()
        .expect("harness");
    let session = harness
        .session("approvals", object_schema())
        .create()
        .await
        .expect("session");
    let started_at = tokio::time::Instant::now();

    // Act
    let (stream, handle) = session.run(RunRequest::new("deploy it")).into_parts();
    let (timed_out, events) = tokio::join!(handle.outcome(), events(stream));
    let waited = started_at.elapsed();
    let next = session.run(RunRequest::new("answer")).await;

    // Assert
    let kinds: Vec<&RunEventKind> = events.iter().map(|event| &event.kind).collect();
    let Some(RunEventKind::ApprovalRequested { id, .. }) = kinds
        .iter()
        .find(|kind| matches!(kind, RunEventKind::ApprovalRequested { .. }))
    else {
        unreachable!("the call asked for approval: {kinds:?}");
    };
    assert!(
        !kinds.iter().any(
            |kind| matches!(kind, RunEventKind::ItemCompleted { id: completed, .. } if completed == id)
        ),
        "the timed-out call's item never completes"
    );
    assert!(matches!(
        kinds.last(),
        Some(RunEventKind::Completed(summary))
            if summary.error.as_ref().map(|failure| failure.kind) == Some(ErrorKind::ApprovalTimeout)
    ));
    assert_eq!(tool_failures(&recorder), ["approval_timeout"]);
    let error = timed_out.result.expect_err("approval timed out");
    assert_eq!(error.kind(), ErrorKind::ApprovalTimeout);
    assert!(!error.is_retryable());
    assert_eq!(
        error.to_string(),
        "approval of tool `deploy` timed out after 30000 ms"
    );
    assert!(waited >= TIMEOUT, "waited {waited:?}");
    assert_eq!(deploy.arguments(), NO_CALLS, "the call never ran");
    dropped.await.expect_err("the pending decision was dropped");
    next.result
        .as_ref()
        .expect("the timed-out turn released the session");
    let transcript = session
        .transcript(TurnRange::All)
        .await
        .expect("transcript");
    assert_eq!(
        transcript[0].error.as_ref().map(|failure| failure.kind),
        Some(ErrorKind::ApprovalTimeout)
    );
}

#[tokio::test]
async fn cancelling_a_run_waiting_for_approval_stops_it_promptly() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let (approver, dropped) = Approver::never();
    let (model, _requests) = scripted(vec![ModelResponse::ToolCall(call(
        "deploy_0",
        "deploy",
        r#"{"target":"prod"}"#,
    ))]);
    let harness = harness(model, tool_set(vec![deploy.clone()]), approver.clone());
    let handle = harness.run(request()).into_handle();
    let cancel = handle.cancel_token();
    let outcome = tokio::spawn(handle.outcome());

    // Act
    approver.asked.notified().await;
    cancel.cancel();
    let outcome = tokio::time::timeout(Duration::from_secs(5), outcome)
        .await
        .expect("the run stopped promptly")
        .expect("run task");

    // Assert
    assert_eq!(
        outcome.result.expect_err("cancelled").kind(),
        ErrorKind::Cancelled
    );
    tokio::time::timeout(Duration::from_secs(5), dropped)
        .await
        .expect("the pending decision was dropped")
        .expect_err("dropped without a decision");
    assert_eq!(deploy.arguments(), NO_CALLS);
}

#[tokio::test(start_paused = true)]
async fn a_decision_slower_than_the_turn_lease_keeps_the_turn_owned() {
    // Arrange
    let lease = Duration::from_secs(TURN_LEASE_SECONDS.unsigned_abs());
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let approver = Approver::new(Behavior::AllowAfter(lease * 2));
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCall(call("deploy_0", "deploy", r#"{"target":"prod"}"#)),
        output(),
        output(),
    ]);
    let harness = builder(model, tool_set(vec![deploy.clone()]), approver)
        .approval_timeout(lease * 3)
        .store(Arc::new(MemoryStore::new()))
        .build()
        .expect("harness");
    let session = harness
        .session("slow", object_schema())
        .create()
        .await
        .expect("session");

    // Act
    let slow = session.run(RunRequest::new("deploy it")).await;
    let next = session.run(RunRequest::new("answer")).await;

    // Assert
    assert!(
        TIMEOUT < lease,
        "the suite's timeout ends a wait within one lease"
    );
    slow.result
        .as_ref()
        .expect("the renewed lease outlives the wait");
    assert_eq!(deploy.arguments(), [json!({"target": "prod"})]);
    next.result.as_ref().expect("the session runs again");
}

#[tokio::test(start_paused = true)]
async fn the_run_wall_time_limit_also_bounds_the_wait() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Executes);
    let (approver, _dropped) = Approver::never();
    let (model, _requests) = scripted(vec![ModelResponse::ToolCall(call(
        "deploy_0",
        "deploy",
        r#"{"target":"prod"}"#,
    ))]);
    let harness = harness(model, tool_set(vec![deploy]), approver);
    let wall_time = TIMEOUT / 3;

    // Act
    let outcome = harness
        .run(request().limits(RunLimits::default().with_max_wall_time(wall_time)))
        .await;

    // Assert
    assert_eq!(
        outcome.result.expect_err("wall time reached").kind(),
        ErrorKind::BudgetExhausted(LimitKind::WallTime)
    );
}

#[tokio::test]
async fn a_one_shot_turn_asks_without_a_run_or_events() {
    // Arrange
    let deploy = Echo::new("deploy", ToolEffects::Writes);
    let approver = Approver::deciding(Decision::Allow);
    let (model, _requests) = scripted(vec![
        ModelResponse::ToolCall(call("deploy_0", "deploy", r#"{"target":"prod"}"#)),
        output(),
    ]);
    let tools = tool_set(vec![deploy.clone()]);
    let harness = harness(model, tools.clone(), approver.clone());

    // Act
    let outcome = harness
        .turn(
            "deploy it",
            TurnOptions::from_output(object_schema()).with_tools(tools),
        )
        .start()
        .await;

    // Assert
    outcome.expect("turn output");
    let requests = approver.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].run, None);
    assert_eq!(requests[0].session, None);
    assert_eq!(deploy.arguments(), [json!({"target": "prod"})]);
}
