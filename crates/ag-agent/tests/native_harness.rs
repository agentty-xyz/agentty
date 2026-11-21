//! Public contract coverage for in-process `ag-harness` session turns and
//! utility runs against a scripted Chat Completions provider.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ag_agent::{
    NativeHarnessConfig, RealOneShotClient, create_agent_channel, instruction_bootstrap_key,
};
use ag_contracts::{
    ActivityKind, ActivityStatus, AgentChannel, AgentDefinitionPrompt, AgentError,
    AgentRequestKind, ContextCompaction, ExecutionPolicy, LiveTranscript, McpPolicy, OneShotClient,
    OneShotRequest, OutputEvent, OutputKind, PermissionMode, ProviderCallBudget, ReasoningLevel,
    ResponseStyle, SpeedMode, TurnContinuation, TurnEvent, TurnNotice, TurnRequest,
};
use ag_harness::bash::Enforcement;
use ag_harness::provider::ModelProvider;
use ag_harness::store::SQLITE_BUSY_TIMEOUT;
use ag_harness::tool::ToolDefinition;
use ag_harness::{Cost, Usage};
use ag_protocol::{TurnPrompt, TurnPromptAttachment};
use ag_session::AgentKind;
use ag_telemetry::{KeyValue, Span};
use opentelemetry::global;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const CLAUDE_MODEL: &str = "claude-opus-5-5";
const MUSE_MODEL: &str = "muse-spark-1.3";
const QWEN_MODEL: &str = "qwen-plus";
/// Locally served model whose declared 128,000-token window is far smaller
/// than the cataloged models' windows.
const FALLBACK_BUDGET_MODEL: &str = "openai-compatible/budget-model";
/// Name the local server serves [`FALLBACK_BUDGET_MODEL`] under.
const FALLBACK_BUDGET_SERVED_NAME: &str = "budget-model";
/// Heading present only in a full instruction bootstrap.
const FULL_CONTRACT: &str = "Structured response protocol:";
/// Heading present only in a compact refresh reminder.
const REFRESH_REMINDER: &str = "Protocol refresh reminder:";

/// Returns provider credentials pointing every provider at `server`.
fn environment(server: &MockServer) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("DASHSCOPE_API_KEY".to_string(), "qwen-key".to_string()),
        ("DASHSCOPE_BASE_URL".to_string(), server.uri()),
        ("HOME".to_string(), "/tmp".to_string()),
        ("MODEL_API_BASE_URL".to_string(), server.uri()),
        ("MODEL_API_KEY".to_string(), "muse-key".to_string()),
        ("OPENAI_COMPATIBLE_BASE_URL".to_string(), server.uri()),
        (
            "OPENAI_COMPATIBLE_CONTEXT_WINDOW".to_string(),
            "128000".to_string(),
        ),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
    ])
}

/// Builds a harness configuration over a fixed environment snapshot.
fn config(data_root: &Path, environment: BTreeMap<String, String>) -> NativeHarnessConfig {
    NativeHarnessConfig::with_environment(data_root.to_path_buf(), move || environment.clone())
}

/// Builds a session turn request for `folder`.
fn turn_request(folder: &Path, model: &str, prompt: &str) -> TurnRequest {
    TurnRequest {
        context_compaction: ContextCompaction::Off,
        provider_call_budget: None,
        continuation: TurnContinuation::fresh(),
        execution_policy: ExecutionPolicy::default(),
        folder: folder.to_path_buf(),
        main_checkout_root: None,
        model: model.to_string(),
        permission_mode: PermissionMode::AutoEdit,
        agent_profile: AgentDefinitionPrompt::default(),
        prompt: TurnPrompt::from_text(prompt.to_string()),
        reasoning_level: ReasoningLevel::Low,
        request_kind: AgentRequestKind::SessionStart,
        response_style: ResponseStyle::Concise,
        speed_mode: SpeedMode::Normal,
    }
}

/// Returns host continuation carrying the stored provider conversation id.
fn continuing(provider_conversation_id: Option<String>) -> TurnContinuation {
    TurnContinuation::provider(None, None, provider_conversation_id, None)
}

/// Live transcript fixture returning fixed replay text.
#[derive(Debug)]
struct LiveText(&'static str);

impl LiveTranscript for LiveText {
    fn replay_text(&self) -> Option<String> {
        Some(self.0.to_string())
    }
}

/// Builds a utility request for `folder`.
fn utility_request(folder: &Path, budget: Option<ProviderCallBudget>) -> OneShotRequest {
    OneShotRequest {
        activity_tx: None,
        attachments: Vec::new(),
        child_pid: None,
        execution_policy: ExecutionPolicy::default(),
        folder: folder.to_path_buf(),
        harness: AgentKind::Harness.to_string(),
        model: MUSE_MODEL.to_string(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "Name this session".to_string(),
        provider_call_budget: budget,
        reasoning_level: ReasoningLevel::Low,
        request_kind: AgentRequestKind::UtilityPrompt,
        speed_mode: SpeedMode::Normal,
    }
}

/// Returns a terminal structured answer with token usage.
fn answer(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"answer": text, "questions": []}).to_string()}
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 7, "total_tokens": 37}
    }))
}

/// Returns a terminal utility answer.
fn utility_answer(text: &str) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({"answer": text}).to_string()}
        }],
        "usage": {"prompt_tokens": 30, "completion_tokens": 7, "total_tokens": 37}
    }))
}

/// Returns one native tool-call response.
fn tool_call(id: &str, name: &str, arguments: &Value) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "tool_calls",
            "message": {
                "content": null,
                "tool_calls": [{
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": arguments.to_string()}
                }]
            }
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}
    }))
}

/// Mounts one response for requests whose body contains `marker`.
async fn respond(server: &MockServer, marker: &str, response: ResponseTemplate, priority: u8) {
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(body_string_contains(marker))
        .respond_with(response)
        .with_priority(priority)
        .mount(server)
        .await;
}

/// Returns the bodies of every provider request received so far.
async fn request_bodies(server: &MockServer) -> Vec<String> {
    chat_requests(server)
        .await
        .iter()
        .map(|request: &Request| String::from_utf8_lossy(&request.body).into_owned())
        .collect()
}

/// Returns the chat requests `server` received, without the model-listing
/// probes a local server also answers.
async fn chat_requests(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|request| request.url.path().ends_with("/chat/completions"))
        .collect()
}

/// Runs one session turn and collects its streamed events.
async fn run_turn(
    channel: &Arc<dyn AgentChannel>,
    session_id: &str,
    request: TurnRequest,
) -> (Result<ag_contracts::TurnResult, String>, Vec<TurnEvent>) {
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let result = channel
        .run_turn(session_id.to_string(), request, events_tx)
        .await
        .map_err(|error| error.to_string());
    let mut events = Vec::new();
    while let Ok(event) = events_rx.try_recv() {
        events.push(event);
    }

    (result, events)
}

#[tokio::test]
async fn session_turn_answers_and_resumes_with_stored_history() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Second question", answer("Second answer"), 1).await;
    respond(&server, "First question", answer("First answer"), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (first, first_events) = run_turn(
        &channel,
        "session-a",
        turn_request(worktree.path(), MUSE_MODEL, "First question"),
    )
    .await;
    let first = first.expect("first turn should succeed");
    let mut resume = turn_request(worktree.path(), MUSE_MODEL, "Second question");
    resume.request_kind = AgentRequestKind::SessionResume;
    resume.continuation = continuing(first.provider_conversation_id.clone());
    let (second, _) = run_turn(&channel, "session-a", resume).await;

    // Assert
    assert_eq!(first.assistant_message.answer, "First answer");
    assert_eq!((first.input_tokens, first.output_tokens), (30, 7));
    let report: Value =
        serde_json::from_str(first.harness_report.as_deref().expect("harness report"))
            .expect("report JSON");
    assert_eq!(report["model_requests"].as_array().map(Vec::len), Some(1));
    let bootstrap_key = instruction_bootstrap_key(Some("session-a")).expect("bootstrap key");
    assert!(
        first
            .provider_conversation_id
            .as_deref()
            .is_some_and(|id| id.starts_with(&format!("{bootstrap_key}#")))
    );
    assert!(first_events.contains(&TurnEvent::ThoughtDelta(format!(
        "Waiting for {MUSE_MODEL}"
    ))));
    let second = second.expect("second turn should succeed");
    assert_eq!(second.assistant_message.answer, "Second answer");
    assert_eq!(
        second.provider_conversation_id,
        first.provider_conversation_id
    );
    let bodies = request_bodies(&server).await;
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("First answer"));
    assert_eq!(bodies[1].matches(FULL_CONTRACT).count(), 1);
    assert!(bodies[1].contains(REFRESH_REMINDER));
    assert!(data_root.path().join("session-a/harness.db").is_file());
    assert!(
        channel
            .start_session(ag_contracts::StartSessionRequest {
                folder: worktree.path().to_path_buf(),
                session_id: "session-a".to_string(),
            })
            .await
            .is_ok()
    );
}

