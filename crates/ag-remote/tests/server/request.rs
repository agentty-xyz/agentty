//! Request round trips and change pushes on authenticated connections.

use std::time::Duration;

use ag_remote::protocol::{
    ClientFrame, RemoteMessage, RemoteMessageKind, RemoteQuestion, RemoteQuestionAnswer,
    RemoteRequest, RemoteResponse, RemoteResult, RemoteSession, RemoteSessionSummary, ServerFrame,
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
            sessions: vec![RemoteSessionSummary {
                id: "session-1".to_string(),
                status: "Review".to_string(),
                title: Some("Build it".to_string()),
                updated_at: 20,
            }],
        })
    );
    assert_eq!(
        loaded,
        RemoteResult::Ok(RemoteResponse::Session {
            session: Some(RemoteSession {
                id: "session-1".to_string(),
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
                questions: vec![RemoteQuestion {
                    options: vec!["main".to_string()],
                    text: "Which branch?".to_string(),
                }],
                queued_messages: Vec::new(),
                status: "Question".to_string(),
                title: Some("Build it".to_string()),
                updated_at: 20,
            }),
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
