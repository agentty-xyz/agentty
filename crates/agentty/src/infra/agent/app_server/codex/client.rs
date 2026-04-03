//! Codex app-server client orchestration.

use tokio::sync::mpsc;

use super::lifecycle::{self, CodexRuntimeState};
use super::transport::CodexStdioTransport;
use crate::infra::app_server::{
    self, AppServerClient, AppServerError, AppServerFuture, AppServerSessionRegistry,
    AppServerStreamEvent, AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::app_server_transport;

/// Production [`AppServerClient`] backed by `codex app-server` process
/// instances.
pub(crate) struct RealCodexAppServerClient {
    sessions: AppServerSessionRegistry<CodexSessionRuntime>,
}

impl RealCodexAppServerClient {
    /// Creates an empty app-server runtime registry.
    pub(crate) fn new() -> Self {
        Self {
            sessions: AppServerSessionRegistry::new("Codex"),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<CodexSessionRuntime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, AppServerError> {
        let stream_tx = stream_tx.clone();
        let reasoning_level = request.reasoning_level;

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            app_server::RuntimeInspector {
                matches_request: CodexSessionRuntime::matches_request,
                pid: |runtime| runtime.child.id(),
                provider_conversation_id: CodexSessionRuntime::provider_conversation_id,
                restored_context: CodexSessionRuntime::restored_context,
            },
            |request| {
                let request = request.clone();

                Box::pin(async move {
                    let (child, transport, state) = lifecycle::start_runtime(&request).await?;

                    Ok(CodexSessionRuntime {
                        child,
                        state,
                        transport,
                    })
                })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

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
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Terminates one runtime process and waits for process exit.
    async fn shutdown_runtime(session: &mut CodexSessionRuntime) {
        session.transport.close_stdin();
        app_server_transport::shutdown_child(&mut session.child).await;
    }
}

impl Default for RealCodexAppServerClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServerClient for RealCodexAppServerClient {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, AppServerError>> {
        let sessions = self.sessions.clone();

        Box::pin(async move { Self::run_turn_internal(&sessions, request, &stream_tx).await })
    }

    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()> {
        let sessions = self.sessions.clone();

        Box::pin(async move {
            let Ok(Some(mut session_runtime)) = sessions.take_session(&session_id) else {
                return;
            };

            RealCodexAppServerClient::shutdown_runtime(&mut session_runtime).await;
        })
    }
}

/// Active Codex app-server session runtime.
struct CodexSessionRuntime {
    child: tokio::process::Child,
    state: CodexRuntimeState,
    transport: CodexStdioTransport,
}

impl CodexSessionRuntime {
    /// Returns whether the stored runtime configuration matches one request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.state.folder == request.folder && self.state.model == request.model
    }

    /// Returns whether the runtime was bootstrapped by resuming stored thread
    /// context.
    fn restored_context(&self) -> bool {
        self.state.restored_context
    }

    /// Returns the active provider-native thread identifier, or `None` when
    /// the runtime has not yet started a thread.
    fn provider_conversation_id(&self) -> Option<String> {
        if self.state.thread_id.is_empty() {
            None
        } else {
            Some(self.state.thread_id.clone())
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use mockall::Sequence;
    use serde_json::Value;
    use tempfile::tempdir;
    use tokio::sync::mpsc;

    use super::*;
    use crate::domain::agent::{AgentModel, ReasoningLevel};
    use crate::infra::agent::app_server::codex::{
        MockCodexRuntimeTransport, lifecycle, policy, stream_parser, usage,
    };

    /// Creates runtime state for one synthetic Codex session path.
    fn build_runtime_state(thread_id: &str, latest_input_tokens: u64) -> CodexRuntimeState {
        let folder = std::env::temp_dir().join(format!(
            "agentty-codex-runtime-state-{thread_id}-{latest_input_tokens}"
        ));
        let mut state = CodexRuntimeState::new(folder, AgentModel::Gpt53Codex.as_str().to_string());
        state.thread_id = thread_id.to_string();
        state.latest_input_tokens = latest_input_tokens;

        state
    }

    /// Captures the dynamic request id from a written payload and returns it
    /// through the provided mutex.
    fn remember_request_id(id_store: Arc<Mutex<Option<String>>>, payload: &Value) {
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
    fn auto_compact_input_token_threshold_uses_400k_limit_for_codex_models() {
        // Arrange
        let gpt_54_model = AgentModel::Gpt54.as_str();
        let gpt_53_codex_model = AgentModel::Gpt53Codex.as_str();

        // Act
        let gpt_54_threshold = policy::auto_compact_input_token_threshold(gpt_54_model);
        let gpt_53_threshold = policy::auto_compact_input_token_threshold(gpt_53_codex_model);

        // Assert
        assert_eq!(
            gpt_54_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT
        );
        assert_eq!(
            gpt_53_threshold,
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT
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
                    remember_request_id(Arc::clone(&request_id), &payload);

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
            AgentModel::Gpt53Codex.as_str(),
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
                    remember_request_id(Arc::clone(&resume_id), &payload);

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
                    remember_request_id(Arc::clone(&start_id), &payload);

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
            AgentModel::Gpt53Codex.as_str(),
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
                    remember_request_id(Arc::clone(&compact_id), &payload);

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
    async fn run_turn_with_runtime_compacts_proactively_before_turn_start() {
        // Arrange
        let mut state = build_runtime_state(
            "thread-1",
            policy::AUTO_COMPACT_INPUT_TOKEN_THRESHOLD_400K_CONTEXT,
        );
        let compact_id = Arc::new(Mutex::new(None));
        let turn_id = Arc::new(Mutex::new(None));
        let mut transport = MockCodexRuntimeTransport::new();
        let mut sequence = Sequence::new();
        let (stream_tx, mut stream_rx) = mpsc::unbounded_channel();

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
                    remember_request_id(Arc::clone(&compact_id), &payload);

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
        transport
            .expect_write_json_line()
            .times(1)
            .in_sequence(&mut sequence)
            .withf(|payload| payload.get("method").and_then(Value::as_str) == Some("turn/start"))
            .returning({
                let turn_id = Arc::clone(&turn_id);

                move |payload| {
                    remember_request_id(Arc::clone(&turn_id), &payload);

                    Box::pin(async { Ok(()) })
                }
            });
        transport
            .expect_next_stdout()
            .in_sequence(&mut sequence)
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
            .in_sequence(&mut sequence)
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
    fn build_pre_action_approval_response_for_command_request_uses_mode_decision() {
        // Arrange
        let response_value = serde_json::json!({
            "id": "approval-1",
            "method": "item/commandExecution/requestApproval",
        });

        // Act
        let approval_response = policy::build_pre_action_approval_response(&response_value)
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
            AgentModel::Gpt53Codex.as_str(),
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