/// Returns one streamed Chat Completions response carrying `chunks` as its
/// deltas.
fn chat_stream(chunks: &[Value]) -> ResponseTemplate {
    let finished = json!({
        "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 30, "completion_tokens": 7, "total_tokens": 37}
    });
    let mut body = chunks
        .iter()
        .chain([&finished])
        .fold(String::new(), |mut body, chunk| {
            let _ = write!(body, "data: {chunk}\n\n");

            body
        });
    body.push_str("data: [DONE]\n\n");

    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Returns one streamed Chat Completions delta.
fn chat_delta(delta: &Value) -> Value {
    json!({"choices": [{"index": 0, "delta": delta}]})
}

#[tokio::test]
async fn streamed_session_turn_forwards_its_output_before_the_answer() {
    // Arrange
    let server = MockServer::start().await;
    let response = json!({"answer": "Streamed answer", "questions": []}).to_string();
    let (head, tail) = response.split_at(response.len() / 2);
    respond(
        &server,
        "Stream it",
        chat_stream(&[
            chat_delta(&json!({"reasoning_content": "Checking."})),
            chat_delta(&json!({"content": head})),
            chat_delta(&json!({"content": tail})),
        ]),
        1,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (result, events) = run_turn(
        &channel,
        "session-stream",
        turn_request(worktree.path(), MUSE_MODEL, "Stream it"),
    )
    .await;

    // Assert
    let result = result.expect("streamed turn should succeed");
    assert_eq!(result.assistant_message.answer, "Streamed answer");
    let outputs = events
        .into_iter()
        .filter_map(|event| match event {
            TurnEvent::Output(output) => Some(output),
            _ => None,
        })
        .collect::<Vec<_>>();
    let streamed = outputs
        .iter()
        .filter_map(|output| match output {
            OutputEvent::Delta { id: 1, text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<String>();
    assert_eq!(streamed, "Streamed answer");
    assert_eq!(
        outputs.first(),
        Some(&OutputEvent::Started {
            id: 0,
            kind: OutputKind::Reasoning,
        })
    );
    assert!(outputs.contains(&OutputEvent::Completed {
        id: 0,
        text: "Checking.".to_string(),
    }));
    assert!(outputs.contains(&OutputEvent::Completed {
        id: 1,
        text: "Streamed answer".to_string(),
    }));
    assert_eq!(
        outputs.last(),
        Some(&OutputEvent::Answer {
            replaces: Some(1),
            text: "Streamed answer".to_string(),
        })
    );
}

#[tokio::test]
async fn traced_session_turn_exports_harness_spans_beneath_the_run() {
    // Arrange
    let exporter = InMemorySpanExporter::default();
    global::set_tracer_provider(
        SdkTracerProvider::builder()
            .with_simple_exporter(exporter.clone())
            .build(),
    );
    let server = MockServer::start().await;
    respond(
        &server,
        r#""tool_call_id":"call-read""#,
        answer("Traced answer"),
        1,
    )
    .await;
    respond(
        &server,
        "Traced question",
        tool_call("call-read", "read", &json!({"path": "missing.txt"})),
        2,
    )
    .await;
    respond(&server, "Untraced question", answer("Untraced answer"), 3).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (untraced, _) = run_turn(
        &channel,
        "session-untraced",
        turn_request(worktree.path(), MUSE_MODEL, "Untraced question"),
    )
    .await;
    let untraced_spans = exporter.get_finished_spans().expect("spans").len();
    let mut run = Span::root("agent.run", Vec::new());
    run.identify("agentty.session.id", "session-traced");
    run.identify("gen_ai.request.model", MUSE_MODEL);
    run.identify("agentty.harness", "harness");
    let (traced, _) = run
        .scope(run_turn(
            &channel,
            "session-traced",
            turn_request(worktree.path(), MUSE_MODEL, "Traced question"),
        ))
        .await;

    // Assert
    untraced.expect("untraced turn should succeed");
    traced.expect("traced turn should succeed");
    assert_eq!(untraced_spans, 0);
    let spans = exporter.get_finished_spans().expect("spans");
    let run = spans
        .iter()
        .find(|span| span.name == "agent.run")
        .expect("run span");
    let turn = spans
        .iter()
        .find(|span| span.name == "invoke_agent")
        .expect("harness turn span");
    assert_eq!(turn.parent_span_id, run.span_context.span_id());
    let chat = spans
        .iter()
        .find(|span| span.name == format!("chat {MUSE_MODEL}"))
        .expect("model request span");
    assert_eq!(chat.parent_span_id, turn.span_context.span_id());
    let tool = spans
        .iter()
        .find(|span| span.name == "execute_tool read")
        .expect("tool span");
    assert_eq!(tool.parent_span_id, turn.span_context.span_id());
    for attribute in [
        KeyValue::new("agentty.session.id", "session-traced"),
        KeyValue::new("gen_ai.request.model", MUSE_MODEL),
        KeyValue::new("agentty.harness", "harness"),
        KeyValue::new("gen_ai.tool.name", "read"),
    ] {
        assert!(tool.attributes.contains(&attribute), "{attribute:?}");
    }
}

#[tokio::test]
async fn stale_instruction_key_resends_the_full_session_contract() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Second question", answer("Second answer"), 1).await;
    respond(&server, "First question", answer("First answer"), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (first, _) = run_turn(
        &channel,
        "session-stale",
        turn_request(worktree.path(), MUSE_MODEL, "First question"),
    )
    .await;
    first.expect("first turn should succeed");
    let mut resume = turn_request(worktree.path(), MUSE_MODEL, "Second question");
    resume.request_kind = AgentRequestKind::SessionResume;
    resume.continuation = continuing(Some("v1:0000000000000000:13:session-stale".to_string()));

    // Act
    let (second, _) = run_turn(&channel, "session-stale", resume).await;

    // Assert
    second.expect("second turn should succeed");
    let bodies = request_bodies(&server).await;
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("First answer"));
    assert_eq!(bodies[1].matches(FULL_CONTRACT).count(), 2);
    assert!(!bodies[1].contains(REFRESH_REMINDER));
}

#[tokio::test]
async fn session_provider_budget_is_enforced_by_the_native_harness() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Budgeted question", answer("Answer"), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let budget = ProviderCallBudget::new(1);
    let mut first = turn_request(worktree.path(), MUSE_MODEL, "Budgeted question");
    first.provider_call_budget = Some(budget.clone());

    // Act
    let (result, _) = run_turn(&channel, "budgeted-session", first).await;
    let mut second = turn_request(worktree.path(), MUSE_MODEL, "Budgeted question again");
    second.provider_call_budget = Some(budget.clone());
    let (exhausted, _) = run_turn(&channel, "budgeted-session", second).await;

    // Assert
    result.expect("initial call succeeds");
    assert!(exhausted.is_err());
    assert_eq!(budget.remaining(), 0);
    assert_eq!(request_bodies(&server).await.len(), 1);
}

#[tokio::test]
async fn evicted_bootstrap_turn_resends_the_full_session_contract() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Third question", answer("Third answer"), 1).await;
    respond(&server, "Second question", answer("Second answer"), 2).await;
    respond(&server, "First question", answer("First answer"), 3).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let resume_request = |prompt: String, provider_conversation_id: Option<String>| {
        let mut request = turn_request(worktree.path(), FALLBACK_BUDGET_MODEL, &prompt);
        request.context_compaction = automatic(None);
        request.request_kind = AgentRequestKind::SessionResume;
        request.continuation = continuing(provider_conversation_id);

        request
    };
    // Each large turn fits the history byte budget alone, but the second
    // turn's input leaves too little context budget to replay the first.
    let (first, _) = run_turn(
        &channel,
        "session-evict",
        turn_request(
            worktree.path(),
            FALLBACK_BUDGET_MODEL,
            &format!("First question {}", "x".repeat(220_000)),
        ),
    )
    .await;
    let first = first.expect("first turn should succeed");
    let (second, second_events) = run_turn(
        &channel,
        "session-evict",
        resume_request(
            format!("Second question {}", "y".repeat(230_000)),
            first.provider_conversation_id,
        ),
    )
    .await;
    let second = second.expect("second turn should succeed");

    // Act
    let (third, _) = run_turn(
        &channel,
        "session-evict",
        resume_request(
            "Third question".to_string(),
            second.provider_conversation_id.clone(),
        ),
    )
    .await;

    // Assert
    third.expect("third turn should succeed");
    // The second turn's compaction failed on the protocol answer, so it
    // evicted history instead, and the third turn backs off from retrying.
    assert!(second_events.iter().any(|event| matches!(
        event,
        TurnEvent::Notice(TurnNotice::ContextCompactionFailed { .. })
    )));
    assert_eq!(
        second.provider_conversation_id.as_deref(),
        Some("session-evict")
    );
    let bodies = request_bodies(&server)
        .await
        .into_iter()
        .filter(|body| !body.contains(COMPACTION_SOURCE))
        .collect::<Vec<_>>();
    assert_eq!(bodies.len(), 3);
    assert!(!bodies[1].contains("First answer"));
    assert!(bodies[1].contains(REFRESH_REMINDER));
    assert!(!bodies[2].contains("First answer"));
    assert_eq!(bodies[2].matches(FULL_CONTRACT).count(), 1);
    // Only the replayed second turn carries a reminder.
    assert_eq!(bodies[2].matches(REFRESH_REMINDER).count(), 1);
}

#[tokio::test]
async fn switching_to_a_smaller_window_fits_history_to_the_new_budget() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Third question", answer("Third answer"), 1).await;
    respond(&server, "Second question", answer("Second answer"), 2).await;
    respond(&server, "First question", answer("First answer"), 3).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let resume_request = |model: &str, prompt: String, provider_conversation_id: Option<String>| {
        let mut request = turn_request(worktree.path(), model, &prompt);
        request.request_kind = AgentRequestKind::SessionResume;
        request.continuation = continuing(provider_conversation_id);

        request
    };
    // Both large turns fit the cataloged model's window together, but not
    // the fallback budget.
    let (first, _) = run_turn(
        &channel,
        "session-shrink",
        turn_request(
            worktree.path(),
            MUSE_MODEL,
            &format!("First question {}", "x".repeat(220_000)),
        ),
    )
    .await;
    let first = first.expect("first turn should succeed");
    let (second, _) = run_turn(
        &channel,
        "session-shrink",
        resume_request(
            MUSE_MODEL,
            format!("Second question {}", "y".repeat(230_000)),
            first.provider_conversation_id,
        ),
    )
    .await;
    let second = second.expect("second turn should succeed");

    // Act
    let (third, _) = run_turn(
        &channel,
        "session-shrink",
        resume_request(
            FALLBACK_BUDGET_MODEL,
            "Third question".to_string(),
            second.provider_conversation_id,
        ),
    )
    .await;

    // Assert
    third.expect("third turn should succeed on the smaller window");
    let bodies = request_bodies(&server).await;
    assert_eq!(bodies.len(), 3);
    assert!(bodies[1].contains("First answer"));
    assert!(bodies[2].contains(&format!(r#""model":"{FALLBACK_BUDGET_SERVED_NAME}""#)));
    assert!(!bodies[2].contains("First answer"));
    assert!(bodies[2].contains("Second answer"));
}

#[tokio::test]
async fn image_attachments_are_rejected_before_any_provider_call() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let mut request = turn_request(worktree.path(), MUSE_MODEL, "Explain [Image #1]");
    request.prompt.attachments = vec![TurnPromptAttachment {
        local_image_path: data_root.path().join("image.png"),
        placeholder: "[Image #1]".to_string(),
    }];
    let client = RealOneShotClient::pooled(None).with_native_harness(config.clone());
    let mut utility = utility_request(worktree.path(), None);
    utility.attachments.clone_from(&request.prompt.attachments);

    // Act
    let (result, _) = run_turn(&channel, "session-image", request).await;
    let utility_result = client.submit(utility).await;

    // Assert
    let error = result.expect_err("image attachments should be rejected");
    assert!(error.contains("do not support image attachments"));
    assert!(
        utility_result
            .expect_err("utility image attachments should be rejected")
            .to_string()
            .contains("do not support image attachments")
    );
    assert_eq!(request_bodies(&server).await, Vec::<String>::new());
    assert!(!data_root.path().join("session-image").exists());
}

#[tokio::test]
async fn utility_turn_on_a_session_gets_the_full_utility_contract() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Resolve conflicts", utility_answer("Resolved"), 1).await;
    respond(&server, "First question", answer("First answer"), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (first, _) = run_turn(
        &channel,
        "session-assist",
        turn_request(worktree.path(), MUSE_MODEL, "First question"),
    )
    .await;
    let first = first.expect("first turn should succeed");
    let mut assist = turn_request(worktree.path(), MUSE_MODEL, "Resolve conflicts");
    assist.request_kind = AgentRequestKind::UtilityPrompt;
    assist.continuation = continuing(first.provider_conversation_id);

    // Act
    let (assisted, _) = run_turn(&channel, "session-assist", assist).await;

    // Assert
    let assisted = assisted.expect("utility turn should succeed");
    assert_eq!(assisted.assistant_message.answer, "Resolved");
    assert_eq!(
        assisted.provider_conversation_id.as_deref(),
        Some("session-assist")
    );
    let bodies = request_bodies(&server).await;
    assert_eq!(bodies.len(), 2);
    assert!(bodies[1].contains("For this utility request, return only"));
    assert!(!bodies[1].contains(REFRESH_REMINDER));
}

#[tokio::test]
async fn edit_mode_turn_reads_writes_and_runs_bash_without_provider_secrets() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, r#""tool_call_id":"call-bash""#, answer("Done"), 1).await;
    respond(
        &server,
        r#""tool_call_id":"call-read""#,
        tool_call(
            "call-bash",
            "bash",
            &json!({"command": "cat notes.txt; env"}),
        ),
        2,
    )
    .await;
    respond(
        &server,
        r#""tool_call_id":"call-write""#,
        tool_call("call-read", "read", &json!({"path": "missing.txt"})),
        3,
    )
    .await;
    respond(
        &server,
        "Create notes",
        tool_call(
            "call-write",
            "write",
            &json!({"path": "notes.txt", "patch": "--- /dev/null\n+++ b/notes.txt\n@@ -0,0 +1 @@\n+hello harness\n"}),
        ),
        4,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let mut environment = environment(&server);
    environment.insert("ANTHROPIC_API_KEY".to_string(), "sk-ant-secret".to_string());
    environment.insert(
        "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
        "claude-oauth-secret".to_string(),
    );
    let config = config(data_root.path(), environment);
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (result, events) = run_turn(
        &channel,
        "session-tools",
        turn_request(worktree.path(), MUSE_MODEL, "Create notes"),
    )
    .await;

    // Assert
    assert_eq!(
        result
            .expect("tool turn should succeed")
            .assistant_message
            .answer,
        "Done"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.path().join("notes.txt")).expect("written file"),
        "hello harness\n"
    );
    let activity = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::Activity(activity) => {
                Some((activity.name.as_str(), activity.kind, activity.status))
            }
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        activity,
        [
            ("write", ActivityKind::FileChange, ActivityStatus::Running),
            ("write", ActivityKind::FileChange, ActivityStatus::Completed),
            ("read", ActivityKind::Tool, ActivityStatus::Running),
            ("read", ActivityKind::Tool, ActivityStatus::Failed),
            ("bash", ActivityKind::Command, ActivityStatus::Running),
            ("bash", ActivityKind::Command, ActivityStatus::Completed),
        ]
    );
    let bodies = request_bodies(&server).await;
    let unsandboxed = json!(ToolDefinition::bash(Enforcement::None).description()).to_string();
    assert!(
        bodies[0].contains(&unsandboxed),
        "the edit-mode bash tool must state that commands run unsandboxed"
    );
    let bash_result = bodies
        .iter()
        .find(|body| body.contains(r#""tool_call_id":"call-bash""#))
        .expect("bash result should reach the model");
    assert!(bash_result.contains("hello harness"));
    assert!(bash_result.contains("PATH="));
    assert!(!bash_result.contains("muse-key"));
    assert!(!bash_result.contains("MODEL_API_KEY"));
    assert!(!bash_result.contains("sk-ant-secret"));
    assert!(!bash_result.contains("claude-oauth-secret"));
}

#[tokio::test]
async fn read_only_turn_answers_writes_as_unavailable_and_continues() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        r#""tool_call_id":"call-write""#,
        answer("Left the notes unchanged"),
        1,
    )
    .await;
    respond(
        &server,
        "Edit something",
        tool_call(
            "call-write",
            "write",
            &json!({"path": "notes.txt", "patch": "--- /dev/null\n+++ b/notes.txt\n"}),
        ),
        2,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let mut request = turn_request(worktree.path(), MUSE_MODEL, "Edit something");
    request.permission_mode = PermissionMode::ReadOnly;

    // Act
    let (result, events) = run_turn(&channel, "session-read-only", request).await;

    // Assert
    let result = result.expect("a denied write is answered and the turn continues");
    assert_eq!(result.assistant_message.answer, "Left the notes unchanged");
    assert!(!worktree.path().join("notes.txt").exists());
    let bodies = request_bodies(&server).await;
    assert!(
        bodies
            .iter()
            .any(|body| body.contains("tool `write` is not available in this turn")),
        "{bodies:?}"
    );
    let activity = events
        .iter()
        .filter_map(|event| match event {
            TurnEvent::Activity(activity) => Some((activity.name.as_str(), activity.status)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        activity,
        [
            ("write", ActivityStatus::Running),
            ("write", ActivityStatus::Failed)
        ]
    );
}

#[tokio::test]
async fn missing_history_restarts_from_the_replay_transcript() {
    for request_kind in [
        AgentRequestKind::SessionStart,
        AgentRequestKind::SessionResume,
    ] {
        // Arrange
        let server = MockServer::start().await;
        respond(&server, "Continue please", answer("Continued"), 1).await;
        let data_root = tempfile::tempdir().expect("data root");
        let worktree = tempfile::tempdir().expect("worktree");
        let config = config(data_root.path(), environment(&server));
        let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
        let mut request = turn_request(worktree.path(), MUSE_MODEL, "Continue please");
        request.request_kind = request_kind;
        request.continuation =
            TurnContinuation::replaying("Earlier we renamed parse_config".into());

        // Act
        let (result, _) = run_turn(&channel, "session-replay", request).await;

        // Assert
        assert_eq!(
            result
                .expect("replayed turn should succeed")
                .assistant_message
                .answer,
            "Continued"
        );
        let bodies = request_bodies(&server).await;
        assert!(bodies[0].contains("Earlier we renamed parse_config"));
    }
}

#[tokio::test]
async fn missing_history_restarts_from_the_live_transcript() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Continue please", answer("Continued"), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let mut request = turn_request(worktree.path(), MUSE_MODEL, "Continue please");
    request.request_kind = AgentRequestKind::SessionResume;
    request.continuation = TurnContinuation::provider(
        Some(Arc::new(LiveText("Earlier we renamed parse_config"))),
        None,
        None,
        None,
    );

    // Act
    let (result, _) = run_turn(&channel, "session-live", request).await;

    // Assert
    result.expect("replayed turn should succeed");
    let bodies = request_bodies(&server).await;
    assert!(bodies[0].contains("Earlier we renamed parse_config"));
}

#[tokio::test]
async fn turn_failing_before_it_is_recorded_keeps_the_bootstrap_for_the_next_turn() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Continue please", answer("Continued"), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let resume_request = |prompt: &str| {
        let mut request = turn_request(worktree.path(), MUSE_MODEL, prompt);
        request.request_kind = AgentRequestKind::SessionResume;
        request.continuation =
            TurnContinuation::replaying("Earlier we renamed parse_config".into());

        request
    };
    let (oversized, _) = run_turn(
        &channel,
        "session-retry",
        resume_request(&"x".repeat(4 * 1024 * 1024)),
    )
    .await;

    // Act
    let (result, _) = run_turn(&channel, "session-retry", resume_request("Continue please")).await;

    // Assert
    assert!(oversized.is_err(), "the oversized turn should be rejected");
    assert!(data_root.path().join("session-retry/harness.db").is_file());
    assert_eq!(
        result
            .expect("retried turn should succeed")
            .assistant_message
            .answer,
        "Continued"
    );
    let bodies = request_bodies(&server).await;
    assert_eq!(bodies.len(), 1);
    assert!(bodies[0].contains("Earlier we renamed parse_config"));
}

#[tokio::test]
async fn model_change_switches_the_stored_session_with_only_the_target_key() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Ask Qwen", answer("From Qwen"), 1).await;
    respond(&server, "Ask Muse", answer("From Muse"), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let mut qwen_only = environment(&server);
    qwen_only.retain(|name, _| !name.starts_with("MODEL_"));
    let qwen_config = config(data_root.path(), qwen_only);
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (first, _) = run_turn(
        &channel,
        "session-switch",
        turn_request(worktree.path(), MUSE_MODEL, "Ask Muse"),
    )
    .await;
    first.expect("first turn should succeed");
    let qwen_channel = create_agent_channel(AgentKind::Harness, None, Some(&qwen_config));

    // Act
    let mut request = turn_request(worktree.path(), QWEN_MODEL, "Ask Qwen");
    request.request_kind = AgentRequestKind::SessionResume;
    let (second, _) = run_turn(&qwen_channel, "session-switch", request).await;

    // Assert
    assert_eq!(
        second
            .expect("switched turn should succeed")
            .assistant_message
            .answer,
        "From Qwen"
    );
    let bodies = request_bodies(&server).await;
    assert!(bodies[1].contains(&format!(r#""model":"{QWEN_MODEL}""#)));
}

/// Returns a keyless environment pointing the OpenAI-compatible provider at
/// `server` and naming `served_model`.
fn local_environment(server: &MockServer, served_model: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".to_string(), "/tmp".to_string()),
        ("OPENAI_COMPATIBLE_BASE_URL".to_string(), server.uri()),
        (
            "OPENAI_COMPATIBLE_CONTEXT_WINDOW".to_string(),
            "40960".to_string(),
        ),
        (
            "OPENAI_COMPATIBLE_MODEL".to_string(),
            served_model.to_string(),
        ),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
    ])
}

#[tokio::test]
async fn openai_compatible_turn_reads_without_a_key_and_follows_the_served_model() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Ask llama", answer("From llama"), 1).await;
    respond(
        &server,
        r#""tool_call_id":"call-read""#,
        answer("Notes read"),
        2,
    )
    .await;
    respond(
        &server,
        "Read the notes",
        tool_call("call-read", "read", &json!({"path": "notes.txt"})),
        3,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    std::fs::write(worktree.path().join("notes.txt"), "local notes\n").expect("notes");
    let qwen_config = config(data_root.path(), local_environment(&server, "qwen3:latest"));
    let llama_config = config(data_root.path(), local_environment(&server, "llama3.2"));
    let qwen_channel = create_agent_channel(AgentKind::Harness, None, Some(&qwen_config));
    let llama_channel = create_agent_channel(AgentKind::Harness, None, Some(&llama_config));

    // Act
    let (first, _) = run_turn(
        &qwen_channel,
        "session-local",
        turn_request(worktree.path(), "openai-compatible", "Read the notes"),
    )
    .await;
    let mut follow_up = turn_request(worktree.path(), "openai-compatible", "Ask llama");
    follow_up.request_kind = AgentRequestKind::SessionResume;
    let (second, _) = run_turn(&llama_channel, "session-local", follow_up).await;

    // Assert
    assert_eq!(
        first
            .expect("local turn should succeed")
            .assistant_message
            .answer,
        "Notes read"
    );
    assert_eq!(
        second
            .expect("served-model change should switch the session")
            .assistant_message
            .answer,
        "From llama"
    );
    let requests = server.received_requests().await.unwrap_or_default();
    assert!(
        requests
            .iter()
            .all(|request| !request.headers.contains_key("authorization"))
    );
    assert_eq!(chat_requests(&server).await.len(), 3);
    let bodies = request_bodies(&server).await;
    assert!(bodies[0].contains(r#""model":"qwen3:latest""#));
    assert!(bodies[1].contains("local notes"));
    assert!(bodies[2].contains(r#""model":"llama3.2""#));
    assert!(bodies.iter().all(|body| !body.contains("prompt_cache_key")));
    assert!(bodies.iter().all(|body| !body.contains("reasoning_effort")));
}

/// Returns one streamed Responses API response completing with `items`.
fn responses_stream(items: &[Value]) -> ResponseTemplate {
    let mut events = items
        .iter()
        .map(|item| json!({"type": "response.output_item.done", "item": item}))
        .collect::<Vec<_>>();
    events.push(json!({
        "type": "response.completed",
        "response": {
            "id": "resp_turn",
            "status": "completed",
            "usage": {
                "input_tokens": 40,
                "input_tokens_details": {"cached_tokens": 32},
                "output_tokens": 8,
                "total_tokens": 48
            }
        }
    }));
    let body = events.iter().fold(String::new(), |mut body, event| {
        let _ = write!(body, "data: {event}\n\n");

        body
    });

    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Mounts one streamed Responses API response for requests containing
/// `marker`.
async fn respond_streamed(server: &MockServer, marker: &str, items: &[Value], priority: u8) {
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(body_string_contains(marker))
        .respond_with(responses_stream(items))
        .with_priority(priority)
        .mount(server)
        .await;
}

/// Returns an `OpenAI` key and an endpoint at `server`.
fn openai_environment(server: &MockServer) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".to_string(), "/tmp".to_string()),
        ("OPENAI_API_KEY".to_string(), "sk-openai".to_string()),
        (
            "OPENAI_BASE_URL".to_string(),
            format!("{}/v1", server.uri()),
        ),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
    ])
}

#[tokio::test]
async fn openai_turn_streams_responses_and_replays_reasoning_with_tool_results() {
    // Arrange
    let server = MockServer::start().await;
    let reasoning = json!({
        "type": "reasoning",
        "id": "rs_turn",
        "summary": [],
        "encrypted_content": "gAAAA-encrypted-turn-reasoning"
    });
    respond_streamed(
        &server,
        r#""type":"function_call_output""#,
        &[json!({
            "type": "message",
            "role": "assistant",
            "content": [{
                "type": "output_text",
                "text": json!({"answer": "Notes read", "questions": []}).to_string()
            }]
        })],
        1,
    )
    .await;
    respond_streamed(
        &server,
        "Read the notes",
        &[
            reasoning.clone(),
            json!({
                "type": "function_call",
                "call_id": "call_notes",
                "name": "read",
                "arguments": json!({"path": "notes.txt"}).to_string()
            }),
        ],
        2,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    std::fs::write(worktree.path().join("notes.txt"), "hosted notes\n").expect("notes");
    let channel = create_agent_channel(
        AgentKind::Harness,
        None,
        Some(&config(data_root.path(), openai_environment(&server))),
    );

    // Act
    let (result, _) = run_turn(
        &channel,
        "session-openai",
        turn_request(worktree.path(), "gpt-6.1-sol", "Read the notes"),
    )
    .await;

    // Assert
    let result = result.expect("OpenAI turn should succeed");
    assert_eq!(result.assistant_message.answer, "Notes read");
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("authorization")
            .is_some_and(|value| value == "Bearer sk-openai")
    }));
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<Value>(&request.body).expect("JSON body"))
        .collect::<Vec<_>>();
    for body in &bodies {
        assert_eq!(body["model"], "gpt-6.1-sol");
        assert_eq!(body["store"], false);
        assert_eq!(body["stream"], true);
        assert_eq!(body["reasoning"], json!({"effort": "low"}));
        assert_eq!(body["input"][0]["role"], "developer");
        assert!(
            body["prompt_cache_key"]
                .as_str()
                .is_some_and(|key| key.starts_with("ag-session-"))
        );
    }
    let replayed = bodies[1]["input"]
        .as_array()
        .expect("input items")
        .iter()
        .collect::<Vec<_>>();
    let reasoning_at = replayed
        .iter()
        .position(|item| **item == reasoning)
        .expect("the turn's reasoning should be replayed");
    assert_eq!(replayed[reasoning_at + 1]["type"], "function_call");
    assert_eq!(replayed[reasoning_at + 1]["call_id"], "call_notes");
    assert_eq!(replayed[reasoning_at + 2]["type"], "function_call_output");
    assert!(
        replayed[reasoning_at + 2]["output"]
            .as_str()
            .is_some_and(|output| output.contains("hosted notes"))
    );
}

/// Returns one streamed Anthropic Messages response of the whole content
/// `blocks`, stopped for `stop_reason`.
fn messages_stream(blocks: &[Value], stop_reason: &str) -> ResponseTemplate {
    let mut events = vec![json!({
        "type": "message_start",
        "message": {
            "id": "msg_turn",
            "model": CLAUDE_MODEL,
            "content": [],
            "usage": {"input_tokens": 8, "cache_read_input_tokens": 32, "output_tokens": 1}
        }
    })];
    for (index, block) in blocks.iter().enumerate() {
        let (started, deltas) = match block["type"].as_str() {
            Some("thinking") => (
                json!({"type": "thinking", "thinking": "", "signature": ""}),
                vec![
                    json!({"type": "thinking_delta", "thinking": block["thinking"]}),
                    json!({"type": "signature_delta", "signature": block["signature"]}),
                ],
            ),
            Some("tool_use") => (
                json!({"type": "tool_use", "id": block["id"], "name": block["name"], "input": {}}),
                vec![json!({
                    "type": "input_json_delta",
                    "partial_json": block["input"].to_string()
                })],
            ),
            _ => (
                json!({"type": "text", "text": ""}),
                vec![json!({"type": "text_delta", "text": block["text"]})],
            ),
        };
        events
            .push(json!({"type": "content_block_start", "index": index, "content_block": started}));
        events.extend(
            deltas.into_iter().map(
                |delta| json!({"type": "content_block_delta", "index": index, "delta": delta}),
            ),
        );
        events.push(json!({"type": "content_block_stop", "index": index}));
    }
    events.push(json!({
        "type": "message_delta",
        "delta": {"stop_reason": stop_reason},
        "usage": {"output_tokens": 8}
    }));
    events.push(json!({"type": "message_stop"}));
    let body = events.iter().fold(String::new(), |mut body, event| {
        let kind = event["type"].as_str().unwrap_or_default();
        let _ = write!(body, "event: {kind}\ndata: {event}\n\n");

        body
    });

    ResponseTemplate::new(200).set_body_raw(body, "text/event-stream")
}

/// Mounts one streamed Anthropic Messages response for requests containing
/// `marker`.
async fn respond_with_messages(
    server: &MockServer,
    marker: &str,
    response: ResponseTemplate,
    priority: u8,
) {
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(body_string_contains(marker))
        .respond_with(response)
        .with_priority(priority)
        .mount(server)
        .await;
}

/// Scripts a Messages server whose first turn opens with `thinking` and a
/// `read` of `notes.txt` before answering, and whose follow-up turn answers
/// directly.
async fn claude_notes_server(thinking: &Value) -> MockServer {
    let server = MockServer::start().await;
    let answer = |text: &str| {
        messages_stream(
            &[json!({
                "type": "text",
                "text": json!({"answer": text, "questions": []}).to_string()
            })],
            "end_turn",
        )
    };
    respond_with_messages(&server, "Summarize them", answer("Notes summarized"), 1).await;
    respond_with_messages(&server, r#""type":"tool_result""#, answer("Notes read"), 2).await;
    respond_with_messages(
        &server,
        "Read the notes",
        messages_stream(
            &[
                thinking.clone(),
                json!({
                    "type": "tool_use",
                    "id": "toolu_notes",
                    "name": "read",
                    "input": {"path": "notes.txt"}
                }),
            ],
            "tool_use",
        ),
        3,
    )
    .await;

    server
}

/// Returns an Anthropic key and an endpoint at `server`.
fn anthropic_environment(server: &MockServer) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("ANTHROPIC_API_KEY".to_string(), "sk-ant-key".to_string()),
        (
            "ANTHROPIC_BASE_URL".to_string(),
            format!("{}/v1", server.uri()),
        ),
        ("HOME".to_string(), "/tmp".to_string()),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
    ])
}

