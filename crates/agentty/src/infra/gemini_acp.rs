//! Gemini ACP-backed implementation of the shared app-server client.

use std::path::PathBuf;

use serde_json::Value;
use tokio::io::{AsyncBufReadExt, BufReader, Lines};
use tokio::sync::mpsc;

use crate::infra::app_server::{
    self, AppServerClient, AppServerFuture, AppServerSessionRegistry, AppServerStreamEvent,
    AppServerTurnRequest, AppServerTurnResponse,
};
use crate::infra::app_server_transport::{
    self, extract_json_error_message, response_id_matches, write_json_line,
};

/// Production [`AppServerClient`] backed by `gemini --experimental-acp`.
pub struct RealGeminiAcpClient {
    sessions: AppServerSessionRegistry<GeminiSessionRuntime>,
}

impl RealGeminiAcpClient {
    /// Creates an empty ACP runtime registry for Gemini sessions.
    pub fn new() -> Self {
        Self {
            sessions: AppServerSessionRegistry::new("Gemini ACP"),
        }
    }

    /// Runs one turn with automatic restart-and-retry on runtime failures.
    async fn run_turn_internal(
        sessions: &AppServerSessionRegistry<GeminiSessionRuntime>,
        request: AppServerTurnRequest,
        stream_tx: &mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<AppServerTurnResponse, String> {
        let stream_tx = stream_tx.clone();

        app_server::run_turn_with_restart_retry(
            sessions,
            request,
            GeminiSessionRuntime::matches_request,
            |runtime| runtime.child.id(),
            |request| {
                let request = request.clone();

                Box::pin(async move { Self::start_runtime(&request).await })
            },
            move |runtime, prompt| {
                let stream_tx = stream_tx.clone();

                Box::pin(Self::run_turn_with_runtime(runtime, prompt, stream_tx))
            },
            |runtime| Box::pin(Self::shutdown_runtime(runtime)),
        )
        .await
    }

    /// Starts one Gemini ACP runtime, initializes it, and creates a session.
    async fn start_runtime(request: &AppServerTurnRequest) -> Result<GeminiSessionRuntime, String> {
        let mut command = tokio::process::Command::new("gemini");
        command
            .arg("--experimental-acp")
            .arg("--model")
            .arg(&request.model)
            .current_dir(&request.folder)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::null());

        let mut child = command
            .spawn()
            .map_err(|error| format!("Failed to spawn `gemini --experimental-acp`: {error}"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| "Gemini ACP stdin is unavailable".to_string())?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| "Gemini ACP stdout is unavailable".to_string())?;
        let mut session = GeminiSessionRuntime {
            child,
            folder: request.folder.clone(),
            model: request.model.clone(),
            session_id: String::new(),
            stdin,
            stdout_lines: BufReader::new(stdout).lines(),
        };

        Self::initialize_runtime(&mut session).await?;
        session.session_id = Self::start_session(&mut session).await?;

        Ok(session)
    }

