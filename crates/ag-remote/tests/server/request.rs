//! Request round trips and change pushes on authenticated connections.

use std::time::Duration;

use ag_remote::protocol::{
    ClientFrame, RemoteMessage, RemoteMessageKind, RemoteQuestion, RemoteQuestionAnswer,
    RemoteRequest, RemoteResponse, RemoteResult, RemoteSession, RemoteSessionMode,
    RemoteSessionSummary, ServerFrame,
};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use crate::support::{
    Client, TestResult, TestServer, assert_rejected, hello_frame, next_frame, send,
};

/// Sends one request and returns its result.
async fn call(client: &mut Client, id: u64, request: RemoteRequest) -> TestResult<RemoteResult> {
    send(client, &ClientFrame::Request { id, request }).await?;
    match next_frame(client).await? {
        ServerFrame::Response {
            id: response_id,
            result,
        } if response_id == id => Ok(result),
        frame => Err(format!("expected response {id}, got {frame:?}").into()),
    }
}

/// Decodes the next text message as a server frame without a client-side
/// timeout, so a paused clock can advance to server deadlines.
async fn next_untimed_frame(client: &mut Client) -> TestResult<ServerFrame> {
    let Some(Ok(Message::Text(text))) = client.next().await else {
        return Err("expected a text frame".into());
    };

    Ok(serde_json::from_str(&text)?)
}

/// Builds the listing entry of the session `id`.
fn summary(id: &str, status: &str, title: &str, updated_at: i64) -> RemoteSessionSummary {
    RemoteSessionSummary {
        id: id.to_string(),
        status: status.to_string(),
        title: Some(title.to_string()),
        updated_at,
        ..RemoteSessionSummary::default()
    }
}

/// Returns the snapshot of the test backend's `session-1`, with its
/// settings, branches, and chat-visible transcript.
fn expected_session() -> RemoteSession {
    RemoteSession {
        agent: Some("codex".to_string()),
        base_branch: Some("main".to_string()),
        branch: Some("wt/session-".to_string()),
        created_at: Some(10),
        id: "session-1".to_string(),
        is_draft: false,
        messages: vec![
            RemoteMessage {
                content: "build it".to_string(),
                kind: RemoteMessageKind::User,
            },
            RemoteMessage {
                content: "which branch?".to_string(),
                kind: RemoteMessageKind::Assistant,
            },
        ],
        model: Some("gpt-6.1-sol".to_string()),
        published_upstream_ref: None,
        questions: vec![RemoteQuestion {
            options: vec!["main".to_string()],
            text: "Which branch?".to_string(),
        }],
        queued_messages: Vec::new(),
        reasoning: Some("high".to_string()),
        response_style: Some("balanced".to_string()),
        review_request: None,
        speed_mode: Some("normal".to_string()),
        status: "Question".to_string(),
        title: Some("Build it".to_string()),
        updated_at: 20,
    }
}

#[tokio::test]
async fn each_request_reaches_the_session_service() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let listed = call(&mut client, 1, RemoteRequest::ListSessions).await?;
    let loaded = call(
        &mut client,
        2,
        RemoteRequest::GetSession {
            session_id: "session-1".to_string(),
        },
    )
    .await?;
    let unlisted = call(
        &mut client,
        3,
        RemoteRequest::GetSession {
            session_id: "other-project-session".to_string(),
        },
    )
    .await?;
    let sent = call(
        &mut client,
        4,
        RemoteRequest::SendMessage {
            message: "continue".to_string(),
            session_id: "session-1".to_string(),
        },
    )
    .await?;
    let answered = call(
        &mut client,
        5,
        RemoteRequest::AnswerQuestions {
            answers: vec![RemoteQuestionAnswer {
                answer: "main".to_string(),
                question: "Which branch?".to_string(),
            }],
            session_id: "session-1".to_string(),
        },
    )
    .await?;

    // Assert
    assert_eq!(
        listed,
        RemoteResult::Ok(RemoteResponse::Sessions {
            sessions: vec![
                summary("session-1", "Review", "Build it", 20),
                summary("session-2", "InProgress", "Plan it", 30),
            ],
        })
    );
    assert_eq!(
        loaded,
        RemoteResult::Ok(RemoteResponse::Session {
            session: Some(Box::new(expected_session())),
        })
    );
    assert_eq!(
        unlisted,
        RemoteResult::Ok(RemoteResponse::Session { session: None })
    );
    assert_eq!(sent, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(answered, RemoteResult::Ok(RemoteResponse::Accepted));
    assert_eq!(
        server.backend.calls(),
        [
            "list",
            "list",
            "get:session-1",
            "list",
            "list",
            "send:session-1:continue",
            "list",
            "answer:session-1:Which branch?=main"
        ]
    );

    Ok(())
}