/// Returns `value` without the `cache_control` breakpoints, which move to
/// the end of each request.
fn without_cache_control(value: &Value) -> Value {
    match value {
        Value::Array(items) => items.iter().map(without_cache_control).collect(),
        Value::Object(fields) => fields
            .iter()
            .filter(|(name, _)| *name != "cache_control")
            .map(|(name, field)| (name.clone(), without_cache_control(field)))
            .collect(),
        other => other.clone(),
    }
}

#[tokio::test]
async fn claude_turn_streams_messages_and_replays_signed_thinking_with_tool_results() {
    // Arrange
    let thinking = json!({
        "type": "thinking",
        "thinking": "The notes file holds the answer.",
        "signature": "sig-turn-thinking"
    });
    let server = claude_notes_server(&thinking).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    std::fs::write(worktree.path().join("notes.txt"), "hosted notes\n").expect("notes");
    let channel = create_agent_channel(
        AgentKind::Harness,
        None,
        Some(&config(data_root.path(), anthropic_environment(&server))),
    );
    // A provider-call budget wraps the client, which must keep replaying
    // each turn's instructions as Anthropic requires.
    let budgeted = |prompt: &str, continuation: TurnContinuation| TurnRequest {
        continuation,
        provider_call_budget: Some(ProviderCallBudget::new(8)),
        ..turn_request(worktree.path(), CLAUDE_MODEL, prompt)
    };

    // Act
    let (first, _) = run_turn(
        &channel,
        "session-claude",
        budgeted("Read the notes", TurnContinuation::fresh()),
    )
    .await;
    let first = first.expect("Claude turn should succeed");
    let (second, _) = run_turn(
        &channel,
        "session-claude",
        budgeted(
            "Summarize them",
            continuing(first.provider_conversation_id.clone()),
        ),
    )
    .await;

    // Assert
    assert_eq!(first.assistant_message.answer, "Notes read");
    let second = second.expect("the follow-up Claude turn should succeed");
    assert_eq!(second.assistant_message.answer, "Notes summarized");
    let requests = server.received_requests().await.unwrap_or_default();
    assert_eq!(requests.len(), 3);
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("x-api-key")
            .is_some_and(|value| value == "sk-ant-key")
            && request.headers.get("authorization").is_none()
    }));
    let bodies = requests
        .iter()
        .map(|request| serde_json::from_slice::<Value>(&request.body).expect("JSON body"))
        .collect::<Vec<_>>();
    for body in &bodies {
        assert_eq!(body["model"], CLAUDE_MODEL);
        assert_eq!(body["stream"], true);
        assert_eq!(body["max_tokens"], 128_000);
        assert_eq!(body["output_config"]["effort"], "low");
        assert_eq!(body["output_config"]["format"]["type"], "json_schema");
        assert_eq!(
            body["thinking"]["block_binding"]["prefix_mismatch_behavior"],
            "drop_block"
        );
    }
    assert!(requests.iter().all(|request| {
        request
            .headers
            .get("anthropic-beta")
            .is_some_and(|value| value == "thinking-binding-controls-2026-08-01")
    }));
    let replayed = bodies[1]["messages"].as_array().expect("messages");
    let assistant = replayed
        .iter()
        .position(|message| message["role"] == "assistant")
        .expect("the tool call should be replayed");
    assert_eq!(replayed[assistant]["content"][0], thinking);
    assert_eq!(replayed[assistant]["content"][1]["type"], "tool_use");
    assert_eq!(replayed[assistant]["content"][1]["id"], "toolu_notes");
    let tool_result = &replayed[assistant + 1]["content"][0];
    assert_eq!(tool_result["type"], "tool_result");
    assert_eq!(tool_result["tool_use_id"], "toolu_notes");
    assert!(tool_result.to_string().contains("hosted notes"));
    // The follow-up turn only grows the first turn's request, so its signed
    // thinking stays valid.
    let earlier = without_cache_control(&bodies[1]["messages"]);
    let later = without_cache_control(&bodies[2]["messages"]);
    let earlier = earlier.as_array().expect("messages");
    assert_eq!(
        later.as_array().expect("messages")[..earlier.len()],
        earlier[..]
    );
}