    /// Sends the ACP initialize handshake.
    async fn initialize_runtime(session: &mut GeminiSessionRuntime) -> Result<(), String> {
        let initialization_request_id = format!("init-{}", uuid::Uuid::new_v4());
        let initialization_request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": initialization_request_id,
            "method": "initialize",
            "params": {
                "protocolVersion": 1,
                "clientCapabilities": {}
            }
        });
        write_json_line(&mut session.stdin, &initialization_request).await?;
        app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &initialization_request_id,
        )
        .await?;

        let initialized_notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": "initialized"
        });
        write_json_line(&mut session.stdin, &initialized_notification).await?;

        Ok(())
    }

    /// Creates one ACP session and returns the assigned `sessionId`.
    async fn start_session(session: &mut GeminiSessionRuntime) -> Result<String, String> {
        let session_new_id = format!("session-new-{}", uuid::Uuid::new_v4());
        let session_new_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": session_new_id,
            "method": "session/new",
            "params": {
                "cwd": session.folder.to_string_lossy(),
                "mcpServers": []
            }
        });
        write_json_line(&mut session.stdin, &session_new_payload).await?;
        let response_line = app_server_transport::wait_for_response_line(
            &mut session.stdout_lines,
            &session_new_id,
        )
        .await?;
        let response_value = serde_json::from_str::<Value>(&response_line)
            .map_err(|error| format!("Failed to parse session/new response JSON: {error}"))?;

        response_value
            .get("result")
            .and_then(|result| result.get("sessionId"))
            .and_then(Value::as_str)
            .map(ToString::to_string)
            .ok_or_else(|| "Gemini ACP `session/new` response missing `sessionId`".to_string())
    }

    /// Sends one prompt turn and waits for the matching prompt response id.
    async fn run_turn_with_runtime(
        session: &mut GeminiSessionRuntime,
        prompt: &str,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> Result<(String, u64, u64), String> {
        let prompt_id = format!("session-prompt-{}", uuid::Uuid::new_v4());
        let session_prompt_payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": prompt_id,
            "method": "session/prompt",
            "params": {
                "sessionId": session.session_id,
                "prompt": [{
                    "type": "text",
                    "text": prompt
                }]
            }
        });
        write_json_line(&mut session.stdin, &session_prompt_payload).await?;

        let mut assistant_message = String::new();
        tokio::time::timeout(app_server_transport::TURN_TIMEOUT, async {
            loop {
                let stdout_line = session
                    .stdout_lines
                    .next_line()
                    .await
                    .map_err(|error| format!("Failed reading Gemini ACP stdout: {error}"))?
                    .ok_or_else(|| {
                        "Gemini ACP terminated before prompt completion response".to_string()
                    })?;

                if stdout_line.trim().is_empty() {
                    continue;
                }

                let Ok(response_value) = serde_json::from_str::<Value>(&stdout_line) else {
                    continue;
                };

                if response_id_matches(&response_value, &prompt_id) {
                    if response_value.get("error").is_some() {
                        return Err(extract_json_error_message(&response_value).unwrap_or_else(
                            || "Gemini ACP returned an error for `session/prompt`".to_string(),
                        ));
                    }

                    return Ok((assistant_message, 0, 0));
                }

                if let Some(progress) =
                    extract_progress_update(&response_value, &session.session_id)
                {
                    let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(progress));
                }

                if let Some(chunk) =
                    extract_assistant_message_chunk(&response_value, &session.session_id)
                {
                    assistant_message.push_str(chunk.as_str());
                }
            }
        })
        .await
        .map_err(|_| {
            format!(
                "Timed out waiting for Gemini ACP prompt completion after {} seconds",
                app_server_transport::TURN_TIMEOUT.as_secs()
            )
        })?
    }

    /// Terminates one Gemini ACP runtime process.
    async fn shutdown_runtime(session: &mut GeminiSessionRuntime) {
        app_server_transport::shutdown_child(&mut session.child).await;
    }
}

impl Default for RealGeminiAcpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl AppServerClient for RealGeminiAcpClient {
    fn run_turn(
        &self,
        request: AppServerTurnRequest,
        stream_tx: mpsc::UnboundedSender<AppServerStreamEvent>,
    ) -> AppServerFuture<Result<AppServerTurnResponse, String>> {
        let sessions = self.sessions.clone();

        Box::pin(async move { Self::run_turn_internal(&sessions, request, &stream_tx).await })
    }

    fn shutdown_session(&self, session_id: String) -> AppServerFuture<()> {
        let sessions = self.sessions.clone();

        Box::pin(async move {
            let Ok(Some(mut session_runtime)) = sessions.take_session(&session_id) else {
                return;
            };

            Self::shutdown_runtime(&mut session_runtime).await;
        })
    }
}

struct GeminiSessionRuntime {
    child: tokio::process::Child,
    folder: PathBuf,
    model: String,
    session_id: String,
    stdin: tokio::process::ChildStdin,
    stdout_lines: Lines<BufReader<tokio::process::ChildStdout>>,
}

impl GeminiSessionRuntime {
    /// Returns whether the runtime matches one incoming turn request.
    fn matches_request(&self, request: &AppServerTurnRequest) -> bool {
        self.folder == request.folder && self.model == request.model
    }
}