#[tokio::test]
async fn host_errors_return_in_the_response_and_keep_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let busy = call(
        &mut client,
        1,
        RemoteRequest::SendMessage {
            message: "busy".to_string(),
            session_id: "session-1".to_string(),
        },
    )
    .await?;
    let canceled = call(
        &mut client,
        2,
        RemoteRequest::CancelSession {
            session_id: "session-1".to_string(),
        },
    )
    .await?;
    let listed = call(&mut client, 3, RemoteRequest::ListSessions).await?;

    // Assert
    assert_eq!(busy, RemoteResult::Error("Session is busy".to_string()));
    assert_eq!(
        canceled,
        RemoteResult::Error("Session not found".to_string())
    );
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

/// Sends request `id`: a message the backend holds until it is released.
async fn send_held_message(client: &mut Client, id: u64) -> TestResult {
    let request = RemoteRequest::SendMessage {
        message: "wait".to_string(),
        session_id: "session-1".to_string(),
    };

    send(client, &ClientFrame::Request { id, request }).await
}

#[tokio::test]
async fn a_slow_request_delays_neither_other_requests_nor_change_pushes() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    send_held_message(&mut client, 1).await?;
    let listed = call(&mut client, 2, RemoteRequest::ListSessions).await?;
    server.revisions_tx.send_replace(5);
    let changed = next_frame(&mut client).await?;
    server.backend.release();
    let held = next_frame(&mut client).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(changed, ServerFrame::Changed { revision: 5 });
    assert_eq!(
        held,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Ok(RemoteResponse::Accepted),
        }
    );

    Ok(())
}

#[tokio::test]
async fn a_request_beyond_eight_in_progress_is_refused() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    for id in 1..=8 {
        send_held_message(&mut client, id).await?;
    }

    // Act
    let refused = call(&mut client, 9, RemoteRequest::ListSessions).await?;
    server.backend.release();
    let mut held_ids = Vec::new();
    for _ in 1..=8 {
        match next_frame(&mut client).await? {
            ServerFrame::Response {
                id,
                result: RemoteResult::Ok(RemoteResponse::Accepted),
            } => held_ids.push(id),
            frame => return Err(format!("expected an accepted response, got {frame:?}").into()),
        }
    }
    held_ids.sort_unstable();
    let listed = call(&mut client, 10, RemoteRequest::ListSessions).await?;

    // Assert
    assert_eq!(
        refused,
        RemoteResult::Error("Too many requests in progress".to_string())
    );
    assert_eq!(held_ids, (1..=8).collect::<Vec<u64>>());
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

/// Takes the next `count` frames as responses and returns them sorted by
/// request id.
async fn responses(client: &mut Client, count: usize) -> TestResult<Vec<(u64, RemoteResult)>> {
    let mut responses = Vec::new();
    for _ in 0..count {
        match next_frame(client).await? {
            ServerFrame::Response { id, result } => responses.push((id, result)),
            frame => return Err(format!("expected a response, got {frame:?}").into()),
        }
    }
    responses.sort_unstable_by_key(|(id, _)| *id);

    Ok(responses)
}

