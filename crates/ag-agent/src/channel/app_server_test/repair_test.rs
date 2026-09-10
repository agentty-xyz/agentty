use std::sync::Arc;

use tokio::sync::mpsc;

use super::support::{collect_pid_updates, make_ok_response, make_turn_request};
use crate::app_server::{AppServerStreamEvent, MockAppServerClient};
use crate::channel::app_server::AppServerAgentChannel;
use crate::channel::contract::{AgentChannel, AgentRequestKind, TurnEvent};
use crate::model::agent::{AgentKind, AgentModel};

#[tokio::test]
async fn repair_forwards_live_pid_and_publishes_retained_or_cleared_response_pid() {
    for final_pid in [Some(456), None] {
        // Arrange
        let initial_pid = final_pid.map(|_| 123);
        let mut client = MockAppServerClient::new();
        let mut sequence = mockall::Sequence::new();
        client
            .expect_run_turn()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_, _| {
                Box::pin(async move {
                    let mut response = make_ok_response("invalid original response");
                    response.pid = initial_pid;

                    Ok(response)
                })
            });
        let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
        let mut finish_rx = Some(finish_rx);
        client
            .expect_run_turn()
            .times(1)
            .in_sequence(&mut sequence)
            .returning(move |_, stream_tx| {
                let finish_rx = finish_rx.take().expect("single repair turn");
                Box::pin(async move {
                    let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(456)));
                    let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                        "private repair diagnostics".to_string(),
                    ));
                    finish_rx.await.expect("release repair");
                    let mut response = make_ok_response(r#"{"answer":"repaired","questions":[]}"#);
                    response.pid = final_pid;

                    Ok(response)
                })
            });
        let channel = AppServerAgentChannel::new(Arc::new(client), AgentKind::Codex);
        let (events_tx, mut events_rx) = mpsc::unbounded_channel();

        // Act
        let turn = tokio::spawn(async move {
            channel
                .run_turn("session".to_string(), make_turn_request(), events_tx)
                .await
        });
        let mut pids = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while let Some(event) = events_rx.recv().await {
                if let TurnEvent::PidUpdate(pid) = event {
                    pids.push(pid);
                    if pid == Some(456) {
                        break;
                    }
                }
            }
        })
        .await
        .expect("repair PID arrives while turn is running");

        // Assert
        assert_eq!(pids, vec![initial_pid, Some(456)]);
        assert!(!turn.is_finished());
        finish_tx.send(()).expect("finish repair");
        let result = turn
            .await
            .expect("join turn")
            .expect("repaired turn succeeds");
        assert_eq!(result.assistant_message.to_display_text(), "repaired");
        assert!(
            matches!(events_rx.recv().await, Some(TurnEvent::PidUpdate(pid)) if pid == final_pid)
        );
        assert!(events_rx.recv().await.is_none());
    }
}