#[tokio::test]
async fn configuration_failures_stop_before_any_provider_call() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let mut without_key = environment(&server);
    without_key.remove("MODEL_API_KEY");
    let missing_key = create_agent_channel(
        AgentKind::Harness,
        None,
        Some(&config(data_root.path(), without_key)),
    );
    let channel = create_agent_channel(
        AgentKind::Harness,
        None,
        Some(&config(data_root.path(), environment(&server))),
    );
    let mut mcp_request = turn_request(worktree.path(), MUSE_MODEL, "Hi");
    mcp_request.execution_policy = ExecutionPolicy {
        mcp: McpPolicy::Disabled,
        ..ExecutionPolicy::default()
    };

    // Act
    let (key_error, _) = run_turn(
        &missing_key,
        "session-key",
        turn_request(worktree.path(), MUSE_MODEL, "Hi"),
    )
    .await;
    let (mcp_error, _) = run_turn(&channel, "session-mcp", mcp_request).await;
    let (model_error, _) = run_turn(
        &channel,
        "session-model",
        turn_request(worktree.path(), "gpt-5.3-codex-spark", "Hi"),
    )
    .await;
    let (openai_key_error, _) = run_turn(
        &channel,
        "session-openai-key",
        turn_request(worktree.path(), "gpt-6.1-sol", "Hi"),
    )
    .await;
    let (id_error, _) = run_turn(
        &channel,
        "../escape",
        turn_request(worktree.path(), MUSE_MODEL, "Hi"),
    )
    .await;

    // Assert
    let key_error = key_error.expect_err("missing key should fail");
    assert!(key_error.contains("MODEL_API_KEY"), "{key_error}");
    assert!(mcp_error.is_err());
    assert!(
        model_error
            .expect_err("unknown model should fail")
            .contains("does not serve model")
    );
    let openai_key_error = openai_key_error.expect_err("missing OpenAI key should fail");
    assert!(
        openai_key_error.contains("Set OPENAI_API_KEY"),
        "{openai_key_error}"
    );
    assert!(
        id_error
            .expect_err("path-like id should fail")
            .contains("Invalid harness session id")
    );
    assert_eq!(request_bodies(&server).await, Vec::<String>::new());
}