/// Extracts assistant text chunks from ACP `session/update` events.
fn extract_assistant_message_chunk(
    response_value: &Value,
    expected_session_id: &str,
) -> Option<String> {
    if extract_session_update_kind(response_value, expected_session_id)? != "agent_message_chunk" {
        return None;
    }

    response_value
        .get("params")
        .and_then(|params| params.get("update"))
        .and_then(|update| update.get("content"))
        .and_then(|content| content.get("text"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

/// Extracts a short user-facing progress label from ACP `session/update`.
fn extract_progress_update(response_value: &Value, expected_session_id: &str) -> Option<String> {
    let session_update = extract_session_update_kind(response_value, expected_session_id)?;
    match session_update {
        "agent_thought_chunk" => Some("Thinking".to_string()),
        "tool_call" | "tool_call_update" => {
            let update = response_value.get("params")?.get("update")?;
            if let Some(status) = update.get("status").and_then(Value::as_str)
                && status.eq_ignore_ascii_case("completed")
            {
                return Some("Tool completed".to_string());
            }

            if let Some(title) = update.get("title").and_then(Value::as_str) {
                return Some(format!("Using tool: {title}"));
            }

            if let Some(kind) = update.get("kind").and_then(Value::as_str) {
                return Some(format!("Using tool: {kind}"));
            }

            Some("Using tool".to_string())
        }
        _ => None,
    }
}

/// Returns the ACP `sessionUpdate` kind for the matching session update.
fn extract_session_update_kind<'value>(
    response_value: &'value Value,
    expected_session_id: &str,
) -> Option<&'value str> {
    if response_value.get("method").and_then(Value::as_str) != Some("session/update") {
        return None;
    }

    let params = response_value.get("params")?;
    if params.get("sessionId").and_then(Value::as_str)? != expected_session_id {
        return None;
    }

    params
        .get("update")
        .and_then(|update| update.get("sessionUpdate"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_session_update_kind_reads_matching_update_kind() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk"
                }
            }
        });

        // Act
        let session_update_kind = extract_session_update_kind(&response_value, "session-1");

        // Assert
        assert_eq!(session_update_kind, Some("agent_message_chunk"));
    }

    #[test]
    fn extract_session_update_kind_ignores_mismatched_session() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-2",
                "update": {
                    "sessionUpdate": "agent_message_chunk"
                }
            }
        });

        // Act
        let session_update_kind = extract_session_update_kind(&response_value, "session-1");

        // Assert
        assert_eq!(session_update_kind, None);
    }

    #[test]
    fn extract_assistant_message_chunk_returns_text_for_message_chunk() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": "Hello from ACP"
                    }
                }
            }
        });

        // Act
        let message_chunk = extract_assistant_message_chunk(&response_value, "session-1");

        // Assert
        assert_eq!(message_chunk, Some("Hello from ACP".to_string()));
    }

    #[test]
    fn extract_progress_update_returns_thinking_for_thought_chunks() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "agent_thought_chunk",
                    "content": {
                        "type": "text",
                        "text": "internal thought"
                    }
                }
            }
        });

        // Act
        let progress_update = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress_update, Some("Thinking".to_string()));
    }

    #[test]
    fn extract_progress_update_returns_tool_title_for_in_progress_tool_call() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call",
                    "status": "in_progress",
                    "title": "Cargo.toml"
                }
            }
        });

        // Act
        let progress_update = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress_update, Some("Using tool: Cargo.toml".to_string()));
    }

    #[test]
    fn extract_progress_update_returns_tool_completed_for_completed_tool_call() {
        // Arrange
        let response_value = serde_json::json!({
            "method": "session/update",
            "params": {
                "sessionId": "session-1",
                "update": {
                    "sessionUpdate": "tool_call_update",
                    "status": "completed"
                }
            }
        });

        // Act
        let progress_update = extract_progress_update(&response_value, "session-1");

        // Assert
        assert_eq!(progress_update, Some("Tool completed".to_string()));
    }
}
