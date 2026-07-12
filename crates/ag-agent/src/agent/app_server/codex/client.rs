//! Codex app-server client orchestration.

use ag_protocol::{ProtocolSchemaInstructionMode, TurnPrompt};
use tokio::sync::mpsc;

use super::super::client::{ProviderRuntimeClient, RuntimeClientProvider, RuntimeClientRuntime};
use super::super::stdio_transport::AppServerStdioTransport;
use super::lifecycle::{self, CodexRuntimeState};
use crate::app_server::{
    AppServerError, AppServerFuture, AppServerStreamEvent, AppServerTurnRequest,
    BorrowedAppServerFuture,
};
use crate::model::agent::{AgentKind, ReasoningLevel};
use crate::{agent, app_server_transport};

/// Production [`AppServerClient`] backed by `codex app-server` process
/// instances.
pub(crate) type RealCodexAppServerClient = ProviderRuntimeClient<CodexRuntimeProvider>;

/// Codex hooks used by the shared app-server runtime client.
pub(crate) struct CodexRuntimeProvider;

impl RuntimeClientProvider for CodexRuntimeProvider {
    type Runtime = CodexSessionRuntime;

    fn label() -> &'static str {
        "Codex"
    }

    fn schema_instruction_mode() -> ProtocolSchemaInstructionMode {
        agent::protocol_schema_instruction_mode(AgentKind::Codex)
    }

    fn retain_runtime_after_turn() -> bool {
        true
    }

    fn start_runtime(
        request: AppServerTurnRequest,
    ) -> AppServerFuture<Result<Self::Runtime, AppServerError>> {
        Box::pin(async move {
            let (child, transport, state) = lifecycle::start_runtime(&request).await?;

            Ok(CodexSessionRuntime {
                child,
                state,
                transport,
            })
        })
    }

    fn run_turn<'scope>(
        runtime: &'scope mut Self::Runtime,
        prompt: &'scope TurnPrompt,
        reasoning_level: ReasoningLevel,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> BorrowedAppServerFuture<'scope, Result<(String, u64, u64), AppServerError>> {
        Box::pin(async move {
            lifecycle::run_turn_with_runtime(
                &mut runtime.transport,
                &mut runtime.state,
                prompt,
                reasoning_level,
                stream_tx,
            )
            .await
        })
    }
}

/// Active Codex app-server session runtime.
pub(crate) struct CodexSessionRuntime {
    child: app_server_transport::AppServerRuntimeChild,
    state: CodexRuntimeState,
    transport: AppServerStdioTransport,
}