/// Session turn left running a 30-second `bash` command.
struct RunningBashTurn {
    events: mpsc::UnboundedReceiver<TurnEvent>,
    task: JoinHandle<Result<ag_contracts::TurnResult, AgentError>>,
}

/// Scripts `server` to answer `prompt` with a slow `bash` call, starts that
/// session turn on `channel`, and waits until the command is running.
async fn start_running_bash_turn(
    server: &MockServer,
    channel: &Arc<dyn AgentChannel>,
    worktree: &Path,
    session_id: &str,
    prompt: &str,
) -> RunningBashTurn {
    respond(
        server,
        prompt,
        tool_call("call-sleep", "bash", &json!({"command": "sleep 30"})),
        1,
    )
    .await;
    let (events_tx, mut events) = mpsc::unbounded_channel();
    let task = tokio::spawn(channel.run_turn(
        session_id.to_string(),
        turn_request(worktree, MUSE_MODEL, prompt),
        events_tx,
    ));
    let bash_running = async {
        while let Some(event) = events.recv().await {
            if matches!(
                &event,
                TurnEvent::Activity(activity)
                    if activity.name == "bash" && activity.status == ActivityStatus::Running
            ) {
                return;
            }
        }
    };
    let started = tokio::time::timeout(Duration::from_secs(10), bash_running).await;
    assert!(started.is_ok(), "the bash command should start running");

    RunningBashTurn { events, task }
}

#[tokio::test]
async fn shutdown_cancels_a_running_command_and_waits_for_cleanup() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let RunningBashTurn {
        events: mut events_rx,
        task: running,
    } = start_running_bash_turn(
        &server,
        &channel,
        worktree.path(),
        "session-cancel",
        "Run slowly",
    )
    .await;

    // Act
    running.abort();
    let shutdown = tokio::time::timeout(
        Duration::from_secs(10),
        channel.shutdown_session("session-cancel".to_string()),
    )
    .await;
    let repeated = channel.shutdown_session("session-cancel".to_string()).await;

    // Assert
    assert!(matches!(shutdown, Ok(Ok(()))), "{shutdown:?}");
    assert!(repeated.is_ok());
    let mut interrupted = false;
    while let Ok(event) = events_rx.try_recv() {
        interrupted |= matches!(
            event,
            TurnEvent::Activity(activity)
                if activity.name == "bash" && activity.status == ActivityStatus::Interrupted
        );
    }
    assert!(interrupted);
}

