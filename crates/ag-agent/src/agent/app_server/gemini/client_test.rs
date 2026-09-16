use std::path::PathBuf;
use std::sync::Arc;

use ag_protocol::{ProtocolRequestProfile, ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use crate::agent::app_server::client::{
    ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime,
};
use crate::agent::app_server::gemini::client::{GeminiRuntimeProvider, GeminiSessionRuntime};
use crate::agent::app_server::gemini::lifecycle::GeminiRuntimeState;
use crate::agent::app_server::stdio_transport::AppServerStdioTransport;
use crate::agent::submission::{OneShotClient, OneShotRequest, RealOneShotClient};
use crate::app_server::{
    AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    BorrowedAppServerFuture,
};
use crate::channel::AgentRequestKind;
use crate::model::agent::{AgentModel, ReasoningLevel};
use crate::model::permission::PermissionMode;
use crate::model::session::SpeedMode;
use crate::{ProviderCallBudget, app_server_transport};

/// Builds one Gemini session runtime whose stdin is already closed so turn
/// writes fail deterministically without a live ACP process.
fn build_stopped_session_runtime() -> GeminiSessionRuntime {
    let (child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(std::process::Command::new("cat"), "cat")
            .expect("`cat` should spawn as a runtime stand-in");
    let mut transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "Gemini ACP stdin is unavailable",
        "Failed reading Gemini ACP stdout",
    );
    transport.close_stdin();
    let mut state = GeminiRuntimeState::new(
        PathBuf::from("/tmp/agentty-gemini-runtime"),
        AgentModel::Gemini31Pro.as_str().to_string(),
        crate::model::permission::PermissionMode::AutoEdit,
    );
    state.session_id = "session-1".to_string();

    GeminiSessionRuntime {
        child,
        state,
        transport,
    }
}

#[tokio::test]
async fn runtime_reuse_requires_matching_permission_mode() {
    // Arrange
    let mut runtime = build_stopped_session_runtime();
    let mut request = runtime_request(&runtime);

    // Act
    let auto_edit_matches = runtime.matches_request(&request);
    request.permission_mode = crate::model::permission::PermissionMode::ReadOnly;
    let read_only_matches = runtime.matches_request(&request);
    runtime.shutdown_runtime().await;

    // Assert
    assert!(auto_edit_matches);
    assert!(!read_only_matches);
}

#[tokio::test]
async fn run_turn_ignores_speed_mode_and_surfaces_transport_failures() {
    // Arrange
    let mut runtime = build_stopped_session_runtime();
    let prompt = TurnPrompt::from("Implement the task");
    let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

    // Act
    let result = GeminiRuntimeProvider::run_turn(
        &mut runtime,
        &prompt,
        ProtocolRequestProfile::SessionTurn,
        ReasoningLevel::default(),
        SpeedMode::Fast,
        stream_tx,
    )
    .await;

    // Assert
    let error = result.expect_err("a closed runtime stdin should fail the turn");
    assert!(matches!(error, AppServerError::Transport(_)));
}

fn runtime_request(runtime: &GeminiSessionRuntime) -> AppServerTurnRequest {
    AppServerTurnRequest {
        provider_call_budget: None,
        folder: runtime.state.folder.clone(),
        live_transcript: None,
        main_checkout_root: None,
        model: runtime.state.model.clone(),
        permission_mode: crate::model::permission::PermissionMode::AutoEdit,
        persisted_instruction_conversation_id: None,
        personality: crate::channel::PersonalityPrompt::default(),
        prompt: TurnPrompt::from("Continue"),
        provider_conversation_id: None,
        reasoning_level: ReasoningLevel::default(),
        replay_transcript: None,
        request_kind: crate::channel::AgentRequestKind::SessionResume,
        session_id: "session-1".to_string(),
        speed_mode: SpeedMode::default(),
    }
}

#[tokio::test]
async fn isolated_context_reset_keeps_process_and_discards_previous_conversation() {
    // Arrange
    let mut runtime = build_stopped_session_runtime();
    let request = runtime_request(&runtime);
    let failed = GeminiRuntimeProvider::reset_context(&mut runtime, &request).await;
    assert!(failed.is_err());
    runtime.shutdown_runtime().await;
    let script = r#"while IFS= read -r request; do
        request_id=$(printf '%s' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
        printf '{"id":"%s","result":{"sessionId":"fresh-context"}}\n' "$request_id"
    done"#;
    let mut command = std::process::Command::new("sh");
    command.arg("-c").arg(script);
    let (child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(command, "fixture").expect("runtime");
    runtime.child = child;
    runtime.transport = AppServerStdioTransport::new(stdin, stdout, "stdin", "stdout");
    runtime.state.restored_context = true;

    let pid = runtime.pid();
    // Act
    let reset = GeminiRuntimeProvider::reset_context(&mut runtime, &request)
        .await
        .expect("fresh context");
    // Assert
    assert!(reset);
    assert_eq!(runtime.pid(), pid);
    assert_eq!(runtime.state.session_id, "fresh-context");
    assert!(!runtime.state.restored_context);
    runtime.shutdown_runtime().await;
}

/// Uses the real Gemini lifecycle with a deterministic ACP process in place
/// of CLI discovery and authentication.
struct FixtureGeminiProvider;

impl RuntimeClientProvider for FixtureGeminiProvider {
    type Runtime = GeminiSessionRuntime;

    fn label() -> &'static str {
        GeminiRuntimeProvider::label()
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        GeminiRuntimeProvider::schema_instruction_mode()
    }

    fn retain_runtime_after_turn() -> bool {
        GeminiRuntimeProvider::retain_runtime_after_turn()
    }

    fn reset_context<'scope>(
        runtime: &'scope mut Self::Runtime,
        request: &'scope AppServerTurnRequest,
    ) -> BorrowedAppServerFuture<'scope, Result<bool, AppServerError>> {
        GeminiRuntimeProvider::reset_context(runtime, request)
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        Box::pin(async move {
            let script = r#"
                printf 'start\n' >> "$1/starts"
                turns=0
                while IFS= read -r request; do
                    printf '%s\n' "$request" >> "$1/events"
                    request_id=$(printf '%s' "$request" | sed -n 's/.*"id":"\([^"]*\)".*/\1/p')
                    case "$request" in
                        *'"method":"session/new"'*)
                            printf '{"id":"%s","result":{"sessionId":"fresh-context"}}\n' "$request_id" ;;
                        *)
                            turns=$((turns + 1))
                            if [ "$turns" -eq 1 ]; then
                                response='malformed'
                            else
                                response='{\"project_impact\":[],\"suggestions\":[]}'
                            fi
                            printf '{"id":"%s","result":{"response":"%s","usage":{"inputTokens":2,"outputTokens":3}}}\n' "$request_id" "$response" ;;
                    esac
                done
            "#;
            let mut command = std::process::Command::new("sh");
            command.args(["-c", script, "fixture"]).arg(&request.folder);
            let (child, stdin, stdout) =
                app_server_transport::spawn_runtime_command(command, "fixture")?;
            let mut state =
                GeminiRuntimeState::new(request.folder, request.model, request.permission_mode);
            state.session_id = "original-context".into();
            Ok(GeminiSessionRuntime {
                child,
                state,
                transport: AppServerStdioTransport::new(stdin, stdout, "stdin", "stdout"),
            })
        })
    }

    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        profile: ProtocolRequestProfile,
        reasoning: ReasoningLevel,
        speed: SpeedMode,
        stream: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        GeminiRuntimeProvider::run_turn(runtime, prompt, profile, reasoning, speed, stream)
    }
}