impl RuntimeClientRuntime for CodexSessionRuntime {
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder && self.state.model == request.model
    }

    fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    fn provider_conversation_id(&self) -> Option<String> {
        if self.state.thread_id.is_empty() {
            None
        } else {
            Some(self.state.thread_id.clone())
        }
    }

    fn restored_context(&self) -> bool {
        self.state.restored_context
    }

    fn shutdown_runtime(&mut self) -> BorrowedAppServerFuture<'_, ()> {
        Box::pin(async move {
            self.transport.close_stdin();
            app_server_transport::shutdown_child(&mut self.child).await;
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use mockall::Sequence;
    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::agent::app_server::codex::{
        MockCodexRuntimeTransport, lifecycle, policy, stream_parser, usage,
    };
    use crate::model::agent::{AgentModel, ReasoningLevel};

    /// Creates runtime state for one synthetic Codex session path.
    fn build_runtime_state(thread_id: &str, latest_input_tokens: u64) -> CodexRuntimeState {
        let folder = std::env::temp_dir().join(format!(
            "agentty-codex-runtime-state-{thread_id}-{latest_input_tokens}"
        ));
        let mut state = CodexRuntimeState::new(folder, AgentModel::Gpt55.as_str().to_string());
        state.thread_id = thread_id.to_string();
        state.latest_input_tokens = latest_input_tokens;

        state
    }

    /// Captures the dynamic request id from a written payload and returns it
    /// through the provided mutex.
    fn remember_request_id(id_store: &Arc<Mutex<Option<String>>>, payload: &Value) {
        let id = payload
            .get("id")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if let Ok(mut guard) = id_store.lock() {
            *guard = id;
        }
    }

    #[test]
    fn turn_completed_timeout_error_includes_timeout_seconds() {
        // Arrange
        let timeout = Duration::from_secs(9_001);

        // Act
        let error = lifecycle::turn_completed_timeout_error(timeout);

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("9001"));
        assert!(error_message.contains("turn/completed"));
    }

    #[test]
    fn compaction_timeout_error_includes_timeout_seconds() {
        // Arrange
        let timeout = Duration::from_mins(70);

        // Act
        let error = lifecycle::compaction_timeout_error(timeout);

        // Assert
        let error_message = error.to_string();
        assert!(error_message.contains("4200"));
        assert!(error_message.contains("compaction"));
    }

    #[test]
    fn auto_compact_input_token_threshold_uses_1050k_limit_for_codex_models() {
        // Arrange
        let gpt_56_sol_model = AgentModel::Gpt56Sol.as_str();
        let gpt_56_terra_model = AgentModel::Gpt56Terra.as_str();
        let gpt_56_luna_model = AgentModel::Gpt56Luna.as_str();
        let gpt_55_model = AgentModel::Gpt55.as_str();
        let spark_model = AgentModel::Gpt53CodexSpark.as_str();

        // Act
        let gpt_56_sol_threshold = policy::auto_compact_input_token_threshold(gpt_56_sol_model);
        let gpt_56_terra_threshold = policy::auto_compact_input_token_threshold(gpt_56_terra_model);
        let gpt_56_luna_threshold = policy::auto_compact_input_token_threshold(gpt_56_luna_model);
        let gpt_55_threshold = policy::auto_compact_input_token_threshold(gpt_55_model);
        let spark_threshold = policy::auto_compact_input_token_threshold(spark_model);

        // Assert
        assert_eq!(
            gpt_56_sol_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
        );
        assert_eq!(
            gpt_56_terra_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
        );
        assert_eq!(
            gpt_56_luna_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
        );
        assert_eq!(
            gpt_55_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT
        );
        assert_eq!(
            spark_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_128K_CONTEXT
        );
    }

    #[tokio::test]
    async fn start_thread_returns_thread_id_from_matching_response() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let request_id = Arc::new(Mutex::new(None));
        let mut transport = MockCodexRuntimeTransport::new();
        let mut sequence = Sequence::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf({
                let folder = folder.path().to_path_buf();

                move |payload| {
                    payload.get("method").and_then(Value::as_str) == Some("thread/start")
                        && payload
                            .get("params")
                            .and_then(|params| params.get("cwd"))
                            .and_then(Value::as_str)
                            == Some(folder.to_string_lossy().as_ref())
                }
            })
            .returning({
                let request_id = Arc::clone(&request_id);

                move |payload| {
                    remember_request_id(&request_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let response_id = request_id
                    .lock()
                    .expect("request id mutex should lock")
                    .clone()
                    .expect("thread/start id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({
                        "id": response_id,
                        "result": {"thread": {"id": "thread-123"}}
                    })
                    .to_string())
                })
            });

        // Act
        let thread_id = lifecycle::start_thread(
            &mut transport,
            folder.path(),
            AgentModel::Gpt55.as_str(),
            ReasoningLevel::default(),
        )
        .await;

        // Assert
        assert_eq!(thread_id.expect("thread should start"), "thread-123");
    }

    #[tokio::test]
    async fn start_or_resume_thread_falls_back_to_thread_start_after_resume_failure() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let resume_id = Arc::new(Mutex::new(None));
        let start_id = Arc::new(Mutex::new(None));
        let mut transport = MockCodexRuntimeTransport::new();
        let mut sequence = Sequence::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("thread/resume"))
            .returning({
                let resume_id = Arc::clone(&resume_id);

                move |payload| {
                    remember_request_id(&resume_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let response_id = resume_id
                    .lock()
                    .expect("resume mutex should lock")
                    .clone()
                    .expect("resume id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({
                        "id": response_id,
                        "result": {"thread": {}}
                    })
                    .to_string())
                })
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("thread/start"))
            .returning({
                let start_id = Arc::clone(&start_id);

                move |payload| {
                    remember_request_id(&start_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let response_id = start_id
                    .lock()
                    .expect("start mutex should lock")
                    .clone()
                    .expect("start id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({
                        "id": response_id,
                        "result": {"thread": {"id": "thread-started"}}
                    })
                    .to_string())
                })
            });

        // Act
        let thread = lifecycle::start_or_resume_thread(
            &mut transport,
            folder.path(),
            AgentModel::Gpt55.as_str(),
            Some("thread-existing"),
            ReasoningLevel::default(),
        )
        .await;

        // Assert
        assert_eq!(
            thread.expect("thread should be started after resume failure"),
            ("thread-started".to_string(), false)
        );
    }

    #[tokio::test]
    async fn send_compact_request_resets_latest_input_tokens_on_success() {
        // Arrange
        let compact_id = Arc::new(Mutex::new(None));
        let mut latest_input_tokens = 450_000;
        let mut transport = MockCodexRuntimeTransport::new();
        let mut sequence = Sequence::new();

        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| {
                payload.get("method").and_then(Value::as_str) == Some("thread/compact/start")
            })
            .returning({
                let compact_id = Arc::clone(&compact_id);

                move |payload| {
                    remember_request_id(&compact_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_| {
                let response_id = compact_id
                    .lock()
                    .expect("compact mutex should lock")
                    .clone()
                    .expect("compact id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({"id": response_id, "result": {}}).to_string())
                })
            });
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "method": "turn/completed",
                            "params": {"turn": {"status": "completed"}}
                        })
                        .to_string(),
                    ))
                })
            });

        // Act
        let result =
            lifecycle::send_compact_request(&mut transport, "thread-1", &mut latest_input_tokens)
                .await;

        // Assert
        result.expect("compaction should succeed");
        assert_eq!(latest_input_tokens, 0);
    }

    #[tokio::test]
    async fn execute_turn_event_loop_answers_user_input_request_without_blocking() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");
        let turn_start_id = Arc::new(Mutex::new(None));
        let mut transport = MockCodexRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

        expect_user_input_request_turn(&mut transport, &mut sequence, turn_start_id);

        // Act
        let result = lifecycle::execute_turn_event_loop(
            &mut transport,
            folder.path(),
            AgentModel::Gpt55.as_str(),
            "thread-1",
            "Implement the task",
            ReasoningLevel::default(),
            stream_tx,
        )
        .await;

        // Assert
        assert_eq!(result.expect("turn should complete"), (String::new(), 0, 0));
    }

    /// Expects a user-input request to receive an empty response before turn
    /// completion.
    fn expect_user_input_request_turn(
        transport: &mut MockCodexRuntimeTransport,
        sequence: &mut Sequence,
        turn_start_id: Arc<Mutex<Option<String>>>,
    ) {
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("turn/start"))
            .returning({
                let turn_start_id = Arc::clone(&turn_start_id);

                move |payload| {
                    remember_request_id(&turn_start_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(move || {
                let response_id = turn_start_id
                    .lock()
                    .expect("turn/start mutex should lock")
                    .clone()
                    .expect("turn/start id should be recorded");

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "id": response_id,
                            "result": {"turn": {"id": "turn-123"}}
                        })
                        .to_string(),
                    ))
                })
            });
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "id": "user-input-1",
                            "method": "item/tool/requestUserInput",
                            "params": {
                                "questions": [{
                                    "id": "approval",
                                    "question": "Allow this action?"
                                }]
                            }
                        })
                        .to_string(),
                    ))
                })
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| {
                payload
                    == &serde_json::json!({
                        "id": "user-input-1",
                        "result": {"answers": {}}
                    })
            })
            .returning(|_| Box::pin(async { Ok(()) }));
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "method": "turn/completed",
                            "params": {
                                "turn": {
                                    "id": "turn-123",
                                    "status": "completed"
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    #[tokio::test]
    async fn run_turn_with_runtime_compacts_proactively_before_turn_start() {
        // Arrange
        let mut state = build_runtime_state(
            "thread-1",
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_1050K_CONTEXT,
        );
        let compact_id = Arc::new(Mutex::new(None));
        let turn_id = Arc::new(Mutex::new(None));
        let mut transport = MockCodexRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

        expect_proactive_compaction_turn(&mut transport, &mut sequence, compact_id, turn_id);

        // Act
        let result = lifecycle::run_turn_with_runtime(
            &mut transport,
            &mut state,
            "Implement the task",
            ReasoningLevel::default(),
            stream_tx,
        )
        .await;

        // Assert
        let (message, input_tokens, output_tokens) =
            result.expect("turn should complete after proactive compaction");
        assert_eq!(message, String::new());
        assert_eq!(input_tokens, 12);
        assert_eq!(output_tokens, 3);
        assert_eq!(state.latest_input_tokens, 12);
        assert_eq!(
            stream_rx.try_recv().ok(),
            Some(AppServerStreamEvent::ProgressUpdate(
                "Compacting context".to_string()
            ))
        );
    }

    /// Expects a proactive compaction request followed by a successful turn.
    fn expect_proactive_compaction_turn(
        transport: &mut MockCodexRuntimeTransport,
        sequence: &mut Sequence,
        compact_id: Arc<Mutex<Option<String>>>,
        turn_id: Arc<Mutex<Option<String>>>,
    ) {
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| {
                payload.get("method").and_then(Value::as_str) == Some("thread/compact/start")
            })
            .returning({
                let compact_id = Arc::clone(&compact_id);

                move |payload| {
                    remember_request_id(&compact_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_wait_for_response_line()
            .times(1)
            .in_sequence(sequence)
            .returning(move |_| {
                let response_id = compact_id
                    .lock()
                    .expect("compact mutex should lock")
                    .clone()
                    .expect("compact id should be recorded");

                Box::pin(async move {
                    Ok(serde_json::json!({"id": response_id, "result": {}}).to_string())
                })
            });
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .returning(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "method": "turn/completed",
                            "params": {"turn": {"status": "completed"}}
                        })
                        .to_string(),
                    ))
                })
            });
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("turn/start"))
            .returning({
                let turn_id = Arc::clone(&turn_id);

                move |payload| {
                    remember_request_id(&turn_id, &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_next_stdout()
            .in_sequence(sequence)
            .times(1)
            .return_once(move || {
                let response_id = turn_id
                    .lock()
                    .expect("turn mutex should lock")
                    .clone()
                    .expect("turn id should be recorded");

                Box::pin(async move {
                    Ok(Some(
                        serde_json::json!({
                            "id": response_id,
                            "result": {"turn": {"id": "turn-123"}}
                        })
                        .to_string(),
                    ))
                })
            });
        transport
            .expect_next_stdout()
            .times(1)
            .in_sequence(sequence)
            .return_once(|| {
                Box::pin(async {
                    Ok(Some(
                        serde_json::json!({
                            "method": "turn/completed",
                            "params": {
                                "turn": {
                                    "id": "turn-123",
                                    "status": "completed",
                                    "usage": {"inputTokens": 12, "outputTokens": 3}
                                }
                            }
                        })
                        .to_string(),
                    ))
                })
            });
    }

    #[test]
    fn resolve_turn_usage_prefers_completed_usage_over_stream_usage() {
        // Arrange
        let completed_turn_usage = Some((33, 7));
        let latest_stream_usage = Some((18, 4));

        // Act
        let usage = usage::resolve_turn_usage(completed_turn_usage, latest_stream_usage);

        // Assert
        assert_eq!(usage, (33, 7));
    }

    #[test]
    /// Verifies Codex auto-edit accepts command approvals when the app-server
    /// emits them despite the non-interactive approval policy.
    fn build_pre_action_approval_response_accepts_command_requests() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "approval-1",
            "method": "item/commandExecution/requestApproval",
        });
        let session_folder = Path::new("/tmp/session");

        // Act
        let approval_response =
            policy::build_server_request_response(&response_value, session_folder)
                .expect("approval response should be generated");

        // Assert
        assert_eq!(
            approval_response,
            serde_json::json!({
                "id": "approval-1",
                "result": {
                    "decision": "accept"
                }
            })
        );
    }

    #[test]
    /// Verifies Codex thread startup asks the app-server to avoid interactive
    /// approval prompts during Agentty-managed turns.
    fn build_thread_start_payload_uses_never_approval_policy() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");

        // Act
        let payload = lifecycle::build_thread_start_payload(
            folder.path(),
            AgentModel::Gpt55.as_str(),
            ReasoningLevel::default(),
            "thread-start-1",
        );

        // Assert
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("approvalPolicy"))
                .and_then(Value::as_str),
            Some("never")
        );
    }

    #[test]
    fn build_pre_action_approval_response_accepts_session_local_file_change() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "approval-1",
            "method": "item/fileChange/requestApproval",
            "params": {
                "changes": [{
                    "path": "/tmp/session/src/main.rs"
                }]
            }
        });
        let session_folder = Path::new("/tmp/session");

        // Act
        let approval_response =
            policy::build_server_request_response(&response_value, session_folder)
                .expect("approval response should be generated");

        // Assert
        assert_eq!(
            approval_response
                .get("result")
                .and_then(|result| result.get("decision"))
                .and_then(Value::as_str),
            Some("accept")
        );
    }

    #[test]
    fn build_pre_action_approval_response_rejects_outside_file_change() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "approval-1",
            "method": "item/fileChange/requestApproval",
            "params": {
                "changes": [{
                    "path": "/tmp/project/src/main.rs"
                }]
            }
        });
        let session_folder = Path::new("/tmp/session");

        // Act
        let approval_response =
            policy::build_server_request_response(&response_value, session_folder)
                .expect("approval response should be generated");

        // Assert
        assert_eq!(
            approval_response
                .get("result")
                .and_then(|result| result.get("decision"))
                .and_then(Value::as_str),
            Some("reject")
        );
    }

    #[test]
    fn build_server_request_response_grants_no_additional_permissions() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "permission-1",
            "method": "item/permissions/requestApproval",
            "params": {
                "permissions": {
                    "network": { "enabled": true }
                }
            }
        });
        let session_folder = Path::new("/tmp/session");

        // Act
        let response = policy::build_server_request_response(&response_value, session_folder)
            .expect("permission response should be generated");

        // Assert
        assert_eq!(
            response,
            serde_json::json!({
                "id": "permission-1",
                "result": {
                    "permissions": {},
                    "scope": "turn"
                }
            })
        );
    }

    #[test]
    fn build_server_request_response_declines_mcp_elicitation() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "elicitation-1",
            "method": "mcpServer/elicitation/request",
            "params": {
                "message": "Allow this MCP action?"
            }
        });
        let session_folder = Path::new("/tmp/session");

        // Act
        let response = policy::build_server_request_response(&response_value, session_folder)
            .expect("elicitation response should be generated");

        // Assert
        assert_eq!(
            response,
            serde_json::json!({
                "id": "elicitation-1",
                "result": {
                    "action": "decline"
                }
            })
        );
    }

    #[test]
    fn parse_turn_completed_returns_success_for_completed_turn() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "turn/completed",
            "params": {
                "turn": {
                    "id": "turn-123",
                    "status": "completed"
                }
            }
        });

        // Act
        let turn_result = stream_parser::parse_turn_completed(&response_value, Some("turn-123"));

        // Assert
        assert_eq!(turn_result, Some(Ok(())));
    }

    #[test]
    fn build_turn_start_payload_sets_structured_output_schema() {
        // Arrange
        let folder = tempdir().expect("temporary folder should be created");

        // Act
        let payload = lifecycle::build_turn_start_payload(
            folder.path(),
            AgentModel::Gpt55.as_str(),
            ReasoningLevel::default(),
            "thread-123",
            "Implement the task",
            "turn-start-1",
        );

        // Assert
        assert_eq!(
            payload
                .get("params")
                .and_then(|params| params.get("outputSchema"))
                .and_then(|schema| schema.get("type"))
                .and_then(Value::as_str),
            Some("object")
        );
    }
}