#[tokio::test]
async fn shutdown_during_a_running_turn_reports_it_interrupted() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let running = start_running_bash_turn(
        &server,
        &channel,
        worktree.path(),
        "session-stop",
        "Run until stopped",
    )
    .await;

    // Act
    let shutdown = tokio::time::timeout(
        Duration::from_secs(10),
        channel.shutdown_session("session-stop".to_string()),
    )
    .await;
    let result = tokio::time::timeout(Duration::from_secs(10), running.task)
        .await
        .expect("cancelled turn should finish")
        .expect("turn task should not panic");

    // Assert
    assert!(matches!(shutdown, Ok(Ok(()))), "{shutdown:?}");
    assert!(
        matches!(&result, Err(AgentError::InterruptedByUser(message)) if message.starts_with("[Stopped]")),
        "{result:?}"
    );
}

#[tokio::test]
async fn overloaded_provider_fails_the_turn_noting_the_requested_wait() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        "Busy provider",
        ResponseTemplate::new(503).insert_header("Retry-After", "7"),
        1,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (result, _) = run_turn(
        &channel,
        "session-overloaded",
        turn_request(worktree.path(), MUSE_MODEL, "Busy provider"),
    )
    .await;

    // Assert
    let error = result.expect_err("an overloaded provider should fail the turn");
    assert!(error.starts_with("Harness turn failed: "), "{error}");
    assert!(
        error.ends_with(
            "\n\nThis failure may be temporary (overloaded); retrying later may succeed. The \
             provider asked to wait at least 7 seconds."
        ),
        "{error}"
    );
}

#[tokio::test]
async fn failed_shutdown_cleanup_is_retried_before_the_next_turn() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Next question", answer("Recovered"), 1).await;
    respond(
        &server,
        "Run slowly",
        tool_call("call-sleep", "bash", &json!({"command": "sleep 30"})),
        2,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let running = tokio::spawn(channel.run_turn(
        "session-stop".to_string(),
        turn_request(worktree.path(), MUSE_MODEL, "Run slowly"),
        events_tx,
    ));
    while let Some(event) = events_rx.recv().await {
        if matches!(
            &event,
            TurnEvent::Activity(activity)
                if activity.name == "bash" && activity.status == ActivityStatus::Running
        ) {
            break;
        }
    }
    let pool = sqlx::SqlitePool::connect_with(session_database_options(
        &data_root.path().join("session-stop/harness.db"),
    ))
    .await
    .expect("session database");
    sqlx::query(
        "CREATE TRIGGER fail_outcome BEFORE UPDATE OF outcome ON session_command BEGIN SELECT \
         RAISE(FAIL, 'injected outcome failure'); END",
    )
    .execute(&pool)
    .await
    .expect("outcome fault");
    // ToolRequested precedes command admission. Wait for the durable intent
    // so cancellation cannot skip the outcome write this test must fail.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let admitted = sqlx::query_scalar::<_, bool>(
                "SELECT EXISTS(SELECT 1 FROM session_command WHERE session_id = 'session-stop' \
                 AND outcome IS NULL)",
            )
            .fetch_one(&pool)
            .await
            .expect("command admission");
            if admitted {
                break;
            }
        }
    })
    .await
    .expect("command must be journaled before cancellation");

    // Act
    running.abort();
    let cancelled = running.await.expect_err("running turn should be cancelled");
    let shutdown = channel.shutdown_session("session-stop".to_string()).await;
    let pending = sqlx::query_scalar::<_, bool>(
        "SELECT outcome IS NULL AND unresolved = 1 FROM session_command WHERE session_id = \
         'session-stop'",
    )
    .fetch_one(&pool)
    .await
    .expect("unresolved command");
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(&pool)
        .await
        .expect("remove outcome fault");
    let mut request = turn_request(worktree.path(), MUSE_MODEL, "Next question");
    request.request_kind = AgentRequestKind::SessionResume;
    let (recovered, _) = run_turn(&channel, "session-stop", request).await;
    let recorded = sqlx::query_scalar::<_, bool>(
        "SELECT outcome IS NOT NULL AND unresolved = 0 FROM session_command WHERE session_id = \
         'session-stop'",
    )
    .fetch_one(&pool)
    .await
    .expect("recorded command outcome");
    pool.close().await;

    // Assert
    assert!(cancelled.is_cancelled());
    let shutdown = shutdown.expect_err("unrecorded command outcome should fail the shutdown");
    assert!(
        shutdown.to_string().contains("Harness turn cleanup failed"),
        "{shutdown}"
    );
    assert!(
        pending,
        "failed shutdown must retain the unresolved command"
    );
    assert!(recorded, "the next turn must persist the retried outcome");
    assert_eq!(
        recovered
            .expect("next turn should be admitted after cleanup is retried")
            .assistant_message
            .answer,
        "Recovered"
    );
}

#[tokio::test]
async fn failed_command_cleanup_is_retried_by_shutdown_and_the_next_turn() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Next question", answer("Recovered"), 1).await;
    respond(
        &server,
        r#""tool_call_id":"call-printf""#,
        answer("Printed"),
        2,
    )
    .await;
    respond(
        &server,
        "Run printf",
        tool_call("call-printf", "bash", &json!({"command": "printf hi"})),
        3,
    )
    .await;
    respond(&server, "First question", answer("First answer"), 4).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let resume_request = |prompt: &str| {
        let mut request = turn_request(worktree.path(), MUSE_MODEL, prompt);
        request.request_kind = AgentRequestKind::SessionResume;

        request
    };
    let (first, _) = run_turn(
        &channel,
        "session-cleanup",
        turn_request(worktree.path(), MUSE_MODEL, "First question"),
    )
    .await;
    first.expect("first turn should succeed");
    let pool = sqlx::SqlitePool::connect_with(session_database_options(
        &data_root.path().join("session-cleanup/harness.db"),
    ))
    .await
    .expect("session database");
    sqlx::query(
        "CREATE TRIGGER fail_outcome BEFORE UPDATE OF outcome ON session_command BEGIN SELECT \
         RAISE(FAIL, 'injected outcome failure'); END",
    )
    .execute(&pool)
    .await
    .expect("outcome fault");
    let (failed, _) = run_turn(&channel, "session-cleanup", resume_request("Run printf")).await;

    // Act
    let still_failing = channel
        .shutdown_session("session-cleanup".to_string())
        .await;
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(&pool)
        .await
        .expect("remove outcome fault");
    pool.close().await;
    let (recovered, _) =
        run_turn(&channel, "session-cleanup", resume_request("Next question")).await;

    // Assert
    let failed = failed.expect_err("unrecorded command outcome should fail the turn");
    assert!(failed.contains("Harness turn cleanup failed"), "{failed}");
    let still_failing = still_failing.expect_err("a retry under the fault should fail again");
    assert!(
        still_failing
            .to_string()
            .contains("Harness turn cleanup failed"),
        "{still_failing}"
    );
    assert_eq!(
        recovered
            .expect("next turn should be admitted after cleanup is retried")
            .assistant_message
            .answer,
        "Recovered"
    );
}

#[tokio::test]
async fn unreadable_history_fails_the_turn() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    std::fs::create_dir_all(data_root.path().join("session-corrupt/harness.db"))
        .expect("directory in place of the database");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (result, _) = run_turn(
        &channel,
        "session-corrupt",
        turn_request(worktree.path(), MUSE_MODEL, "Hi"),
    )
    .await;

    // Assert
    assert!(
        result
            .expect_err("unreadable history should fail")
            .contains("Failed to resume the harness session")
    );
    assert_eq!(request_bodies(&server).await, Vec::<String>::new());
}

/// Runs `prompts` as consecutive turns of `session_id`, each resuming with the
/// previous turn's provider conversation id, and returns the last one's or
/// the first turn failure.
async fn run_session_turns(
    channel: &Arc<dyn AgentChannel>,
    worktree: &Path,
    session_id: &str,
    prompts: &[&str],
) -> Result<Option<String>, String> {
    let mut provider_conversation_id = None;
    for prompt in prompts {
        let mut request = turn_request(worktree, MUSE_MODEL, prompt);
        if provider_conversation_id.is_some() {
            request.request_kind = AgentRequestKind::SessionResume;
            request.continuation = continuing(provider_conversation_id);
        }
        let (result, _) = run_turn(channel, session_id, request).await;
        provider_conversation_id = result?.provider_conversation_id;
    }

    Ok(provider_conversation_id)
}

/// Runs `statement` against the harness database of `session_id`.
async fn execute_in_session_database(
    data_root: &Path,
    session_id: &str,
    statement: &'static str,
) -> Result<(), sqlx::Error> {
    let pool = sqlx::SqlitePool::connect_with(session_database_options(
        &data_root.join(session_id).join("harness.db"),
    ))
    .await?;
    sqlx::query(statement).execute(&pool).await?;
    pool.close().await;

    Ok(())
}

/// Returns connection options for a harness database that wait for the
/// harness's locks as long as the harness waits for theirs.
fn session_database_options(database_path: &Path) -> sqlx::sqlite::SqliteConnectOptions {
    sqlx::sqlite::SqliteConnectOptions::new()
        .filename(database_path)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
}

/// Returns a session resume of `prompt` continuing `provider_conversation_id`
/// with an Agentty replay transcript.
fn resume_with_replay(
    worktree: &Path,
    prompt: &str,
    provider_conversation_id: Option<String>,
) -> TurnRequest {
    let mut request = turn_request(worktree, MUSE_MODEL, prompt);
    request.request_kind = AgentRequestKind::SessionResume;
    request.continuation = TurnContinuation::provider(
        None,
        None,
        provider_conversation_id,
        Some("Agentty replay transcript".to_string()),
    );

    request
}

#[tokio::test]
async fn history_outside_the_replay_window_resends_the_contract_without_the_replay_transcript() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Next question", answer("Next answer"), 1).await;
    respond(&server, "First question", answer("First answer"), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let provider_conversation_id = run_session_turns(
        &channel,
        worktree.path(),
        "session-window",
        &["First question"],
    )
    .await
    .expect("first turn should succeed");
    // The finished turn stays stored but leaves the replay window.
    execute_in_session_database(
        data_root.path(),
        "session-window",
        "UPDATE session SET history_start = 1",
    )
    .await
    .expect("move the replay window past the turn");

    // Act
    let (result, _) = run_turn(
        &channel,
        "session-window",
        resume_with_replay(worktree.path(), "Next question", provider_conversation_id),
    )
    .await;

    // Assert
    result.expect("resumed turn should succeed");
    let bodies = request_bodies(&server).await;
    let resumed = bodies.last().expect("resumed request");
    assert!(!resumed.contains("First answer"), "{resumed}");
    assert!(!resumed.contains("Agentty replay transcript"), "{resumed}");
    assert_eq!(resumed.matches(FULL_CONTRACT).count(), 1);
}

