use std::path::PathBuf;
use std::sync::Arc;

use ag_protocol::TurnPromptAttachment;
use tokio::sync::mpsc;

use super::support::{collect_pid_updates, make_ok_response, make_turn_request};
use crate::app_server::{AppServerStreamEvent, AppServerTurnResponse, MockAppServerClient};
use crate::channel::app_server::AppServerAgentChannel;
use crate::channel::contract::{AgentChannel, TurnEvent};
use crate::model::agent::{AgentKind, ReasoningLevel};

#[tokio::test]
async fn forwards_runtime_pid_and_clears_it_after_non_retained_turn() {
    // Arrange
    let mut client = MockAppServerClient::new();
    let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
    let mut finish_rx = Some(finish_rx);
    client
        .expect_run_turn()
        .times(1)
        .returning(move |_, stream_tx| {
            let finish_rx = finish_rx.take().expect("single turn");
            Box::pin(async move {
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(123)));
                finish_rx.await.expect("release turn");

                Ok(make_ok_response(r#"{"answer":"ok","questions":[]}"#))
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(client), AgentKind::Gemini);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let turn = tokio::spawn(async move {
        channel
            .run_turn("session".to_string(), make_turn_request(), events_tx)
            .await
    });
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), events_rx.recv())
        .await
        .expect("PID arrives during turn");

    // Assert
    assert!(matches!(event, Some(TurnEvent::PidUpdate(Some(123)))));
    assert!(!turn.is_finished());
    finish_tx.send(()).expect("finish turn");
    turn.await.expect("join turn").expect("successful turn");
    assert!(matches!(
        events_rx.recv().await,
        Some(TurnEvent::PidUpdate(None))
    ));
}

#[tokio::test]
/// Verifies app-server turns pass pasted image prompt payloads through to
/// the underlying app-server client.
async fn test_run_turn_allows_image_attachments() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .times(1)
        .returning(|request, _stream_tx| {
            assert_eq!(request.prompt.attachments.len(), 1);

            Box::pin(async { Ok(make_ok_response(r#"{"answer":"codex ok","questions":[]}"#)) })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, _events_rx) = mpsc::unbounded_channel();
    let mut request = make_turn_request();
    request.prompt.attachments.push(TurnPromptAttachment {
        placeholder: "[Image #1]".to_string(),
        local_image_path: PathBuf::from("/tmp/image.png"),
    });

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), request, events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(result.assistant_message.to_display_text(), "codex ok");
}

#[tokio::test]
/// Verifies client turn failure propagates as `Err(AgentError)`.
async fn test_run_turn_client_failure_returns_agent_error() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(42)));
            let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(43)));

            Box::pin(async {
                Err(crate::app_server::AppServerError::Provider(
                    "server timeout".to_string(),
                ))
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await;

    // Assert
    let error_message = result
        .expect_err("expected Err on server timeout")
        .to_string();
    assert!(error_message.contains("server timeout"));
    assert_eq!(
        collect_pid_updates(&mut events_rx),
        vec![Some(42), Some(43), None]
    );
}

#[tokio::test]
/// Verifies `TurnResult` carries the correct token counts and context-reset
/// flag from the underlying `AppServerTurnResponse`.
async fn test_run_turn_returns_correct_token_counts_and_context_reset() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, _stream_tx| {
            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"Result","questions":[]}"#.to_string(),
                    context_reset: true,
                    input_tokens: 100,
                    output_tokens: 50,
                    pid: Some(1234),
                    provider_conversation_id: None,
                })
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, _events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(result.assistant_message.to_display_text(), "Result");
    assert!(result.context_reset);
    assert_eq!(result.input_tokens, 100);
    assert_eq!(result.output_tokens, 50);
}

#[tokio::test]
/// Verifies `provider_conversation_id` is forwarded from `TurnRequest` to
/// the underlying `AppServerTurnRequest` and propagated back from the
/// response into the returned `TurnResult`.
async fn test_run_turn_passes_and_returns_provider_conversation_id() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|request, _stream_tx| {
            assert_eq!(
                request.provider_conversation_id,
                Some("thread-abc".to_string()),
                "request should carry the provider conversation id"
            );
            assert_eq!(
                request.reasoning_level,
                ReasoningLevel::Medium,
                "request should carry the codex reasoning level"
            );

            Box::pin(async {
                Ok(AppServerTurnResponse {
                    assistant_message: r#"{"answer":"ok","questions":[]}"#.to_string(),
                    context_reset: false,
                    input_tokens: 1,
                    output_tokens: 1,
                    pid: Some(42),
                    provider_conversation_id: Some("thread-xyz".to_string()),
                })
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();
    let mut request = make_turn_request();
    request.reasoning_level = ReasoningLevel::Medium;
    request.continuation = crate::channel::TurnContinuation::provider(
        None,
        None,
        Some("thread-abc".to_string()),
        None,
    );

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), request, events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(
        result.provider_conversation_id,
        Some("thread-xyz".to_string()),
        "result should carry the provider conversation id from the response"
    );

    // Verify PID event was emitted from the response.
    let mut pid_event_seen = false;
    while let Ok(event) = events_rx.try_recv() {
        if matches!(event, TurnEvent::PidUpdate(Some(42))) {
            pid_event_seen = true;
        }
    }
    assert!(pid_event_seen, "should emit PidUpdate from response pid");
}
