use super::*;

/// Creates runtime state for one synthetic Codex session path.
pub(super) fn build_runtime_state(thread_id: &str, latest_input_tokens: u64) -> CodexRuntimeState {
    let folder = std::env::temp_dir().join(format!(
        "agentty-codex-runtime-state-{thread_id}-{latest_input_tokens}"
    ));
    let mut state = CodexRuntimeState::new(
        folder,
        AgentModel::Gpt56Sol.as_str().to_string(),
        crate::model::permission::PermissionMode::AutoEdit,
    );
    state.thread_id = thread_id.to_string();
    state.latest_input_tokens = latest_input_tokens;

    state
}

/// Captures the dynamic request id from a written payload and returns it
/// through the provided mutex.
pub(super) fn remember_request_id(id_store: &Arc<Mutex<Option<String>>>, payload: &Value) {
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    if let Ok(mut guard) = id_store.lock() {
        *guard = id;
    }
}

/// Builds one Codex session runtime whose stdin is already closed so turn
/// writes fail deterministically without a live app-server process.
pub(super) fn build_stopped_session_runtime(thread_id: &str) -> CodexSessionRuntime {
    let (child, stdin, stdout) =
        app_server_transport::spawn_runtime_command(std::process::Command::new("cat"), "cat")
            .expect("`cat` should spawn as a runtime stand-in");
    let mut transport = AppServerStdioTransport::new(
        stdin,
        stdout,
        "Codex app-server stdin is unavailable",
        "Failed reading Codex app-server stdout",
    );
    transport.close_stdin();

    CodexSessionRuntime {
        child,
        state: build_runtime_state(thread_id, 0),
        transport,
    }
}

/// Expects one turn to emit commentary before carrying its structured
/// final answer in `turn/completed`.
pub(super) fn expect_commentary_then_completed_final_turn(
    transport: &mut MockCodexRuntimeTransport,
    sequence: &mut Sequence,
    turn_start_id: Arc<Mutex<Option<String>>>,
    final_response: &'static str,
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
                        "method": "item/completed",
                        "params": {
                            "turnId": "turn-123",
                            "item": {
                                "type": "agentMessage",
                                "phase": "commentary",
                                "text": "I'll inspect the current code."
                            }
                        }
                    })
                    .to_string(),
                ))
            })
        });
    transport
        .expect_next_stdout()
        .times(1)
        .in_sequence(sequence)
        .return_once(move || {
            Box::pin(async move {
                Ok(Some(
                    serde_json::json!({
                        "method": "turn/completed",
                        "params": {
                            "turnId": "turn-123",
                            "turn": {
                                "status": "completed",
                                "items": [{
                                    "type": "agentMessage",
                                    "phase": "final_answer",
                                    "text": final_response
                                }]
                            }
                        }
                    })
                    .to_string(),
                ))
            })
        });
}

/// Expects a user-input request to receive an empty response before turn
/// completion.
pub(super) fn expect_user_input_request_turn(
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

/// Expects a proactive compaction request followed by a successful turn.
pub(super) fn expect_proactive_compaction_turn(
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

            Box::pin(
                async move { Ok(serde_json::json!({"id": response_id, "result": {}}).to_string()) },
            )
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

/// Expects one `turn/start` request that fails with a context-overflow
/// error response.
pub(super) fn expect_context_overflow_turn(
    transport: &mut MockCodexRuntimeTransport,
    sequence: &mut Sequence,
    turn_id: Arc<Mutex<Option<String>>>,
) {
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
        .times(1)
        .in_sequence(sequence)
        .return_once(move || {
            let response_id = turn_id
                .lock()
                .expect("turn/start mutex should lock")
                .clone()
                .expect("turn/start id should be recorded");

            Box::pin(async move {
                Ok(Some(
                    serde_json::json!({
                        "id": response_id,
                        "error": {
                            "message": "[contextWindowExceeded] Codex ran out of room."
                        }
                    })
                    .to_string(),
                ))
            })
        });
}

/// Expects the compaction request that follows a context overflow.
pub(super) fn expect_reactive_compaction(
    transport: &mut MockCodexRuntimeTransport,
    sequence: &mut Sequence,
    compact_id: Arc<Mutex<Option<String>>>,
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

            Box::pin(
                async move { Ok(serde_json::json!({"id": response_id, "result": {}}).to_string()) },
            )
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
                        "params": {"turn": {"status": "completed"}}
                    })
                    .to_string(),
                ))
            })
        });
}

/// Expects the retried turn that follows compaction and completes with
/// usage.
pub(super) fn expect_retried_turn_after_compaction(
    transport: &mut MockCodexRuntimeTransport,
    sequence: &mut Sequence,
    turn_id: Arc<Mutex<Option<String>>>,
) {
    transport
        .expect_write_json_line()
        .times(1)
        .in_sequence(sequence)
        .withf(|payload| {
            payload.get("method").and_then(Value::as_str) == Some("turn/start")
                && payload
                    .pointer("/params/serviceTier")
                    .and_then(Value::as_str)
                    == Some("fast")
        })
        .returning({
            let turn_id = Arc::clone(&turn_id);

            move |payload| {
                remember_request_id(&turn_id, &payload);

                Box::pin(async { Ok(()) })
            }
        });
    transport
        .expect_next_stdout()
        .times(1)
        .in_sequence(sequence)
        .return_once(move || {
            let response_id = turn_id
                .lock()
                .expect("retried turn mutex should lock")
                .clone()
                .expect("retried turn id should be recorded");

            Box::pin(async move {
                Ok(Some(
                    serde_json::json!({
                        "id": response_id,
                        "result": {"turn": {"id": "turn-456"}}
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
                                "id": "turn-456",
                                "status": "completed",
                                "usage": {"inputTokens": 21, "outputTokens": 5}
                            }
                        }
                    })
                    .to_string(),
                ))
            })
        });
}