#[tokio::test]
async fn mutations_of_one_session_reach_the_host_in_arrival_order() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    send_held_message(&mut client, 1).await?;
    let cancel = RemoteRequest::CancelSession {
        session_id: "session-1".to_string(),
    };
    send(
        &mut client,
        &ClientFrame::Request {
            id: 2,
            request: cancel,
        },
    )
    .await?;
    let listed = call(&mut client, 3, RemoteRequest::ListSessions).await?;
    let mutations_while_held = host_mutations(&server);
    server.backend.release();
    let replies = responses(&mut client, 2).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(mutations_while_held, Vec::<String>::new());
    assert_eq!(
        replies,
        [
            (1, RemoteResult::Ok(RemoteResponse::Accepted)),
            (2, RemoteResult::Error("Session not found".to_string())),
        ]
    );
    assert_eq!(
        host_mutations(&server),
        ["send:session-1:wait", "cancel:session-1"]
    );

    Ok(())
}

/// Returns the recorded backend calls other than the session listings that
/// run concurrently with them.
fn host_mutations(server: &TestServer) -> Vec<String> {
    server
        .backend
        .calls()
        .into_iter()
        .filter(|call| call != "list")
        .collect()
}

#[tokio::test]
async fn a_request_that_panics_gets_an_error_and_releases_its_session() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let message = |id, message: &str| ClientFrame::Request {
        id,
        request: RemoteRequest::SendMessage {
            message: message.to_string(),
            session_id: "session-1".to_string(),
        },
    };

    // Act
    send(&mut client, &message(1, "panic")).await?;
    send(&mut client, &message(2, "next")).await?;
    let replies = responses(&mut client, 2).await?;

    // Assert
    assert_eq!(
        replies,
        [
            (1, RemoteResult::Error("Request failed".to_string())),
            (2, RemoteResult::Ok(RemoteResponse::Accepted)),
        ]
    );

    Ok(())
}

#[tokio::test]
async fn shutdown_waits_for_requests_in_progress() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    send_held_message(&mut client, 1).await?;
    // A reply to a later request shows the held one has started.
    call(&mut client, 2, RemoteRequest::ListSessions).await?;
    let TestServer {
        backend,
        devices: _devices,
        handle,
        revisions_tx: _revisions_tx,
    } = server;

    // Act
    let mut shutdown = tokio::spawn(handle.shutdown());
    let finished_while_held = tokio::time::timeout(Duration::from_millis(300), &mut shutdown)
        .await
        .is_ok();
    backend.release();
    shutdown.await?;

    // Assert
    assert!(!finished_while_held);
    assert_eq!(backend.calls(), ["list", "list", "send:session-1:wait"]);

    Ok(())
}

#[tokio::test]
async fn mutations_of_unlisted_sessions_never_reach_the_host() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let session_id = "other-project-session".to_string();

    // Act
    let sent = call(
        &mut client,
        1,
        RemoteRequest::SendMessage {
            message: "continue".to_string(),
            session_id: session_id.clone(),
        },
    )
    .await?;
    let answered = call(
        &mut client,
        2,
        RemoteRequest::AnswerQuestions {
            answers: Vec::new(),
            session_id: session_id.clone(),
        },
    )
    .await?;
    let canceled = call(&mut client, 3, RemoteRequest::CancelSession { session_id }).await?;

    // Assert
    let not_found = RemoteResult::Error("Session not found".to_string());
    assert_eq!(sent, not_found);
    assert_eq!(answered, not_found);
    assert_eq!(canceled, not_found);
    assert_eq!(server.backend.calls(), ["list", "list", "list"]);

    Ok(())
}

#[tokio::test]
async fn merge_reaches_the_host_only_for_listed_sessions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let refused = call(
        &mut client,
        1,
        RemoteRequest::MergeSession {
            session_id: "session-1".to_string(),
        },
    )
    .await?;
    let unlisted = call(
        &mut client,
        2,
        RemoteRequest::MergeSession {
            session_id: "other-project-session".to_string(),
        },
    )
    .await?;

    // Assert
    assert_eq!(
        refused,
        RemoteResult::Error("Session is not ready to merge".to_string())
    );
    assert_eq!(
        unlisted,
        RemoteResult::Error("Session not found".to_string())
    );
    assert_eq!(server.backend.calls(), ["list", "merge:session-1", "list"]);

    Ok(())
}

