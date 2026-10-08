//! Public contract coverage for in-process `ag-harness` session turns and
//! utility runs against a scripted Chat Completions provider.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use ag_agent::{
    NativeHarnessConfig, RealOneShotClient, create_agent_channel, instruction_bootstrap_key,
};
use ag_contracts::{
    ActivityKind, ActivityStatus, AgentChannel, AgentRequestKind, ExecutionPolicy, LiveTranscript,
    McpPolicy, OneShotClient, OneShotRequest, PermissionMode, PersonalityPrompt,
    ProviderCallBudget, ReasoningLevel, ResponseStyle, SpeedMode, TurnContinuation, TurnEvent,
    TurnRequest,
};
use ag_protocol::{TurnPrompt, TurnPromptAttachment};
use ag_session::AgentKind;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{body_string_contains, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const MUSE_MODEL: &str = "muse-spark-1.3";
const QWEN_MODEL: &str = "qwen-plus";
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
        continuation: TurnContinuation::fresh(),
        execution_policy: ExecutionPolicy::default(),
        folder: folder.to_path_buf(),
        main_checkout_root: None,
        model: model.to_string(),
        permission_mode: PermissionMode::AutoEdit,
        personality: PersonalityPrompt::default(),
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
    server
        .received_requests()
        .await
        .unwrap_or_default()
        .iter()
        .map(|request: &Request| String::from_utf8_lossy(&request.body).into_owned())
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
        let mut request = turn_request(worktree.path(), MUSE_MODEL, &prompt);
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
            MUSE_MODEL,
            &format!("First question {}", "x".repeat(220_000)),
        ),
    )
    .await;
    let first = first.expect("first turn should succeed");
    let (second, _) = run_turn(
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
    assert_eq!(
        second.provider_conversation_id.as_deref(),
        Some("session-evict")
    );
    let bodies = request_bodies(&server).await;
    assert_eq!(bodies.len(), 3);
    assert!(!bodies[1].contains("First answer"));
    assert!(bodies[1].contains(REFRESH_REMINDER));
    assert!(!bodies[2].contains("First answer"));
    assert_eq!(bodies[2].matches(FULL_CONTRACT).count(), 1);
    // Only the replayed second turn carries a reminder.
    assert_eq!(bodies[2].matches(REFRESH_REMINDER).count(), 1);
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

    // Act
    let (result, _) = run_turn(&channel, "session-image", request).await;

    // Assert
    let error = result.expect_err("image attachments should be rejected");
    assert!(error.contains("do not support image attachments"));
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
    let config = config(data_root.path(), environment(&server));
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
    let bash_result = bodies
        .iter()
        .find(|body| body.contains(r#""tool_call_id":"call-bash""#))
        .expect("bash result should reach the model");
    assert!(bash_result.contains("hello harness"));
    assert!(bash_result.contains("PATH="));
    assert!(!bash_result.contains("muse-key"));
    assert!(!bash_result.contains("MODEL_API_KEY"));
}

#[tokio::test]
async fn read_only_turn_denies_writes() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        "Edit something",
        tool_call(
            "call-write",
            "write",
            &json!({"path": "notes.txt", "patch": "--- /dev/null\n+++ b/notes.txt\n"}),
        ),
        1,
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
    let error = result.expect_err("denied write should fail the turn");
    assert!(error.contains("unsupported tool: write"), "{error}");
    assert!(!worktree.path().join("notes.txt").exists());
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, TurnEvent::Activity(_)))
    );
}

#[tokio::test]
async fn missing_history_restarts_from_the_replay_transcript() {
    // Arrange
    let server = MockServer::start().await;
    respond(&server, "Continue please", answer("Continued"), 1).await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let mut request = turn_request(worktree.path(), MUSE_MODEL, "Continue please");
    request.request_kind = AgentRequestKind::SessionResume;
    request.continuation = TurnContinuation::replaying("Earlier we renamed parse_config".into());

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
    assert!(
        id_error
            .expect_err("path-like id should fail")
            .contains("Invalid harness session id")
    );
    assert_eq!(request_bodies(&server).await, Vec::<String>::new());
}

#[tokio::test]
async fn shutdown_cancels_a_running_command_and_waits_for_cleanup() {
    // Arrange
    let server = MockServer::start().await;
    respond(
        &server,
        "Run slowly",
        tool_call("call-sleep", "bash", &json!({"command": "sleep 30"})),
        1,
    )
    .await;
    let data_root = tempfile::tempdir().expect("data root");
    let worktree = tempfile::tempdir().expect("worktree");
    let config = config(data_root.path(), environment(&server));
    let channel = create_agent_channel(AgentKind::Harness, None, Some(&config));
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let turn = channel.run_turn(
        "session-cancel".to_string(),
        turn_request(worktree.path(), MUSE_MODEL, "Run slowly"),
        events_tx,
    );
    let running = tokio::spawn(turn);
    while let Some(event) = events_rx.recv().await {
        if matches!(
            &event,
            TurnEvent::Activity(activity)
                if activity.name == "bash" && activity.status == ActivityStatus::Running
        ) {
            break;
        }
    }

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
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(data_root.path().join("session-stop/harness.db")),
    )
    .await
    .expect("session database");
    sqlx::query(
        "CREATE TRIGGER fail_outcome BEFORE UPDATE OF outcome ON session_command BEGIN SELECT \
         RAISE(FAIL, 'injected outcome failure'); END",
    )
    .execute(&pool)
    .await
    .expect("outcome fault");
    running.abort();
    let shutdown = channel.shutdown_session("session-stop".to_string()).await;
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(&pool)
        .await
        .expect("remove outcome fault");
    pool.close().await;
    let mut request = turn_request(worktree.path(), MUSE_MODEL, "Next question");
    request.request_kind = AgentRequestKind::SessionResume;

    // Act
    let (recovered, _) = run_turn(&channel, "session-stop", request).await;

    // Assert
    let shutdown = shutdown.expect_err("unrecorded command outcome should fail the shutdown");
    assert!(
        shutdown.to_string().contains("Harness turn cleanup failed"),
        "{shutdown}"
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
async fn failed_command_cleanup_is_retried_before_the_next_turn() {
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
    let pool = sqlx::SqlitePool::connect_with(
        sqlx::sqlite::SqliteConnectOptions::new()
            .filename(data_root.path().join("session-cleanup/harness.db")),
    )
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
    sqlx::query("DROP TRIGGER fail_outcome")
        .execute(&pool)
        .await
        .expect("remove outcome fault");
    pool.close().await;

    // Act
    let (recovered, _) =
        run_turn(&channel, "session-cleanup", resume_request("Next question")).await;

    // Assert
    let failed = failed.expect_err("unrecorded command outcome should fail the turn");
    assert!(failed.contains("Harness turn cleanup failed"), "{failed}");
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
            .contains("Failed to load the harness session")
    );
    assert_eq!(request_bodies(&server).await, Vec::<String>::new());
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