#[tokio::test]
async fn repair_transport_failure_clears_latest_runtime_pid() {
    // Arrange
    let mut client = MockAppServerClient::new();
    let mut sequence = mockall::Sequence::new();
    client
        .expect_run_turn()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, _| {
            Box::pin(async {
                let mut response = make_ok_response("invalid original response");
                response.pid = Some(123);

                Ok(response)
            })
        });
    client
        .expect_run_turn()
        .times(1)
        .in_sequence(&mut sequence)
        .returning(|_, stream_tx| {
            Box::pin(async move {
                let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(456)));

                Err(crate::app_server::AppServerError::Provider(
                    "repair runtime failed".to_string(),
                ))
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let error = channel
        .run_turn("session".to_string(), make_turn_request(), events_tx)
        .await
        .expect_err("repair transport fails");

    // Assert
    assert!(
        error
            .to_string()
            .contains("protocol repair transport failed")
    );
    assert_eq!(
        collect_pid_updates(&mut events_rx),
        vec![Some(123), Some(456), None]
    );
}

#[tokio::test]
/// Verifies app-server turns surface invalid structured output after both
/// the original parse and the protocol-repair retry fail.
async fn test_run_turn_returns_error_for_invalid_structured_output() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .times(2)
        .returning(|request, stream_tx| {
            assert_eq!(request.request_kind, AgentRequestKind::SessionStart);
            let _ = stream_tx.send(AppServerStreamEvent::PidUpdate(Some(42)));

            Box::pin(async {
                let mut response = make_ok_response("plain non-json response");
                response.pid = Some(42);

                Ok(response)
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let error = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await
        .expect_err("invalid structured output should fail");

    // Assert
    let error_message = error.to_string();
    assert!(error_message.contains("did not match the required JSON schema"));
    assert!(!error_message.contains("plain non-json response"));
    assert_eq!(collect_pid_updates(&mut events_rx).last(), Some(&None));
}

#[tokio::test]
async fn repair_preserves_permissions_for_the_next_session_turn() {
    for (kind, model) in [
        (AgentKind::Codex, AgentModel::Gpt56Sol),
        (AgentKind::Gemini, AgentModel::Gemini31Pro),
        (AgentKind::Antigravity, AgentModel::Gemini31Pro),
    ] {
        for permission_mode in crate::model::permission::PermissionMode::ALL {
            // Arrange
            let captured = Arc::new(std::sync::Mutex::new(Vec::new()));
            let mut client = MockAppServerClient::new();
            client.expect_run_turn().times(3).returning({
                let captured = Arc::clone(&captured);
                move |request, _stream_tx| {
                    let mut requests = captured.lock().expect("requests");
                    let content = match requests.len() {
                        0 => "malformed response",
                        1 => r#"{"answer":"Repaired response","questions":[]}"#,
                        _ => r#"{"answer":"Continued work","questions":[]}"#,
                    };
                    requests.push(request);
                    let mut response = make_ok_response(content);
                    response.provider_conversation_id = Some("native-session".into());

                    Box::pin(async move { Ok(response) })
                }
            });
            let channel = AppServerAgentChannel::new(Arc::new(client), kind);
            let (events_tx, _events_rx) = mpsc::unbounded_channel();
            let mut request = make_turn_request();
            request.permission_mode = permission_mode;
            request.model = model.provider_model_str().into();

            // Act
            let repaired = channel
                .run_turn("session".into(), request.clone(), events_tx.clone())
                .await
                .expect("repair succeeds");
            request.request_kind = AgentRequestKind::SessionResume;
            request.continuation = crate::channel::TurnContinuation::provider(
                None,
                None,
                repaired.provider_conversation_id.clone(),
                Some("Earlier work and repaired response".into()),
            );
            let continued = channel
                .run_turn("session".into(), request, events_tx)
                .await
                .expect("next session turn succeeds");

            // Assert
            assert_eq!(
                repaired.assistant_message.to_display_text(),
                "Repaired response"
            );
            assert_eq!(
                continued.assistant_message.to_display_text(),
                "Continued work"
            );
            let requests = captured.lock().expect("requests");
            assert_eq!(requests.len(), 3);
            for request in requests.iter() {
                assert_eq!(request.permission_mode, permission_mode);
                assert_eq!(request.session_id, "session");
            }
            assert_eq!(
                requests[1].provider_conversation_id.as_deref(),
                Some("native-session")
            );
            assert_eq!(
                requests[2].provider_conversation_id.as_deref(),
                Some("native-session")
            );
            assert!(requests[1].prompt.contains("Do not redo the task"));
            assert_eq!(
                requests[2].replay_transcript.as_deref(),
                Some("Earlier work and repaired response")
            );
        }
    }
}

#[tokio::test]
/// Verifies Codex turns surface invalid plain-text output after both the
/// original parse and the protocol-repair retry fail.
async fn test_run_turn_codex_rejects_plain_text_after_repair_retry() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .times(2)
        .returning(|_request, _stream_tx| {
            Box::pin(async { Ok(make_ok_response("plain-text-payload")) })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, _events_rx) = mpsc::unbounded_channel();

    // Act
    let error = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await
        .expect_err("plain-text turn should fail");

    // Assert
    let error_message = error.to_string();
    assert!(error_message.contains("did not match the required JSON schema"));
    assert!(!error_message.contains("plain-text-payload"));
}