#[tokio::test]
async fn unreadable_transcript_resends_the_contract_without_the_replay_transcript() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Third question", answer("Third answer"), 1).await;
    respond(&server, "Second question", answer("Second answer"), 2).await;
    respond(&server, "First question", answer("First answer"), 3).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let provider_conversation_id = run_session_turns(
        &channel,
        worktree.path(),
        "session-transcript",
        &["First question", "Second question"],
    )
    .await
    .expect("earlier turns should succeed");
    // Resuming decodes only the newest turn's options; the transcript of the
    // replayed turns decodes every one.
    execute_in_session_database(
        data_root.path(),
        "session-transcript",
        "UPDATE session_turn SET turn_options = '{}' WHERE turn_position = 0",
    )
    .await
    .expect("corrupt the oldest turn's options");

    // Act
    let (result, _) = run_turn(
        &channel,
        "session-transcript",
        resume_with_replay(worktree.path(), "Third question", provider_conversation_id),
    )
    .await;

    // Assert
    result.expect("an unreadable transcript should not fail the turn");
    let bodies = request_bodies(&server).await;
    let resumed = bodies.last().expect("resumed request");
    assert!(
        resumed.contains("Second answer"),
        "the history still replays"
    );
    assert!(!resumed.contains("Agentty replay transcript"), "{resumed}");
    // The replayed first turn carries one contract; the resumed input
    // re-sends it instead of a refresh reminder.
    assert_eq!(resumed.matches(FULL_CONTRACT).count(), 2);
}

#[tokio::test]
async fn forced_shutdown_stops_utility_runs() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let client = RealOneShotClient::pooled(None)
        .with_native_harness(config(data_root.path(), environment(&server)));
    client.force_shutdown();

    // Act
    let result = client.submit(utility_request(worktree.path(), None)).await;

    // Assert
    assert_eq!(
        result.expect_err("forced shutdown should stop").to_string(),
        "[Stopped] Agent runtime forced to shut down"
    );
}

#[tokio::test]
async fn utility_run_charges_the_budget_and_parses_the_answer() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        "Name this session",
        utility_answer("Harness title"),
        1,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let client = RealOneShotClient::pooled(None)
        .with_native_harness(config(data_root.path(), environment(&server)));
    let budget = ProviderCallBudget::new(1);
    let (activity_tx, _activity_rx) = mpsc::unbounded_channel();
    let mut request = utility_request(worktree.path(), Some(budget.clone()));
    request.activity_tx = Some(activity_tx);

    // Act
    let submission = client.submit(request).await;
    let exhausted = client
        .submit(utility_request(worktree.path(), Some(budget.clone())))
        .await;

    // Assert
    let submission = submission.expect("utility run should succeed");
    assert_eq!(submission.response.answer, "Harness title");
    assert_eq!(
        (
            submission.stats.input_tokens,
            submission.stats.output_tokens
        ),
        (30, 7)
    );
    assert!(exhausted.is_err());
    assert!(budget.ensure_available().is_err());
    assert_eq!(request_bodies(&server).await.len(), 1);
}

#[tokio::test]
async fn utility_run_forwards_tool_activity() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        r#""tool_call_id":"call-read""#,
        utility_answer("Harness title"),
        1,
    )
    .await;
    respond(
        &server,
        "Name this session",
        tool_call("call-read", "read", &json!({"path": "missing.txt"})),
        2,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let client = RealOneShotClient::pooled(None)
        .with_native_harness(config(data_root.path(), environment(&server)));
    let (activity_tx, mut activity_rx) = mpsc::unbounded_channel();
    let mut request = utility_request(worktree.path(), None);
    request.activity_tx = Some(activity_tx);

    // Act
    let submission = client.submit(request).await;

    // Assert
    assert_eq!(
        submission
            .expect("utility run should succeed")
            .response
            .answer,
        "Harness title"
    );
    let activity = activity_rx.try_recv().expect("read activity");
    assert_eq!(
        (activity.name.as_str(), activity.status),
        ("read", ActivityStatus::Running)
    );
}

#[tokio::test]
async fn utility_configuration_failures_stop_before_any_provider_call() {
    // Arrange
    let server = MockServer::start().await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let mut without_key = environment(&server);
    without_key.remove("MODEL_API_KEY");
    let missing_key =
        RealOneShotClient::pooled(None).with_native_harness(config(data_root.path(), without_key));
    let client = RealOneShotClient::pooled(None)
        .with_native_harness(config(data_root.path(), environment(&server)));
    let mut mcp_request = utility_request(worktree.path(), None);
    mcp_request.execution_policy = ExecutionPolicy {
        mcp: McpPolicy::Disabled,
        ..ExecutionPolicy::default()
    };

    // Act
    let key_error = missing_key
        .submit(utility_request(worktree.path(), None))
        .await;
    let mcp_error = client.submit(mcp_request).await;

    // Assert
    let key_error = key_error.expect_err("missing key should fail").to_string();
    assert!(key_error.contains("MODEL_API_KEY"), "{key_error}");
    assert!(mcp_error.is_err());
    assert_eq!(request_bodies(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn utility_run_stops_when_cancelled() {
    // Arrange
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(utility_answer("Late").set_delay(Duration::from_secs(30)))
        .mount(&server)
        .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let client = RealOneShotClient::pooled(None)
        .with_native_harness(config(data_root.path(), environment(&server)));
    let cancellation = CancellationToken::new();
    cancellation.cancel();

    // Act
    let result = client
        .submit_cancellable(utility_request(worktree.path(), None), cancellation)
        .await;

    // Assert
    assert_eq!(
        result.expect_err("cancelled run should stop").to_string(),
        "[Stopped] Agent run canceled"
    );
}

#[tokio::test]
async fn unavailable_provider_fails_the_utility_run_as_possibly_temporary() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Name this session", ResponseTemplate::new(503), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let client = RealOneShotClient::pooled(None)
        .with_native_harness(config(data_root.path(), environment(&server)));

    // Act
    let result = client.submit(utility_request(worktree.path(), None)).await;

    // Assert
    let error = result
        .expect_err("an unavailable provider should fail the run")
        .to_string();
    assert!(error.starts_with("Harness turn failed: "), "{error}");
    assert!(
        error.ends_with(
            "\n\nThis failure may be temporary (overloaded); retrying later may succeed."
        ),
        "{error}"
    );
}

#[tokio::test]
async fn unconfigured_harness_fails_before_execution() {
    // Arrange
    let worktree = tempfile::tempdir().expect("worktree");
    let channel = create_agent_channel(AgentKind::Harness, None, None);
    let client = RealOneShotClient::pooled(None);

    // Act
    let (turn, _) = run_turn(
        &channel,
        "session-off",
        turn_request(worktree.path(), MUSE_MODEL, "Hi"),
    )
    .await;
    let utility = client.submit(utility_request(worktree.path(), None)).await;

    // Assert
    assert!(
        turn.expect_err("unconfigured turn should fail")
            .contains("--experimental-harness")
    );
    assert!(
        utility
            .expect_err("unconfigured utility should fail")
            .to_string()
            .contains("--experimental-harness")
    );
}

/// Locally served session model with a small declared window.
const SMALL_WINDOW_MODEL: &str = "openai-compatible/small-window";
/// Leading line of every compaction request's source.
const COMPACTION_SOURCE: &str = "Conversation source to summarize";
/// Input tokens the fast model reports for each compaction request.
const COMPACTION_INPUT_TOKENS: u64 = 800;
/// Output tokens the fast model reports for each compaction request.
const COMPACTION_OUTPUT_TOKENS: u64 = 40;

/// Returns credentials serving [`SMALL_WINDOW_MODEL`] from `local` with a
/// 16,384-token window and Muse, the fast model, from `fast`.
fn compaction_environment(local: &MockServer, fast: &MockServer) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("HOME".to_string(), "/tmp".to_string()),
        ("MODEL_API_BASE_URL".to_string(), fast.uri()),
        ("MODEL_API_KEY".to_string(), "muse-key".to_string()),
        ("OPENAI_COMPATIBLE_BASE_URL".to_string(), local.uri()),
        (
            "OPENAI_COMPATIBLE_CONTEXT_WINDOW".to_string(),
            "16384".to_string(),
        ),
        (
            "PATH".to_string(),
            std::env::var("PATH").unwrap_or_default(),
        ),
    ])
}

/// Most session turns [`run_until_notice`] runs before the session must
/// have compacted.
const MAX_UNCOMPACTED_TURNS: usize = 8;

/// Returns a session answer large enough that a few turns fill the small
/// window's history.
fn long_answer() -> ResponseTemplate {
    answer(&"long answer ".repeat(400))
}

/// Returns a valid compaction summary with the fast model's usage.
fn summary_answer() -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_json(json!({
        "choices": [{
            "finish_reason": "stop",
            "message": {"content": json!({
                "context": "Earlier questions were answered at length.",
                "decisions": [],
                "state": "Answering the next question."
            }).to_string()}
        }],
        "usage": {
            "prompt_tokens": COMPACTION_INPUT_TOKENS,
            "completion_tokens": COMPACTION_OUTPUT_TOKENS,
            "total_tokens": COMPACTION_INPUT_TOKENS + COMPACTION_OUTPUT_TOKENS
        }
    }))
}

/// Returns automatic compaction summarizing on `summary_model`.
fn automatic(summary_model: Option<&str>) -> ContextCompaction {
    ContextCompaction::Automatic {
        summary_model: summary_model.map(str::to_string),
    }
}