#[tokio::test]
async fn new_sessions_go_to_the_active_project_in_a_served_mode() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    let host = call(&mut client, 1, RemoteRequest::GetHost).await?;
    let created = call(
        &mut client,
        2,
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Regular,
        },
    )
    .await?;
    let harness = call(
        &mut client,
        3,
        RemoteRequest::CreateSession {
            mode: RemoteSessionMode::Harness,
        },
    )
    .await?;

    // Assert
    assert_eq!(
        host,
        RemoteResult::Ok(RemoteResponse::Host {
            base_branch: "main".to_string(),
            default_model: "gpt-6.1-sol".to_string(),
            project: "agentty".to_string(),
            session_modes: vec![RemoteSessionMode::Regular],
        })
    );
    assert_eq!(
        created,
        RemoteResult::Ok(RemoteResponse::Created {
            session_id: "session-2".to_string(),
        })
    );
    assert_eq!(
        harness,
        RemoteResult::Error("Session mode is not available".to_string())
    );
    assert_eq!(
        server.backend.calls(),
        ["host", "host", "create:Regular:1", "host"]
    );

    Ok(())
}

#[tokio::test]
async fn pings_are_ignored_between_requests() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;

    // Act
    client.send(Message::Ping(vec![1].into())).await?;
    client.send(Message::Pong(vec![2].into())).await?;
    let listed = call(&mut client, 1, RemoteRequest::ListSessions).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}

#[tokio::test]
async fn protocol_violations_after_authentication_close_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut repeated_hello, _device_id, token) = server.pair("Phone").await?;
    let (mut invalid, _welcome) = server.hello(&token).await?;
    let (mut binary, _welcome) = server.hello(&token).await?;

    // Act
    send(&mut repeated_hello, &hello_frame(&token)).await?;
    invalid.send(Message::Text("[]".into())).await?;
    binary.send(Message::Binary(vec![0].into())).await?;

    // Assert
    assert_rejected(&mut repeated_hello, "Connection is already authenticated").await?;
    assert_rejected(&mut invalid, "Invalid frame").await?;
    assert_rejected(&mut binary, "Binary frames are not supported").await
}

#[tokio::test]
async fn unknown_methods_get_an_error_and_keep_the_connection() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let unknown =
        r#"{"type":"request","id":1,"request":{"method":"stop_turn","session_id":"session-1"}}"#;

    // Act
    client.send(Message::Text(unknown.into())).await?;
    let refused = next_frame(&mut client).await?;
    let listed = call(&mut client, 2, RemoteRequest::ListSessions).await?;

    // Assert
    assert_eq!(
        refused,
        ServerFrame::Response {
            id: 1,
            result: RemoteResult::Error("Unsupported request".to_string()),
        }
    );
    assert!(matches!(listed, RemoteResult::Ok(_)));
    assert_eq!(server.backend.calls(), ["list"]);

    Ok(())
}

#[tokio::test]
async fn bursts_of_host_changes_push_one_changed_frame() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    // Pausing only after pairing keeps the clock from skipping past the
    // pairing round trips while loopback I/O is in flight.
    tokio::time::pause();
    let started = tokio::time::Instant::now();

    // Act
    server.revisions_tx.send_replace(2);
    server.revisions_tx.send_replace(3);
    let first = next_untimed_frame(&mut client).await?;
    let first_elapsed = started.elapsed();
    server.revisions_tx.send_replace(4);
    let second = next_untimed_frame(&mut client).await?;

    // Assert
    assert_eq!(first, ServerFrame::Changed { revision: 3 });
    assert!(first_elapsed >= Duration::from_millis(250));
    assert_eq!(second, ServerFrame::Changed { revision: 4 });

    Ok(())
}

#[tokio::test]
async fn requests_keep_working_after_the_host_drops_revisions() -> TestResult {
    // Arrange
    let server = TestServer::start().await?;
    let (mut client, _device_id, _token) = server.pair("Phone").await?;
    let TestServer {
        backend: _backend,
        devices: _devices,
        handle: _handle,
        revisions_tx,
    } = server;

    // Act
    drop(revisions_tx);
    let listed = call(&mut client, 1, RemoteRequest::ListSessions).await?;

    // Assert
    assert!(matches!(listed, RemoteResult::Ok(_)));

    Ok(())
}