#[tokio::test]
async fn pooled_gemini_repair_retains_process_and_context_until_the_next_submission() {
    // Arrange
    let folder = tempfile::tempdir().expect("fixture folder");
    let budget = ProviderCallBudget::new(3);
    let client = RealOneShotClient::pooled(Some(Arc::new(ProviderRuntimeClient::<
        FixtureGeminiProvider,
    >::new())));
    let request = OneShotRequest {
        child_pid: None,
        folder: folder.path().into(),
        harness: "gemini".into(),
        model: AgentModel::Gemini31Pro.as_str().into(),
        permission_mode: PermissionMode::ReadOnly,
        prompt: "review".into(),
        provider_call_budget: Some(budget.clone()),
        reasoning_level: ReasoningLevel::High,
        request_kind: AgentRequestKind::FocusedReview,
        speed_mode: SpeedMode::Normal,
    };

    // Act
    let first = client
        .submit(request.clone())
        .await
        .expect("repair succeeds");
    client
        .submit(request)
        .await
        .expect("next submission reuses process");
    client.close().await;

    // Assert
    assert_eq!(first.stats.input_tokens, 4);
    assert_eq!(first.stats.output_tokens, 6);
    assert!(
        budget.ensure_available().is_err(),
        "all three attempts were charged"
    );
    assert_eq!(
        std::fs::read_to_string(folder.path().join("starts")).expect("starts"),
        "start\n"
    );
    let events: Vec<serde_json::Value> = std::fs::read_to_string(folder.path().join("events"))
        .expect("events")
        .lines()
        .map(|line| serde_json::from_str(line).expect("ACP request"))
        .collect();
    assert_eq!(events.len(), 4);
    assert_eq!(events[0]["params"]["sessionId"], "original-context");
    assert_eq!(events[1]["params"]["sessionId"], "original-context");
    assert_eq!(events[2]["method"], "session/new");
    assert_eq!(events[3]["params"]["sessionId"], "fresh-context");
}