/// Returns numbered session turn `number` on `model` with `compaction`,
/// resuming the session unless it is the first turn of a new one.
fn numbered_turn(
    worktree: &Path,
    model: &str,
    number: usize,
    compaction: &ContextCompaction,
    resumes: bool,
) -> TurnRequest {
    let mut request = turn_request(worktree, model, &format!("Question {number}"));
    request.context_compaction = compaction.clone();
    if resumes {
        request.request_kind = AgentRequestKind::SessionResume;
    }

    request
}

/// Runs numbered session turns of `session_id` on [`SMALL_WINDOW_MODEL`]
/// with `compaction` until one reports a notice, and returns that turn's
/// result and events; the first turn resumes the session when `resumed`.
///
/// # Errors
/// Returns a failed turn's error, or a failure when no turn reports a notice.
async fn run_until_notice(
    channel: &Arc<dyn AgentChannel>,
    worktree: &Path,
    session_id: &str,
    compaction: &ContextCompaction,
    resumed: bool,
) -> Result<(ag_contracts::TurnResult, Vec<TurnEvent>), String> {
    let mut provider_conversation_id = None;
    for number in 1..=MAX_UNCOMPACTED_TURNS {
        let mut request = numbered_turn(
            worktree,
            SMALL_WINDOW_MODEL,
            number,
            compaction,
            resumed || number > 1,
        );
        request.continuation = continuing(provider_conversation_id.take());
        let (result, events) = run_turn(channel, session_id, request).await;
        let result = result?;
        if events
            .iter()
            .any(|event| matches!(event, TurnEvent::Notice(_)))
        {
            return Ok((result, events));
        }
        provider_conversation_id = result.provider_conversation_id;
    }

    Err(format!(
        "{MAX_UNCOMPACTED_TURNS} turns should compact the session"
    ))
}

#[tokio::test]
async fn long_session_compacts_on_the_fast_model_at_its_prices() {
    // Arrange
    let local = MockServer::start().await;
    let fast = MockServer::start().await;
    respond(&local, "Question", long_answer(), 1).await;
    respond(&fast, COMPACTION_SOURCE, summary_answer(), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), compaction_environment(&local, &fast));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (result, events) = run_until_notice(
        &channel,
        worktree.path(),
        "session-compact",
        &automatic(Some(MUSE_MODEL)),
        false,
    )
    .await
    .expect("the session should compact");
    let mut next = numbered_turn(
        worktree.path(),
        SMALL_WINDOW_MODEL,
        99,
        &automatic(Some(MUSE_MODEL)),
        true,
    );
    next.continuation = continuing(result.provider_conversation_id.clone());
    let (next_result, _) = run_turn(&channel, "session-compact", next).await;

    // Assert
    assert!(
        events.contains(&TurnEvent::Notice(TurnNotice::ContextCompacted {
            model: Some(MUSE_MODEL.to_string()),
        }))
    );
    let fast_bodies = request_bodies(&fast).await;
    assert_eq!(fast_bodies.len(), 1);
    assert!(fast_bodies[0].contains(COMPACTION_SOURCE));
    // The checkpoint covers the turn that carried the full contract, so the
    // next turn sends it again beside the summary.
    next_result.expect("the turn after compaction should succeed");
    let next_body = request_bodies(&local)
        .await
        .into_iter()
        .find(|body| body.contains("Question 99"))
        .expect("the next turn reaches the session model");
    assert_eq!(next_body.matches(FULL_CONTRACT).count(), 1);
    assert!(next_body.contains("Earlier questions were answered at length."));
    assert!(fast_bodies[0].contains(r#""model":"muse-spark-1.3""#));
    assert!(
        !request_bodies(&local)
            .await
            .iter()
            .any(|body| body.contains(COMPACTION_SOURCE))
    );
    let report: Value = serde_json::from_str(result.harness_report.as_deref().expect("report"))
        .expect("report JSON");
    let compaction = &report["model_requests"][0];
    assert_eq!(compaction["purpose"], "Compaction");
    assert_eq!(compaction["model"], "muse/muse-spark-1.3");
    let pricing = ModelProvider::Muse
        .pricing(MUSE_MODEL)
        .expect("Muse prices");
    let mut usage = Usage::default();
    usage.input = COMPACTION_INPUT_TOKENS;
    usage.output = COMPACTION_OUTPUT_TOKENS;
    assert_eq!(
        compaction["cost"]["micro_usd"],
        Cost::of(&usage, pricing).micro_usd
    );
    // The turn's usage counts the compaction request with its own.
    assert_eq!(
        result.input_tokens,
        COMPACTION_INPUT_TOKENS + 30,
        "{report}"
    );
    assert_eq!(result.output_tokens, COMPACTION_OUTPUT_TOKENS + 7);
}

#[tokio::test]
async fn an_unusable_fast_model_leaves_compaction_on_the_session_model() {
    // Arrange
    let local = MockServer::start().await;
    let fast = MockServer::start().await;
    respond(&local, COMPACTION_SOURCE, summary_answer(), 1).await;
    respond(&local, "Question", long_answer(), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let mut environment = compaction_environment(&local, &fast);
    environment.remove("MODEL_API_KEY");
    let config = config(data_root.path(), environment);
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (_, events) = run_until_notice(
        &channel,
        worktree.path(),
        "session-compact",
        &automatic(Some(MUSE_MODEL)),
        false,
    )
    .await
    .expect("the session should compact");

    // Assert
    assert!(
        events.contains(&TurnEvent::Notice(TurnNotice::ContextCompacted {
            model: Some(SMALL_WINDOW_MODEL.to_string()),
        }))
    );
    assert_eq!(request_bodies(&fast).await, Vec::<String>::new());
}

#[tokio::test]
async fn turns_without_automatic_compaction_evict_instead() {
    // Arrange
    let local = MockServer::start().await;
    let fast = MockServer::start().await;
    respond(&local, "Question", long_answer(), 1).await;
    respond(&fast, COMPACTION_SOURCE, summary_answer(), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), compaction_environment(&local, &fast));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let outcome = run_until_notice(
        &channel,
        worktree.path(),
        "session-evict",
        &ContextCompaction::Off,
        false,
    )
    .await;

    // Assert
    assert!(outcome.is_err_and(|error| error.contains("should compact")));
    assert_eq!(request_bodies(&fast).await, Vec::<String>::new());
    assert!(
        !request_bodies(&local)
            .await
            .iter()
            .any(|body| body.contains(COMPACTION_SOURCE))
    );
}

#[tokio::test]
async fn switching_to_a_small_window_compacts_on_the_fast_model() {
    // Arrange
    let local = MockServer::start().await;
    let fast = MockServer::start().await;
    respond(&local, "Question", long_answer(), 1).await;
    respond(&fast, COMPACTION_SOURCE, summary_answer(), 1).await;
    respond(&fast, "Question", long_answer(), 2).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), compaction_environment(&local, &fast));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let compaction = automatic(Some(MUSE_MODEL));
    // Muse's large window holds these turns, the small window does not.
    for number in 1..=4 {
        let request = numbered_turn(worktree.path(), MUSE_MODEL, number, &compaction, number > 1);
        let (result, events) = run_turn(&channel, "session-switch", request).await;
        result.expect("Muse turn should succeed");
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, TurnEvent::Notice(_)))
        );
    }

    // Act
    let request = numbered_turn(worktree.path(), SMALL_WINDOW_MODEL, 5, &compaction, true);
    let (result, events) = run_turn(&channel, "session-switch", request).await;

    // Assert
    result.expect("the switching turn should succeed");
    // The switching turn summarizes on the Fast model, not on the new
    // session model.
    assert!(
        events.contains(&TurnEvent::Notice(TurnNotice::ContextCompacted {
            model: Some(MUSE_MODEL.to_string()),
        })),
        "{events:?}"
    );
    let fast_bodies = request_bodies(&fast).await;
    assert_eq!(
        fast_bodies
            .iter()
            .filter(|body| body.contains(COMPACTION_SOURCE))
            .count(),
        1
    );
    let local_bodies = request_bodies(&local).await;
    assert!(
        !local_bodies
            .iter()
            .any(|body| body.contains(COMPACTION_SOURCE))
    );
    assert!(local_bodies.iter().any(|body| body.contains("Question 5")));
}

#[tokio::test]
async fn session_recorded_before_automatic_compaction_resumes_and_compacts() {
    // Arrange
    let local = MockServer::start().await;
    let fast = MockServer::start().await;
    respond(&local, "Question", long_answer(), 1).await;
    respond(&fast, COMPACTION_SOURCE, summary_answer(), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let session_directory = data_root.path().join("fixture-session");
    std::fs::create_dir_all(&session_directory).expect("session directory");
    // A one-turn session on `muse-spark-1.3`, prompt `First question` and
    // answer `First answer`, recorded under registration revision
    // `agentty-native-2` by this crate at d0841a242, the last revision
    // before sessions compacted automatically. A one-off test ran that
    // turn as `session_turn_answers_and_resumes_with_stored_history` does,
    // in the worktree `/tmp/agentty-worktree`, then copied the session's
    // `harness.db` and folded its WAL in with `sqlite3 harness.db
    // "PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;
    // VACUUM;"`.
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pre_compaction_harness.db"),
        session_directory.join("harness.db"),
    )
    .expect("fixture copy");
    let config = config(data_root.path(), compaction_environment(&local, &fast));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));

    // Act
    let (_, events) = run_until_notice(
        &channel,
        worktree.path(),
        "fixture-session",
        &automatic(Some(MUSE_MODEL)),
        true,
    )
    .await
    .expect("the recorded session should resume and compact");

    // Assert
    let local_bodies = request_bodies(&local).await;
    // The first resumed turn replays the recorded one.
    assert!(local_bodies[0].contains("First question"));
    assert!(local_bodies[0].contains("First answer"));
    assert!(
        events.contains(&TurnEvent::Notice(TurnNotice::ContextCompacted {
            model: Some(MUSE_MODEL.to_string()),
        }))
    );
    let fast_bodies = request_bodies(&fast).await;
    assert_eq!(fast_bodies.len(), 1);
    assert!(fast_bodies[0].contains(COMPACTION_SOURCE));
}
