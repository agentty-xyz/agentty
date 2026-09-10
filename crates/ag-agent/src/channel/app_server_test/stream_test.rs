use super::*;

#[tokio::test]
/// Verifies non-thought assistant deltas are withheld from the unified
/// event stream so transcript output is only appended from the final turn
/// result.
async fn test_run_turn_suppresses_non_thought_assistant_delta_streaming() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: "Hello world".to_string(),
                phase: None,
                is_delta: true,
            });

            Box::pin(async {
                Ok(make_ok_response(
                    r#"{"answer":"Hello world","questions":[]}"#,
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
    assert!(result.is_ok());
    let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
    assert_ne!(events, [] as [crate::channel::contract::TurnEvent; 0]);
    assert!(
        events
            .iter()
            .all(|event| matches!(event, TurnEvent::PidUpdate(_))),
        "only pid events should be emitted, got: {events:?}"
    );
}

#[tokio::test]
/// Verifies completed assistant chunks are also withheld from the unified
/// event stream so the transcript only changes when the turn completes.
async fn test_run_turn_suppresses_non_delta_assistant_messages() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: "Full paragraph   ".to_string(),
                phase: None,
                is_delta: false,
            });

            Box::pin(async {
                Ok(make_ok_response(
                    r#"{"answer":"Full paragraph","questions":[]}"#,
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
    assert!(result.is_ok());
    let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
    assert_ne!(events, [] as [crate::channel::contract::TurnEvent; 0]);
    assert!(
        events
            .iter()
            .all(|event| matches!(event, TurnEvent::PidUpdate(_))),
        "only pid events should be emitted, got: {events:?}"
    );
}

#[tokio::test]
/// Verifies structured assistant payload chunks are not emitted as live
/// transcript output.
async fn test_run_turn_suppresses_non_delta_structured_json_streaming() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}]}"#.to_string(),
                phase: None,
                is_delta: false,
            });

            Box::pin(async {
                Ok(make_ok_response(
                    r#"{"answer":"Done.","questions":[{"text":"Need clarification.","options":[]}]}"#,
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
    assert!(result.is_ok());
    let events = std::iter::from_fn(|| events_rx.try_recv().ok()).collect::<Vec<_>>();
    assert_ne!(events, [] as [crate::channel::contract::TurnEvent; 0]);
    assert!(
        events
            .iter()
            .all(|event| matches!(event, TurnEvent::PidUpdate(_))),
        "only pid events should be emitted, got: {events:?}"
    );
}

#[tokio::test]
/// Verifies Codex thought-phase deltas are routed to `ThoughtDelta`.
async fn test_run_turn_routes_codex_thinking_delta_to_thought_event() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: "Inspecting files".to_string(),
                phase: Some("thinking".to_string()),
                is_delta: true,
            });

            Box::pin(async { Ok(make_ok_response(r#"{"answer":"Done.","questions":[]}"#)) })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await;

    // Assert
    assert!(result.is_ok());
    let event = events_rx.try_recv().expect("should have received an event");
    assert_eq!(
        event,
        TurnEvent::ThoughtDelta("Inspecting files".to_string())
    );
}

#[tokio::test]
/// Verifies Codex thought-phase matching is case-insensitive for streamed
/// thought routing.
async fn test_run_turn_routes_uppercase_codex_thinking_delta_to_thought_event() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: "Inspecting files".to_string(),
                phase: Some("Thinking".to_string()),
                is_delta: true,
            });

            Box::pin(async { Ok(make_ok_response(r#"{"answer":"Done.","questions":[]}"#)) })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await;

    // Assert
    assert!(result.is_ok());
    let event = events_rx.try_recv().expect("should have received an event");
    assert_eq!(
        event,
        TurnEvent::ThoughtDelta("Inspecting files".to_string())
    );
}

#[tokio::test]
/// Verifies nonempty `ProgressUpdate` events drive the transient loader
/// while blank updates leave it unchanged.
async fn test_run_turn_routes_progress_update_events_to_thought_delta() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(" \n ".to_string()));
            let _ = stream_tx.send(AppServerStreamEvent::ProgressUpdate(
                "Running tool".to_string(),
            ));

            Box::pin(async { Ok(make_ok_response(r#"{"answer":"","questions":[]}"#)) })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await;

    // Assert
    assert!(result.is_ok());
    let event = events_rx
        .try_recv()
        .expect("should have received a progress event");
    assert_eq!(event, TurnEvent::ThoughtDelta("Running tool".to_string()));
}

#[tokio::test]
/// Verifies whitespace-only `AssistantMessage` does not emit a thinking
/// update.
async fn test_run_turn_skips_whitespace_only_assistant_messages() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: "   \n  ".to_string(),
                phase: None,
                is_delta: true,
            });

            Box::pin(async { Ok(make_ok_response(r#"{"answer":"","questions":[]}"#)) })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await;

    // Assert
    assert!(result.is_ok());
    while let Ok(event) = events_rx.try_recv() {
        assert!(
            !matches!(event, TurnEvent::ThoughtDelta(_)),
            "no ThoughtDelta should be emitted for whitespace-only messages, got: {event:?}"
        );
    }
}

#[tokio::test]
/// Verifies delta protocol JSON fragments do not emit transient loader
/// updates.
async fn test_run_turn_skips_delta_protocol_json_fragments() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: r#"{"answer":"#.to_string(),
                phase: None,
                is_delta: true,
            });

            Box::pin(async {
                Ok(make_ok_response(
                    r#"{"answer":"Final answer.","questions":[]}"#,
                ))
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(result.assistant_message.to_display_text(), "Final answer.");
    while let Ok(event) = events_rx.try_recv() {
        assert!(
            !matches!(event, TurnEvent::ThoughtDelta(_)),
            "no ThoughtDelta should be emitted for protocol fragments, got: {event:?}"
        );
    }
}

#[tokio::test]
/// Verifies app-server providers suppress streamed assistant chunks and
/// rely on the final parsed payload.
async fn test_run_turn_app_server_suppresses_streamed_assistant_messages() {
    // Arrange
    let mut mock_client = MockAppServerClient::new();
    mock_client
        .expect_run_turn()
        .returning(|_request, stream_tx| {
            let _ = stream_tx.send(AppServerStreamEvent::AssistantMessage {
                message: "streamed plain text".to_string(),
                phase: None,
                is_delta: true,
            });

            Box::pin(async {
                Ok(make_ok_response(
                    r#"{"answer":"Final structured output.","questions":[]}"#,
                ))
            })
        });
    let channel = AppServerAgentChannel::new(Arc::new(mock_client), AgentKind::Codex);
    let (events_tx, mut events_rx) = mpsc::unbounded_channel();

    // Act
    let result = channel
        .run_turn("sess-1".to_string(), make_turn_request(), events_tx)
        .await
        .expect("turn should succeed");

    // Assert
    assert_eq!(
        result.assistant_message.to_display_text(),
        "Final structured output."
    );
    while let Ok(event) = events_rx.try_recv() {
        assert!(
            !matches!(event, TurnEvent::ThoughtDelta(_)),
            "no ThoughtDelta should be emitted for plain assistant deltas, got: {event:?}"
        );
    }
}
